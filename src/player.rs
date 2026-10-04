//! The Spotify Connect player: connecting, and the playback controls that
//! key handlers invoke.

use crate::paths;
use crate::state::{AppState, RepeatMode, Screen, ShuffleMode};
use librespot_connect::{ConnectConfig, LoadContextOptions, LoadRequest, LoadRequestOptions, Options, PlayingTrack, Spirc};
use librespot_core::cache::Cache;
use librespot_core::config::{DeviceType, SessionConfig};
use librespot_core::session::Session;
use librespot_playback::audio_backend;
use librespot_playback::config::{AudioFormat, PlayerConfig};
use librespot_playback::mixer::{self, MixerConfig};
use librespot_playback::player::{Player, PlayerEventChannel};
use tokio::task::JoinHandle;

pub const SEEK_STEP_MS: i64 = 5000;

/// The volume a fresh player starts at (see `connect`).
pub const INITIAL_VOLUME: u16 = u16::MAX;

/// A live Connect session. `task` finishing means the session dropped.
pub struct Connection {
    pub spirc: Spirc,
    pub task: JoinHandle<()>,
    pub events: PlayerEventChannel,
    pub session: Session,
}

/// Why a connect attempt failed. `NoCredentials` needs the user to act; every
/// other failure is treated as transient and retried.
#[derive(Debug, PartialEq)]
pub enum ConnectError {
    NoCredentials,
    Other(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCredentials => f.write_str("no cached credentials found in ncspot's cache"),
            Self::Other(e) => f.write_str(e),
        }
    }
}

impl From<String> for ConnectError {
    fn from(e: String) -> Self {
        Self::Other(e)
    }
}

impl From<&str> for ConnectError {
    fn from(e: &str) -> Self {
        Self::Other(e.to_string())
    }
}

/// Full librespot bootstrap. Returns `Err` instead of panicking so a failed
/// attempt can back off and retry rather than crash.
pub async fn connect() -> Result<Connection, ConnectError> {
    // Login credentials come from ncspot's cache. Volume and audio cache get
    // a directory of their own: librespot writes a file literally named
    // `volume` there, which collides with ncspot's `volume/` directory.
    let own_cache = paths::cache_dir().join("librespot");
    let cache = Cache::new(Some(&paths::ncspot_librespot_cache()), Some(&own_cache), Some(&own_cache), None)
        .map_err(|e| e.to_string())?;
    let credentials = cache.credentials().ok_or(ConnectError::NoCredentials)?;

    let session = Session::new(SessionConfig::default(), Some(cache));
    let mixer_fn = mixer::find(None).ok_or("no default mixer available")?;
    let mixer = mixer_fn(MixerConfig::default()).map_err(|e| e.to_string())?;
    let backend = audio_backend::find(None).ok_or("no default audio backend")?;
    let soft_volume = mixer.get_soft_volume();
    let player =
        Player::new(PlayerConfig::default(), session.clone(), soft_volume, move || backend(None, AudioFormat::default()));
    let events = player.get_player_event_channel();

    let config = ConnectConfig {
        name: "spot-tui".to_string(),
        device_type: DeviceType::Computer,
        // The default of 50% feeds the mixer's soft-volume attenuation, a
        // real loudness drop on every launch. Start at max; loudness is
        // controlled with the system volume.
        initial_volume: INITIAL_VOLUME,
        ..ConnectConfig::default()
    };
    // `Spirc::new` consumes the session; keep a handle for lyrics lookups.
    let lyrics_session = session.clone();
    let (spirc, spirc_task) =
        Spirc::new(config, session, credentials, player, mixer).await.map_err(|e| e.to_string())?;

    Ok(Connection { spirc, task: tokio::spawn(spirc_task), events, session: lyrics_session })
}

/// Clamped at both ends: a negative position is nonsensical, and librespot
/// silently ignores a target past the track's end instead of clamping it, so
/// an unclamped target would make every later seek press a no-op.
pub fn seek_target_ms(current_ms: i64, delta_ms: i64, duration_ms: i64) -> u32 {
    (current_ms + delta_ms).clamp(0, duration_ms.max(0)) as u32
}

/// Mute toggle. `remembered` holds the volume to restore and is
/// deliberately not cleared on track change: mute is a device state.
pub fn mute_toggle(current: u16, remembered: &mut Option<u16>) -> u16 {
    match remembered.take() {
        Some(restored) => restored,
        None if current > 0 => {
            *remembered = Some(current);
            0
        }
        // Already silent with nothing remembered: nothing to undo.
        None => 0,
    }
}

/// librespot resets shuffle and repeat on every `load` unless the request
/// carries explicit options, and emits no event when it does. Every load
/// therefore hands the current values back in, as Spotify's own clients do.
pub fn carry_modes(shuffle: bool, repeat: RepeatMode, mut opts: LoadRequestOptions) -> LoadRequestOptions {
    let (repeat_context, repeat_track) = repeat.flags();
    opts.context_options =
        Some(LoadContextOptions::Options(Options { shuffle, repeat: repeat_context, repeat_track }));
    opts
}

/// Starts playback of `context_uri` (a track, album or playlist), optionally
/// at `start_index` within it, and shows Now Playing. `label` names where it
/// was started from; it is display-only and survives `n`/`p` skips.
pub fn play_context(app: &mut AppState, spirc: &Spirc, context_uri: String, start_index: Option<u32>, label: String) {
    let opts = LoadRequestOptions { playing_track: start_index.map(PlayingTrack::Index), ..Default::default() };
    let opts = carry_modes(app.shuffle, app.repeat, opts);
    // `activate` must precede `load`: Spirc ignores Load while inactive.
    let _ = spirc.activate();
    let _ = spirc.load(LoadRequest::from_context_uri(context_uri, opts));
    let _ = spirc.play();
    app.context_label = Some(label);
    app.nav.goto(Screen::NowPlaying);
}

/// `s`: off -> on -> smart -> off.
///
/// Only `shuffle` is set optimistically. The smart flag is read back from
/// librespot's own state, so it can't drift. (librespot emits its shuffle
/// event with the *requested* value before validating it, so a context that
/// forbids shuffling still reads as on here; there is no way to detect that.)
pub fn cycle_shuffle(app: &mut AppState, spirc: &Spirc) {
    let next = ShuffleMode::from_flags(app.shuffle, app.smart_shuffle).next();
    let sent = match next {
        ShuffleMode::Off => spirc.shuffle(false),
        ShuffleMode::On => spirc.shuffle(true),
        ShuffleMode::Smart => spirc.smart_shuffle(),
    };
    match sent {
        Ok(()) => {
            app.shuffle = next.shuffle();
            app.status = Some((next.status_label().to_string(), false));
        }
        Err(e) => app.status = Some((format!("couldn't change shuffle: {e}"), true)),
    }
}

/// `r`: repeat off -> album/playlist -> this song -> off. The player keeps
/// repeat as two independent flags, so each step sets both.
pub fn cycle_repeat(app: &mut AppState, spirc: &Spirc) {
    let next = app.repeat.next();
    let (context, track) = next.flags();
    match spirc.repeat(context).and_then(|()| spirc.repeat_track(track)) {
        Ok(()) => {
            app.repeat = next;
            app.status = Some((format!("repeat {}", next.status_label()), false));
        }
        Err(e) => app.status = Some((format!("couldn't change repeat: {e}"), true)),
    }
}

#[cfg(test)]
mod seek_target_tests {
    use super::*;

    #[test]
    fn ordinary_forward_seek_within_bounds_is_unchanged() {
        assert_eq!(seek_target_ms(10_000, 5_000, 300_000), 15_000);
    }

    #[test]
    fn ordinary_backward_seek_within_bounds_is_unchanged() {
        assert_eq!(seek_target_ms(10_000, -5_000, 300_000), 5_000);
    }

    #[test]
    fn backward_seek_past_the_start_clamps_to_zero() {
        assert_eq!(seek_target_ms(3_000, -5_000, 300_000), 0);
    }

    #[test]
    fn forward_seek_past_the_end_clamps_to_duration_not_left_unbounded() {
        // The exact regression: holding Right for a few repeats pushes
        // the naive target (297_000 + 5_000 = 302_000) past a 300_000ms
        // track -- librespot would silently ignore that, not clamp it.
        assert_eq!(seek_target_ms(297_000, 5_000, 300_000), 300_000);
    }
}


#[cfg(test)]
mod mute_toggle_tests {
    use super::*;

    #[test]
    fn muting_remembers_the_volume_and_goes_silent() {
        let mut remembered = None;
        assert_eq!(mute_toggle(40_000, &mut remembered), 0);
        assert_eq!(remembered, Some(40_000));
    }

    #[test]
    fn unmuting_restores_exactly_the_remembered_value_and_forgets_it() {
        let mut remembered = Some(40_000);
        assert_eq!(mute_toggle(0, &mut remembered), 40_000);
        assert_eq!(remembered, None);
    }

    #[test]
    fn muting_at_zero_with_nothing_remembered_is_a_no_op() {
        let mut remembered = None;
        assert_eq!(mute_toggle(0, &mut remembered), 0);
        assert_eq!(remembered, None);
    }

    #[test]
    fn muting_twice_in_a_row_keeps_the_first_remembered_value() {
        // A second `m` press before unmuting must not overwrite the
        // remembered level with the current (already-zero) volume.
        let mut remembered = None;
        assert_eq!(mute_toggle(50_000, &mut remembered), 0);
        assert_eq!(mute_toggle(0, &mut remembered), 50_000);
    }

    #[test]
    fn a_full_round_trip_returns_to_the_exact_starting_volume() {
        let mut remembered = None;
        let original = 12_345;
        let muted = mute_toggle(original, &mut remembered);
        assert_eq!(muted, 0);
        let restored = mute_toggle(muted, &mut remembered);
        assert_eq!(restored, original);
    }

    #[test]
    fn max_volume_round_trips_cleanly() {
        let mut remembered = None;
        assert_eq!(mute_toggle(u16::MAX, &mut remembered), 0);
        assert_eq!(mute_toggle(0, &mut remembered), u16::MAX);
    }
}


/// Guards the vendored librespot patch (see `[patch.crates-io]` in
/// Cargo.toml): upstream's `SetOptionsCommand` drops the `modes` map, which
/// is how the official app sets smart shuffle. If a librespot upgrade swaps
/// the vendored crates back for stock ones, this stops compiling or fails.
#[cfg(test)]
mod smart_shuffle_patch_tests {
    use librespot_core::dealer::protocol::{Command, Request};

    /// Shape captured live from the phone's `set_options` command (ids
    /// shortened): smart shuffle on = shuffle on + `context_enhancement`
    /// set to `RECOMMENDATION`.
    fn set_options_json(modes: &str) -> String {
        format!(
            r#"{{"message_id":1104469769,"sent_by_device_id":"7151e96b","target_alias_id":null,
            "command":{{"endpoint":"set_options","modes":{modes},"shuffling_context":true,
            "options":{{"only_for_local_device":false,"override_restrictions":false,"system_initiated":false}},
            "logging_params":{{"command_id":"abc","device_identifier":"7151e96b",
            "command_initiated_time":1789958347649,"command_received_time":1789958347649,
            "interaction_ids":["x"],"page_instance_ids":["y"]}}}}}}"#
        )
    }

    fn parse(json: &str) -> librespot_core::dealer::protocol::SetOptionsCommand {
        match serde_json::from_str::<Request>(json).expect("valid request").command {
            Command::SetOptions(o) => o,
            other => panic!("expected set_options, got {other:?}"),
        }
    }

    #[test]
    fn smart_shuffle_mode_survives_deserialization() {
        let cmd = parse(&set_options_json(r#"{"context_enhancement":"RECOMMENDATION"}"#));
        assert_eq!(cmd.shuffling_context, Some(true));
        let modes = cmd.modes.expect("modes must not be dropped");
        assert_eq!(modes.get("context_enhancement").map(String::as_str), Some("RECOMMENDATION"));
    }

    #[test]
    fn plain_shuffle_mode_is_none_not_recommendation() {
        let cmd = parse(&set_options_json(r#"{"context_enhancement":"NONE"}"#));
        let modes = cmd.modes.expect("modes must not be dropped");
        assert_eq!(modes.get("context_enhancement").map(String::as_str), Some("NONE"));
    }

    #[test]
    fn a_command_without_modes_still_parses() {
        let json = set_options_json("null");
        let cmd = parse(&json);
        assert!(cmd.modes.is_none());
        assert_eq!(cmd.shuffling_context, Some(true));
    }
}


#[cfg(test)]
mod carry_modes_tests {
    use super::*;
    use librespot_connect::LoadContextOptions;

    fn carried(shuffle: bool, repeat: RepeatMode) -> (bool, bool, bool) {
        match carry_modes(shuffle, repeat, LoadRequestOptions::default()).context_options {
            Some(LoadContextOptions::Options(o)) => (o.shuffle, o.repeat, o.repeat_track),
            other => panic!("expected explicit options, got {other:?}"),
        }
    }

    #[test]
    fn a_load_keeps_shuffle_on_instead_of_silently_resetting_it() {
        // librespot resets shuffle/repeat on every load unless told otherwise,
        // which left the playbar lit while Spotify showed shuffle off.
        assert_eq!(carried(true, RepeatMode::Off), (true, false, false));
    }

    #[test]
    fn repeat_album_and_repeat_song_map_to_the_two_player_flags() {
        assert_eq!(carried(false, RepeatMode::Context), (false, true, false));
        assert_eq!(carried(false, RepeatMode::Track), (false, true, true));
    }

    #[test]
    fn everything_off_is_still_sent_explicitly() {
        assert_eq!(carried(false, RepeatMode::Off), (false, false, false));
    }

    #[test]
    fn the_rest_of_the_load_request_is_untouched() {
        let opts = LoadRequestOptions { start_playing: true, seek_to: 42, ..Default::default() };
        let out = carry_modes(true, RepeatMode::Off, opts);
        assert!(out.start_playing);
        assert_eq!(out.seek_to, 42);
    }
}

