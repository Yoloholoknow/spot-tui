mod config;
mod lyrics;
mod pins;
mod position;
mod api;
mod spike;
mod ui;
mod spicy;
mod ytmusic;

use crossterm::event::{self, DisableFocusChange, EnableFocusChange, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, ExecutableCommand};
use librespot_connect::{ConnectConfig, LoadRequest, LoadRequestOptions, PlayingTrack, Spirc};
use librespot_core::cache::Cache;
use librespot_core::config::{DeviceType, SessionConfig};
use librespot_core::session::Session;
use librespot_metadata::audio::UniqueFields;
use librespot_playback::audio_backend;
use librespot_playback::config::{AudioFormat, PlayerConfig};
use librespot_playback::mixer::{self, MixerConfig};
use librespot_playback::player::{Player, PlayerEvent};
use lyrics::{
    cached_synced, current_line_index, spicy_cache_key, spotify_lyrics, store_synced, CachedLyrics, LyricLine,
    LyricsClient,
};
use position::PositionTracker;
use std::io::stdout;
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use rspotify::AuthCodeSpotify;
use api::library::{FollowedArtist, PlaylistSummary, SavedAlbumSummary};
use api::search::TrackResult;
use ui::{
    filtered_sorted, pinned_first, quick_jump_entries, AlbumDetailState, AppState, ArtistDetailState, ConfirmAction,
    Fetch, Focus, LibraryState, ListFilter, LyricsState, Nav, PendingConfirm, PlaylistDetailState, PlaylistPicker,
    QuickJump, QuickJumpEntry, QuickJumpKind, RepeatMode, Screen, SearchState, ShuffleMode, TextPrompt,
    TextPromptAction, LIBRARY_ENTRIES,
};

enum LibraryFetchResult {
    LikedSongs(Result<Vec<TrackResult>, String>),
    SavedAlbums(Result<Vec<SavedAlbumSummary>, String>),
    FollowedArtists(Result<Vec<FollowedArtist>, String>),
    Playlists(Result<Vec<PlaylistSummary>, String>),
    PlaylistTracks {
        playlist_uri: String,
        result: Result<Vec<TrackResult>, String>,
    },
    Queue(Result<api::queue::QueueSummary, String>),
    Devices(Result<Vec<api::devices::DeviceSummary>, String>),
    ArtistDetail { artist_uri: String, result: Result<api::artist::ArtistDetail, String> },
    AlbumDetail { album_uri: String, result: Result<api::album::AlbumDetail, String> },
}

/// Results of Phase 5's mutating calls, following the exact same
/// spawn-then-`try_recv` convention as `LibraryFetchResult` (drained in
/// the same per-frame loop). Every variant that targets a specific
/// playlist carries its URI so a stale response can be checked against
/// whatever's actually showing before being applied.
enum CrudResult {
    PlaylistCreated(Result<PlaylistSummary, String>),
    PlaylistRenamed { playlist_uri: String, new_name: String, result: Result<(), String> },
    PlaylistDeleted { playlist_uri: String, result: Result<(), String> },
    // `track_uri` added alongside the existing fields specifically so
    // the success arms can write through into `AppState::playlist_membership`
    // -- the same optimistic-update-over-refetch lesson `bump_track_count`
    // already established for track counts.
    TrackAdded { playlist_uri: String, track_uri: String, result: Result<(), String> },
    /// The picker's target playlist already contains this track (checked
    /// live before adding, not assumed) -- carries a ready-made message
    /// so the main loop just has to show it, not re-derive the track's
    /// name from a bare URI.
    PlaylistAlreadyHasTrack { playlist_uri: String, track_uri: String, message: String },
    TrackRemoved { playlist_uri: String, track_uri: String, occurrences: usize, result: Result<(), String> },
    TrackReordered { playlist_uri: String, result: Result<(), String> },
    DeviceTransferred(Result<(), String>),
    // Phase 13: like/follow/save. Each carries which *direction* it was
    // (not inferred from the result) so the status message and the
    // affected list's refetch trigger are unambiguous even on failure.
    LikeToggled { track_uri: String, liked: bool, result: Result<(), String> },
    FollowToggled { artist_uri: String, followed: bool, result: Result<(), String> },
    SaveToggled { album_uri: String, saved: bool, result: Result<(), String> },
    QueueAdded { track_uri: String, result: Result<(), String> },
}

/// Owned result of resolving a Sidebar row into an action -- computed in
/// its own statement so the borrow of `app` inside `ui::sidebar_rows(&app)`
/// ends there, before the action actually mutates `app`. A `match`'s
/// scrutinee temporary lives for the whole arm body it's matched into,
/// not just until a binding's last use, so doing the borrow and the
/// mutation in the same match arm does not compile.
enum SidebarAction {
    Goto(Screen),
    OpenPlaylist(PlaylistSummary),
}

const TICK: Duration = Duration::from_millis(100);
/// The redraw interval while a word-by-word lyric sweep is moving (20 fps).
/// At the normal 10 fps the sweep would step visibly; only lines that have
/// word timing, while playing, ever use it (see `ui::word_sweep_active`).
const WORD_TICK: Duration = Duration::from_millis(50);
const DEBOUNCE: Duration = Duration::from_millis(250);
const SEEK_STEP_MS: i64 = 5000;

/// Clamped at both ends: `0` because a negative position is nonsensical,
/// `duration_ms` because librespot's own `Spirc::set_position_ms` doc
/// comment says a target past the track's real length is silently
/// ignored, not clamped there for you -- without this, holding the seek
/// key runs the target past that ceiling within a couple of repeats,
/// and every press after that keeps recomputing a target that's still
/// out of range, silently dropped forever until something else moves
/// the real position. Reported live as "arrow keys stop working."
fn seek_target_ms(current_ms: i64, delta_ms: i64, duration_ms: i64) -> u32 {
    (current_ms + delta_ms).clamp(0, duration_ms.max(0)) as u32
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

#[derive(Clone)]
struct TrackMeta {
    track_id: String,
    artist: String,
    title: String,
    album: Option<String>,
    duration_ms: u32,
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).expect("HOME not set")
}

fn log_file_path() -> PathBuf {
    // macOS convention for app logs, distinct from the cache dir.
    dirs_home().join("Library/Logs/spot-tui/spot-tui.log")
}

fn cache_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "spot-tui")
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("spot-tui-cache"))
}

/// The third element is a clonable handle to the same result channel the
/// lrclib thread below reports through -- Phase 17's Spotify-first-party
/// lookup runs as a `tokio::spawn`ed task per track (not on this thread,
/// since `SpClient::get_lyrics` is async), and sends its own success
/// results straight into this channel so the existing drain loop and its
/// staleness guard (keyed on `fetch_gen`) apply unchanged regardless of
/// which source actually produced the result.
fn spawn_fetch_thread() -> (
    mpsc::Sender<(u64, TrackMeta)>,
    mpsc::Receiver<(u64, CachedLyrics)>,
    mpsc::Sender<(u64, CachedLyrics)>,
) {
    let (req_tx, req_rx) = mpsc::channel::<(u64, TrackMeta)>();
    let (res_tx, res_rx) = mpsc::channel();
    let res_tx_for_spotify = res_tx.clone();

    std::thread::spawn(move || {
        let client = LyricsClient::new(cache_dir());
        for (generation, meta) in req_rx {
            let result = client.fetch(
                &meta.track_id,
                &meta.artist,
                &meta.title,
                meta.album.as_deref(),
                meta.duration_ms,
            );
            if res_tx.send((generation, result)).is_err() {
                return;
            }
        }
    });

    (req_tx, res_rx, res_tx_for_spotify)
}

struct TerminalGuard;

impl TerminalGuard {
    fn new() -> std::io::Result<Self> {
        enable_raw_mode()?;
        execute!(stdout(), EnterAlternateScreen, EnableFocusChange)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = stdout().execute(DisableFocusChange);
        let _ = stdout().execute(LeaveAlternateScreen);
    }
}

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = stdout().execute(LeaveAlternateScreen);
        default_hook(info);
    }));
}

fn tmux_toggle_zoom() {
    if std::env::var("TMUX").is_ok() {
        let _ = std::process::Command::new("tmux")
            .args(["resize-pane", "-Z"])
            .status();
    }
}

/// Whether the real terminal (possibly wrapped in tmux) is Ghostty --
/// used to work around this crate version's missing Ghostty handling
/// (see the call site's own doc comment). Checking `TERM`/`TERM_PROGRAM`
/// alone misses the case reported live: running inside tmux, tmux
/// deliberately overwrites both to its own values (`tmux`/
/// `tmux-256color`) for every pane, by design, so a session can be
/// detached and reattached under a *different* terminal later without
/// panes caring -- there is no way to recover the real outer terminal
/// from those two variables once tmux has rewritten them. tmux still
/// knows the real client terminal internally, though, and exposes it
/// on request -- confirmed live: `tmux display-message -p
/// '#{client_termtype}'` returned `"ghostty 1.3.1"` in the exact
/// session where `$TERM_PROGRAM` inside the pane reported `"tmux"`.
fn is_ghostty() -> bool {
    if std::env::var("TERM_PROGRAM").is_ok_and(|t| t.eq_ignore_ascii_case("ghostty"))
        || std::env::var("TERM").is_ok_and(|t| t.contains("ghostty"))
    {
        return true;
    }
    if std::env::var("TMUX").is_ok()
        && let Ok(output) =
            std::process::Command::new("tmux").args(["display-message", "-p", "#{client_termtype}"]).output()
    {
        let client_termtype = String::from_utf8_lossy(&output.stdout).to_lowercase();
        if client_termtype.contains("ghostty") {
            return true;
        }
    }
    false
}

/// `f`, made universal (every screen except Search, same standing
/// exception every other universal key already has -- every printable
/// character there has to reach the query box). Already on Now Playing:
/// toggles fullscreen in place, same as always. Anywhere else: jumps
/// straight to the fullscreen Now Playing/lyrics view rather than merely
/// flipping a flag that would have no visible effect until Now Playing
/// was reached some other way -- reported live as wanted ("fullscreen
/// lyric view should be global").
fn toggle_or_enter_fullscreen(app: &mut AppState) {
    if *app.nav.top() == Screen::NowPlaying {
        app.fullscreen = !app.fullscreen;
    } else {
        app.fullscreen = true;
        app.nav.goto(Screen::NowPlaying);
    }
    tmux_toggle_zoom();
}

/// While move-mode is active, `pd.selected` is a raw array index (see
/// `render_playlist_detail`'s doc comment on why pinned-first bubbling is
/// skipped during a move). This finds where `real_index` lands in the
/// normal pinned-first display, so callers can convert back to a display
/// position -- used both entering move-mode (display index -> real
/// index) and exiting it (real index -> display index).
fn display_index_for_real_index(pd: &PlaylistDetailState, pinned_tracks: &std::collections::HashSet<String>, real_index: usize) -> usize {
    let label = |t: &TrackResult| format!("{} \u{2014} {}", t.artist, t.title);
    if let Fetch::Ready(items) = &pd.tracks {
        let display = pinned_first(filtered_sorted(items, &pd.filter, &label), pinned_tracks, |t| t.uri.as_str());
        display.iter().position(|&(i, _)| i == real_index).unwrap_or(real_index)
    } else {
        real_index
    }
}

/// Move-mode just exited (confirm or cancel) -- converts `pd.selected`
/// from a raw array index back to its position in the normal
/// pinned-first display that resumes now, so the highlight stays on the
/// same track rather than landing on an unrelated row whenever a pin is
/// active. Shared by both exit paths (`Enter` confirms, `Esc` cancels).
fn resume_normal_display_index(app: &mut AppState) {
    let new_selected = app
        .playlist_detail
        .as_ref()
        .map(|pd| display_index_for_real_index(pd, &app.pinned_tracks, pd.selected));
    if let (Some(pd), Some(new_selected)) = (&mut app.playlist_detail, new_selected) {
        pd.selected = new_selected;
    }
}

/// Re-fetches one playlist's track list, setting `Fetch::Loading` first.
/// Shared by `open_playlist_detail` (opening fresh) and Phase 5's
/// add/remove-track success handlers (refreshing after a mutation) --
/// the second concrete case that justifies pulling this out of
/// `open_playlist_detail` rather than duplicating its spawn body.
fn refetch_playlist_tracks(
    app: &mut AppState,
    library_tx: &mpsc::Sender<LibraryFetchResult>,
    spotify_client: &Option<AuthCodeSpotify>,
    playlist_uri: String,
) {
    if let Some(pd) = &mut app.playlist_detail
        && pd.playlist.uri == playlist_uri {
            pd.tracks = Fetch::Loading;
        }
    match spotify_client.clone() {
        Some(client) => {
            let tx = library_tx.clone();
            let uri_for_task = playlist_uri.clone();
            tokio::spawn(async move {
                let result = api::library::playlist_tracks(&client, &uri_for_task).await;
                let _ = tx.send(LibraryFetchResult::PlaylistTracks { playlist_uri: uri_for_task, result });
            });
        }
        None => {
            if let Some(pd) = &mut app.playlist_detail
                && pd.playlist.uri == playlist_uri {
                    pd.tracks = Fetch::Failed("Spotify client not ready yet".into());
                }
        }
    }
}

/// Re-fetches Your Playlists (and, transitively, the Sidebar's live
/// playlist rows -- both read `app.library.playlists`). There is no
/// generic "whenever `Fetch` is `NotStarted`, refetch" watcher anywhere
/// in this codebase -- the two existing triggers (`client_checked`'s
/// eager fetch, and Library home's push-into-Your-Playlists guard) each
/// fire at most once, at a specific moment, not on every frame. Phase 5's
/// mutations need a third, explicit trigger: call this directly after a
/// create/rename/delete succeeds.
fn refetch_playlists(
    app: &mut AppState,
    library_tx: &mpsc::Sender<LibraryFetchResult>,
    spotify_client: &Option<AuthCodeSpotify>,
) {
    app.library.playlists = Fetch::Loading;
    if let Some(client) = spotify_client.clone() {
        let tx = library_tx.clone();
        tokio::spawn(async move {
            let result = api::library::your_playlists(&client).await.map_err(|e| e.to_string());
            let _ = tx.send(LibraryFetchResult::Playlists(result));
        });
    } else {
        app.library.playlists = Fetch::Failed("Spotify client not ready yet".into());
    }
}

fn refetch_liked_songs(app: &mut AppState, library_tx: &mpsc::Sender<LibraryFetchResult>, spotify_client: &Option<AuthCodeSpotify>) {
    app.library.liked_songs = Fetch::Loading;
    if let Some(client) = spotify_client.clone() {
        let tx = library_tx.clone();
        tokio::spawn(async move {
            let result = api::library::liked_songs(&client).await.map_err(|e| e.to_string());
            let _ = tx.send(LibraryFetchResult::LikedSongs(result));
        });
    } else {
        app.library.liked_songs = Fetch::Failed("Spotify client not ready yet".into());
    }
}

fn refetch_followed_artists(app: &mut AppState, library_tx: &mpsc::Sender<LibraryFetchResult>, spotify_client: &Option<AuthCodeSpotify>) {
    app.library.followed_artists = Fetch::Loading;
    if let Some(client) = spotify_client.clone() {
        let tx = library_tx.clone();
        tokio::spawn(async move {
            let result = api::library::followed_artists(&client).await.map_err(|e| e.to_string());
            let _ = tx.send(LibraryFetchResult::FollowedArtists(result));
        });
    } else {
        app.library.followed_artists = Fetch::Failed("Spotify client not ready yet".into());
    }
}

fn refetch_saved_albums(app: &mut AppState, library_tx: &mpsc::Sender<LibraryFetchResult>, spotify_client: &Option<AuthCodeSpotify>) {
    app.library.saved_albums = Fetch::Loading;
    if let Some(client) = spotify_client.clone() {
        let tx = library_tx.clone();
        tokio::spawn(async move {
            let result = api::library::saved_albums(&client).await.map_err(|e| e.to_string());
            let _ = tx.send(LibraryFetchResult::SavedAlbums(result));
        });
    } else {
        app.library.saved_albums = Fetch::Failed("Spotify client not ready yet".into());
    }
}

/// Locally adjusts a playlist's `track_count` by `delta`, both in the
/// library-wide list (Your Playlists/Sidebar) and in Playlist Detail's
/// own copy if it's the one currently open. Applied immediately after a
/// successful add/remove instead of trusting a refetch to reflect it --
/// a refetch was tried first and reported still showing the stale count.
/// Confirmed via the request log that the refetch really was firing
/// immediately after the add (not a wiring bug), so this isn't a client
/// bug being covered up: Spotify's own `/me/playlists` response just
/// doesn't necessarily reflect a write completed a moment earlier,
/// within the same request-response round trip a refetch fires in. This
/// also sidesteps the refetch briefly flashing the whole list to
/// "loading..." for what should be a single-number update.
fn bump_track_count(app: &mut AppState, playlist_uri: &str, delta: i64) {
    if let Fetch::Ready(items) = &mut app.library.playlists
        && let Some(p) = items.iter_mut().find(|p| p.uri == playlist_uri) {
            p.track_count = (p.track_count as i64 + delta).max(0) as u32;
        }
    if let Some(pd) = &mut app.playlist_detail
        && pd.playlist.uri == playlist_uri {
            pd.playlist.track_count = (pd.playlist.track_count as i64 + delta).max(0) as u32;
        }
}

/// Shared by entering the Devices screen and by a successful transfer
/// (to move the active-device marker) -- the second concrete case that
/// justifies pulling this out rather than duplicating the spawn body.
fn refetch_devices(app: &mut AppState, library_tx: &mpsc::Sender<LibraryFetchResult>, spotify_client: &Option<AuthCodeSpotify>) {
    app.devices.fetch = Fetch::Loading;
    if let Some(client) = spotify_client.clone() {
        let tx = library_tx.clone();
        tokio::spawn(async move {
            let result = api::devices::list_devices(&client).await;
            let _ = tx.send(LibraryFetchResult::Devices(result));
        });
    } else {
        app.devices.fetch = Fetch::Failed("Spotify client not ready yet".into());
    }
}

/// Opens Playlist Detail on `playlist` and kicks off its track fetch.
/// Shared by two real call sites now (Sidebar's own playlist rows, and
/// the Your Playlists screen's Enter) -- the second concrete case that
/// justifies pulling this out rather than duplicating it.
fn open_playlist_detail(
    app: &mut AppState,
    library_tx: &mpsc::Sender<LibraryFetchResult>,
    spotify_client: &Option<AuthCodeSpotify>,
    playlist: PlaylistSummary,
) {
    app.nav.push(Screen::PlaylistDetail);
    let playlist_uri = playlist.uri.clone();
    app.playlist_detail = Some(PlaylistDetailState {
        playlist,
        tracks: Fetch::Loading,
        selected: 0,
        filter: ListFilter::default(),
        move_mode: None,
    });
    refetch_playlist_tracks(app, library_tx, spotify_client, playlist_uri);
}

/// Opens Artist Detail on `artist_uri` (Phase 9) and kicks off its fetch.
/// Reachable from Followed Artists, Search results, Liked Songs, Playlist
/// Detail, Queue, and Album Detail -- every one of those just needs an
/// artist URI, so they all funnel through this one function rather than
/// each spawning the fetch themselves.
fn open_artist_detail(
    app: &mut AppState,
    library_tx: &mpsc::Sender<LibraryFetchResult>,
    spotify_client: &Option<AuthCodeSpotify>,
    artist_uri: String,
) {
    if artist_uri.is_empty() {
        app.status = Some(("no artist info for this track".to_string(), true));
        return;
    }
    app.nav.push(Screen::ArtistDetail);
    app.artist_detail = Some(ArtistDetailState { artist_uri: artist_uri.clone(), detail: Fetch::Loading, selected: 0 });
    match spotify_client.clone() {
        Some(client) => {
            let tx = library_tx.clone();
            let uri_for_task = artist_uri.clone();
            tokio::spawn(async move {
                let result = api::artist::get_artist_detail(&client, &uri_for_task).await;
                let _ = tx.send(LibraryFetchResult::ArtistDetail { artist_uri: uri_for_task, result });
            });
        }
        None => {
            if let Some(state) = &mut app.artist_detail {
                state.detail = Fetch::Failed("Spotify client not ready yet".into());
            }
        }
    }
}

/// Opens Album Detail on `album_uri` (Phase 9) and kicks off its fetch.
/// Reachable from Saved Albums and from Artist Detail's own album list.
fn open_album_detail(
    app: &mut AppState,
    library_tx: &mpsc::Sender<LibraryFetchResult>,
    spotify_client: &Option<AuthCodeSpotify>,
    album_uri: String,
) {
    if album_uri.is_empty() {
        app.status = Some(("no album info for this track".to_string(), true));
        return;
    }
    app.nav.push(Screen::AlbumDetail);
    app.album_detail = Some(AlbumDetailState { album_uri: album_uri.clone(), detail: Fetch::Loading, selected: 0 });
    match spotify_client.clone() {
        Some(client) => {
            let tx = library_tx.clone();
            let uri_for_task = album_uri.clone();
            tokio::spawn(async move {
                let result = api::album::get_album_detail(&client, &uri_for_task).await;
                let _ = tx.send(LibraryFetchResult::AlbumDetail { album_uri: uri_for_task, result });
            });
        }
        None => {
            if let Some(state) = &mut app.album_detail {
                state.detail = Fetch::Failed("Spotify client not ready yet".into());
            }
        }
    }
}

/// A physical Shift+<letter>: matches the literal uppercase char (how
/// most terminals report it) as well as lowercase-plus-SHIFT-modifier
/// (how some terminals/configurations report it instead). Originally
/// written just for Shift+P -- reported live as "pinning does not do
/// anything," most likely this exact platform encoding gap rather than
/// the pin logic itself being wrong -- generalized here as the second
/// concrete case (Shift+D, deleting the open playlist from within
/// Playlist Detail, needs the identical robustness).
fn is_shift_char(code: KeyCode, modifiers: KeyModifiers, upper: char, lower: char) -> bool {
    code == KeyCode::Char(upper) || (code == KeyCode::Char(lower) && modifiers.contains(KeyModifiers::SHIFT))
}

fn is_pin_key(code: KeyCode, modifiers: KeyModifiers) -> bool {
    is_shift_char(code, modifiers, 'P', 'p')
}

/// Fires the mutation behind a confirmed `ConfirmAction`. Split out of
/// `handle_confirm_key` so the "no client yet" bail-out is written once.
/// Returns whether the caller should actually quit -- `ConfirmAction::Quit`
/// needs no client and can't spawn anything, it just tells the main loop
/// to break its event loop once confirmed (a labeled `break` can't cross
/// a function boundary, so this is the only way that signal gets back).
fn fire_confirm_action(
    app: &mut AppState,
    action: ConfirmAction,
    crud_tx: &mpsc::Sender<CrudResult>,
    spotify_client: &Option<AuthCodeSpotify>,
) -> bool {
    if matches!(action, ConfirmAction::Quit) {
        return true;
    }
    let Some(client) = spotify_client.clone() else {
        app.status = Some(("Spotify client not ready yet".to_string(), true));
        return false;
    };
    match action {
        ConfirmAction::Quit => unreachable!("handled above"),
        ConfirmAction::DeletePlaylist(playlist) => {
            app.status = Some((format!("deleting \"{}\"\u{2026}", playlist.name), false));
            let tx = crud_tx.clone();
            let playlist_uri = playlist.uri.clone();
            tokio::spawn(async move {
                let result = api::playlists::delete_playlist(&client, &playlist_uri).await;
                let _ = tx.send(CrudResult::PlaylistDeleted { playlist_uri, result });
            });
        }
        ConfirmAction::RemoveTrack { playlist_uri, track_uri, occurrences } => {
            app.status = Some(("removing track\u{2026}".to_string(), false));
            let tx = crud_tx.clone();
            let playlist_uri_for_result = playlist_uri.clone();
            tokio::spawn(async move {
                let result = api::playlists::remove_track(&client, &playlist_uri, &track_uri).await;
                let _ = tx.send(CrudResult::TrackRemoved {
                    playlist_uri: playlist_uri_for_result,
                    track_uri,
                    occurrences,
                    result,
                });
            });
        }
        ConfirmAction::AddTrackAnyway { playlist_uri, track_uri } => {
            app.status = Some(("adding to playlist\u{2026}".to_string(), false));
            let tx = crud_tx.clone();
            let playlist_uri_for_result = playlist_uri.clone();
            tokio::spawn(async move {
                let result = api::playlists::add_track(&client, &playlist_uri, &track_uri).await;
                let _ = tx.send(CrudResult::TrackAdded { playlist_uri: playlist_uri_for_result, track_uri, result });
            });
        }
        ConfirmAction::UnlikeTrack { track_uri } => {
            app.status = Some(("unliking\u{2026}".to_string(), false));
            let tx = crud_tx.clone();
            tokio::spawn(async move {
                let result = api::library::unlike_track(&client, &track_uri).await;
                let _ = tx.send(CrudResult::LikeToggled { track_uri, liked: false, result });
            });
        }
        ConfirmAction::UnfollowArtist { artist_uri } => {
            app.status = Some(("unfollowing\u{2026}".to_string(), false));
            let tx = crud_tx.clone();
            tokio::spawn(async move {
                let result = api::library::unfollow_artist(&client, &artist_uri).await;
                let _ = tx.send(CrudResult::FollowToggled { artist_uri, followed: false, result });
            });
        }
        ConfirmAction::UnsaveAlbum { album_uri } => {
            app.status = Some(("unsaving\u{2026}".to_string(), false));
            let tx = crud_tx.clone();
            tokio::spawn(async move {
                let result = api::library::unsave_album(&client, &album_uri).await;
                let _ = tx.send(CrudResult::SaveToggled { album_uri, saved: false, result });
            });
        }
    }
    false
}

/// `y` confirms and fires the mutation; `n`/`Esc` cancels; every other
/// key is swallowed without touching or dismissing the dialog -- an
/// arbitrary keypress shouldn't accidentally confirm or cancel a
/// destructive action.
/// Returns whether the caller should break its event loop and quit.
fn handle_confirm_key(
    app: &mut AppState,
    code: KeyCode,
    crud_tx: &mpsc::Sender<CrudResult>,
    spotify_client: &Option<AuthCodeSpotify>,
) -> bool {
    match code {
        // Enter as a synonym for `y` -- reported live as wanted, matches
        // `Enter`'s standing role elsewhere in the app as the one
        // universal "confirm/activate" key.
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
            if let Some(confirm) = app.pending_confirm.take() {
                return fire_confirm_action(app, confirm.action, crud_tx, spotify_client);
            }
            false
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            app.pending_confirm = None;
            false
        }
        _ => false,
    }
}

/// Whether a printable key is about to be typed into a text field right
/// now -- Search's query box, or a list's `/` filter -- so a global letter
/// binding (shuffle) has to stay out of the way. Scoped to the screen
/// that's actually on top with focus in Main, not just "any filter's
/// `editing` flag is set": `Tab` moves focus to the Sidebar without
/// clearing a filter that was mid-edit, and a stale flag would otherwise
/// leave the key silently dead until that filter got an `Esc`.
fn text_input_active(app: &AppState) -> bool {
    if app.nav.focus != Focus::Main {
        return false;
    }
    match *app.nav.top() {
        Screen::Search => true,
        Screen::LikedSongs => app.library.liked_songs_filter.editing,
        Screen::SavedAlbums => app.library.saved_albums_filter.editing,
        Screen::FollowedArtists => app.library.followed_artists_filter.editing,
        Screen::YourPlaylists => app.library.playlists_filter.editing,
        Screen::PlaylistDetail => app.playlist_detail.as_ref().is_some_and(|pd| pd.filter.editing),
        _ => false,
    }
}

/// `z`: shuffle off <-> on. The player is the source of truth (its
/// `ShuffleChanged` event lands in `app.shuffle` and drives the playbar),
/// but the flag is also set here so a quick second press toggles back from
/// what was just requested instead of a not-yet-updated value. Note
/// librespot emits that event with the *requested* value before it
/// validates the change, so a toggle the current context disallows (its
/// restrictions can forbid shuffling) would still read as on here while
/// the player quietly refuses -- a real librespot quirk, not something
/// this app can detect from its side.
/// librespot resets shuffle and both repeat flags on every `load` unless the
/// request carries explicit options, and emits no event when it does -- so a
/// new playlist/album/track silently turned shuffle off in Spotify while the
/// playbar stayed lit. Spotify's own clients keep shuffle and repeat across
/// context switches, so every load hands the current values back in.
fn carry_modes(shuffle: bool, repeat: RepeatMode, mut opts: LoadRequestOptions) -> LoadRequestOptions {
    let (repeat_context, repeat_track) = repeat.flags();
    opts.context_options = Some(librespot_connect::LoadContextOptions::Options(librespot_connect::Options {
        shuffle,
        repeat: repeat_context,
        repeat_track,
    }));
    opts
}

fn cycle_shuffle(app: &mut AppState, spirc: &Spirc) {
    let next = ShuffleMode::from_flags(app.shuffle, app.smart_shuffle).next();
    let sent = match next {
        ShuffleMode::Off => spirc.shuffle(false),
        ShuffleMode::On => spirc.shuffle(true),
        ShuffleMode::Smart => spirc.smart_shuffle(),
    };
    match sent {
        // Only `shuffle` is set optimistically: the smart flag is read back
        // from librespot's own state on the next pass, so it can't drift.
        Ok(()) => {
            app.shuffle = next.shuffle();
            app.status = Some((next.status_label().to_string(), false));
        }
        Err(e) => app.status = Some((format!("couldn't change shuffle: {e}"), true)),
    }
}

/// `Shift+R`: repeat off -> album/playlist -> this song -> off. The player
/// keeps repeat as two independent flags, so each step sets both.
fn cycle_repeat(app: &mut AppState, spirc: &Spirc) {
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

/// Appends `track_uri` to the playback queue. Shared by every screen with
/// a selected track (Liked Songs, Playlist Detail, Album Detail, Search) --
/// four identical spawn-then-report bodies otherwise. Fires immediately with
/// no confirm, same as `a` (add-to-playlist) and like: adding is
/// non-destructive, and the queue can't be un-added-to from this app
/// anyway (no remove endpoint exists), so a confirm here would only be
/// friction with nothing to protect.
fn fire_add_to_queue(
    app: &mut AppState,
    crud_tx: &mpsc::Sender<CrudResult>,
    spotify_client: &Option<AuthCodeSpotify>,
    track_uri: String,
) {
    let Some(client) = spotify_client.clone() else {
        app.status = Some(("Spotify client not ready yet".to_string(), true));
        return;
    };
    app.status = Some(("adding to queue\u{2026}".to_string(), false));
    let tx = crud_tx.clone();
    tokio::spawn(async move {
        let result = api::queue::add_to_queue(&client, &track_uri).await;
        let _ = tx.send(CrudResult::QueueAdded { track_uri, result });
    });
}

/// Move-mode's `g`: splices the track being moved straight to a typed
/// 1-based position instead of nudging it one slot per keystroke -- moving
/// a track 49 slots was 49 keypresses, the single most quantifiable place
/// this app was slower than dragging in Spotify's own UI. Purely a local
/// reorder of the already-fetched list via the same `move_item_to` the
/// `Esc`-cancel path uses; move-mode stays active afterward, so `Enter`
/// still sends exactly one `reorder_track(start, end)` for the net
/// displacement and `Esc` still walks the track back to where it began.
/// A bad answer leaves the list untouched and says why.
fn apply_move_to_position(app: &mut AppState, input: &str) {
    let Some(pd) = &mut app.playlist_detail else { return };
    if pd.move_mode.is_none() {
        return;
    }
    let Fetch::Ready(items) = &mut pd.tracks else { return };
    match ui::parse_move_position(input, items.len()) {
        Ok(target) => pd.selected = ui::move_item_to(items, pd.selected, target),
        Err(message) => app.status = Some((message, true)),
    }
}

/// `Enter` submits (fires create/rename); `Esc` cancels outright -- unlike
/// `ListFilter`'s filter box, there's no "keep it applied" middle state
/// for a name that was never submitted. Every other key edits the field,
/// same cursor conventions as Search/`ListFilter`.
fn handle_text_prompt_key(
    app: &mut AppState,
    code: KeyCode,
    crud_tx: &mpsc::Sender<CrudResult>,
    spotify_client: &Option<AuthCodeSpotify>,
) {
    match code {
        KeyCode::Esc => {
            app.text_prompt = None;
        }
        KeyCode::Enter => {
            let Some(prompt) = app.text_prompt.take() else { return };
            // Purely local, needs no Spotify client and no name -- handled
            // before either of the checks below, which only make sense for
            // the two prompts that actually call the API.
            if matches!(prompt.action, TextPromptAction::MoveToPosition) {
                apply_move_to_position(app, &prompt.query);
                return;
            }
            let name = prompt.query.trim().to_string();
            if name.is_empty() {
                app.status = Some(("name can't be empty".to_string(), true));
                return;
            }
            let Some(client) = spotify_client.clone() else {
                app.status = Some(("Spotify client not ready yet".to_string(), true));
                return;
            };
            let tx = crud_tx.clone();
            match prompt.action {
                TextPromptAction::CreatePlaylist => {
                    app.status = Some(("creating playlist\u{2026}".to_string(), false));
                    tokio::spawn(async move {
                        let result = api::playlists::create_playlist(&client, &name).await;
                        let _ = tx.send(CrudResult::PlaylistCreated(result));
                    });
                }
                TextPromptAction::RenamePlaylist(playlist) => {
                    app.status = Some(("renaming playlist\u{2026}".to_string(), false));
                    let playlist_uri = playlist.uri;
                    tokio::spawn(async move {
                        let result = api::playlists::rename_playlist(&client, &playlist_uri, &name).await;
                        let _ = tx.send(CrudResult::PlaylistRenamed { playlist_uri, new_name: name, result });
                    });
                }
                // Already handled and returned above.
                TextPromptAction::MoveToPosition => {}
            }
        }
        KeyCode::Backspace => {
            if let Some(prompt) = &mut app.text_prompt {
                prompt.backspace_at_cursor();
            }
        }
        KeyCode::Left => {
            if let Some(prompt) = &mut app.text_prompt {
                prompt.cursor_left();
            }
        }
        KeyCode::Right => {
            if let Some(prompt) = &mut app.text_prompt {
                prompt.cursor_right();
            }
        }
        KeyCode::Char(c) => {
            if let Some(prompt) = &mut app.text_prompt {
                prompt.insert_at_cursor(c);
            }
        }
        _ => {}
    }
}

/// `Up`/`Down` move the picker's own selection; `Enter` adds the captured
/// track to whichever playlist is selected and closes; `Esc` cancels.
fn handle_picker_key(
    app: &mut AppState,
    code: KeyCode,
    crud_tx: &mpsc::Sender<CrudResult>,
    library_tx: &mpsc::Sender<LibraryFetchResult>,
    spotify_client: &Option<AuthCodeSpotify>,
) {
    let count = |app: &AppState| match &app.library.playlists {
        Fetch::Ready(items) => {
            let label = |p: &PlaylistSummary| p.name.clone();
            app.playlist_picker
                .as_ref()
                .map(|picker| filtered_sorted(items, &picker.filter, &label).len())
                .unwrap_or(0)
        }
        _ => 0,
    };
    match code {
        KeyCode::Esc => {
            app.playlist_picker = None;
        }
        KeyCode::Up => {
            if let Some(picker) = &mut app.playlist_picker {
                picker.selected = picker.selected.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            let count = count(app);
            if let Some(picker) = &mut app.playlist_picker
                && count > 0 {
                    picker.selected = (picker.selected + 1).min(count - 1);
                }
        }
        KeyCode::Left => {
            if let Some(picker) = &mut app.playlist_picker {
                picker.filter.cursor_left();
            }
        }
        KeyCode::Right => {
            if let Some(picker) = &mut app.playlist_picker {
                picker.filter.cursor_right();
            }
        }
        KeyCode::Backspace => {
            if let Some(picker) = &mut app.playlist_picker {
                picker.filter.backspace_at_cursor();
                picker.selected = 0;
            }
        }
        KeyCode::Char(c) => {
            if let Some(picker) = &mut app.playlist_picker {
                picker.filter.insert_at_cursor(c);
                picker.selected = 0;
            }
        }
        KeyCode::Enter => {
            let label = |p: &PlaylistSummary| p.name.clone();
            let picked_playlist = match &app.library.playlists {
                Fetch::Ready(items) => {
                    let ordered = app.playlist_picker.as_ref().map(|picker| {
                        pinned_first(filtered_sorted(items, &picker.filter, &label), &app.pinned_playlists, |p| {
                            p.uri.as_str()
                        })
                    });
                    ordered.and_then(|ordered| {
                        app.playlist_picker
                            .as_ref()
                            .and_then(|picker| ordered.get(picker.selected).map(|&(_, p)| (p.uri.clone(), p.name.clone())))
                    })
                }
                _ => None,
            };
            let Some(picker) = app.playlist_picker.take() else { return };
            let Some((playlist_uri, playlist_name)) = picked_playlist else { return };
            let Some(client) = spotify_client.clone() else {
                app.status = Some(("Spotify client not ready yet".to_string(), true));
                return;
            };
            app.status = Some(("adding to playlist\u{2026}".to_string(), false));
            let tx = crud_tx.clone();
            let lib_tx = library_tx.clone();
            let track_uri = picker.track_uri.clone();
            tokio::spawn(async move {
                // Checks membership before adding, rather than warning
                // after the fact the way `d`'s duplicate-removal warning
                // has to (removal can't be undone; a would-be duplicate
                // add can be caught before it ever happens). Fetches the
                // whole target playlist to do it -- Spotify's Web API has
                // no "does this playlist contain this URI" endpoint, so
                // there's no cheaper primitive to check against. Falls
                // back to adding directly if the fetch itself fails,
                // rather than blocking the add on a check that couldn't
                // run.
                match api::library::playlist_tracks(&client, &playlist_uri).await {
                    Ok(tracks) => {
                        // A free opportunity to populate the picker's
                        // membership cache for this whole playlist, not
                        // just the one track being checked -- this fetch
                        // already has the full list in hand. Routed
                        // through the same `LibraryFetchResult::PlaylistTracks`
                        // arm every other track-list fetch already goes
                        // through, so there's no second cache-population
                        // code path to keep in sync.
                        let _ = lib_tx.send(LibraryFetchResult::PlaylistTracks {
                            playlist_uri: playlist_uri.clone(),
                            result: Ok(tracks.clone()),
                        });
                        if let Some(existing) = tracks.iter().find(|t| t.uri == track_uri) {
                            let message = format!(
                                "\"{} \u{2014} {}\" is already in \"{playlist_name}\". Add it again anyway? y/n",
                                existing.artist, existing.title
                            );
                            let _ = tx.send(CrudResult::PlaylistAlreadyHasTrack { playlist_uri, track_uri, message });
                        } else {
                            let result = api::playlists::add_track(&client, &playlist_uri, &track_uri).await;
                            let _ = tx.send(CrudResult::TrackAdded { playlist_uri, track_uri, result });
                        }
                    }
                    Err(e) => {
                        log::warn!("duplicate check before add_track failed, adding anyway: {e}");
                        let result = api::playlists::add_track(&client, &playlist_uri, &track_uri).await;
                        let _ = tx.send(CrudResult::TrackAdded { playlist_uri, track_uri, result });
                    }
                }
            });
        }
        _ => {}
    }
}

/// `true` for the global quick-jump trigger (`Ctrl+P`) -- checked both to
/// open the overlay and, while it's already open, to close it again
/// (toggle), rather than letting a repeat press fall through to the
/// filter's own `Char` arm and insert a stray `p`.
fn is_quick_jump_trigger(code: KeyCode, modifiers: KeyModifiers) -> bool {
    code == KeyCode::Char('p') && modifiers.contains(KeyModifiers::CONTROL)
}

/// Length of the currently filtered quick-jump list -- computed via an
/// immutable borrow of `app` before any `&mut app.quick_jump` borrow is
/// taken, same ordering `handle_picker_key`'s own `count` closure uses to
/// dodge the exact same borrow conflict.
fn quick_jump_count(app: &AppState) -> usize {
    let Some(qj) = &app.quick_jump else { return 0 };
    let entries = quick_jump_entries(app, &qj.filter);
    let label = |e: &QuickJumpEntry| e.label.clone();
    filtered_sorted(&entries, &qj.filter, &label).len()
}

/// Fires whatever the picked quick-jump entry means -- exactly what that
/// entity's own home screen's `Enter` already does, no new activation
/// semantics invented. `Playlist`/`Artist`/`Album` collapse the nav stack
/// to `[NowPlaying]` first (`goto`) so the destination lands at a clean
/// `[NowPlaying, X]`, matching "teleport" semantics rather than stacking
/// on top of whatever drill-down depth the overlay happened to be opened
/// from -- unlike `Screen::Help`, which drills in and returns to wherever
/// it was opened from, matching its existing global `?` convention.
fn activate_quick_jump(
    app: &mut AppState,
    kind: QuickJumpKind,
    spirc: &Spirc,
    crud_tx: &mpsc::Sender<CrudResult>,
    library_tx: &mpsc::Sender<LibraryFetchResult>,
    spotify_client: &Option<AuthCodeSpotify>,
) {
    match kind {
        QuickJumpKind::Screen(Screen::Help) => {
            app.nav.push(Screen::Help);
            app.nav.focus = Focus::Main;
        }
        QuickJumpKind::Screen(screen) => {
            app.nav.goto(screen);
            app.nav.focus = Focus::Main;
            if screen == Screen::Search {
                app.search.query.clear();
                app.search.cursor = 0;
                app.search.results.clear();
                app.search.error = None;
            }
            if screen == Screen::Devices && matches!(app.devices.fetch, Fetch::NotStarted) {
                refetch_devices(app, library_tx, spotify_client);
            }
        }
        QuickJumpKind::Playlist(playlist) => {
            app.nav.goto(Screen::NowPlaying);
            open_playlist_detail(app, library_tx, spotify_client, playlist);
            app.nav.focus = Focus::Main;
        }
        QuickJumpKind::Artist { uri } => {
            app.nav.goto(Screen::NowPlaying);
            open_artist_detail(app, library_tx, spotify_client, uri);
            app.nav.focus = Focus::Main;
        }
        QuickJumpKind::Album { uri } => {
            app.nav.goto(Screen::NowPlaying);
            open_album_detail(app, library_tx, spotify_client, uri);
            app.nav.focus = Focus::Main;
        }
        // Single-track context, matching Liked Songs' own Enter handler
        // exactly -- `context_label` names its real origin rather than
        // something generic like "Quick Jump", since that's what the
        // user would see had they navigated to Liked Songs and pressed
        // Enter there instead.
        QuickJumpKind::Track(track) => {
            let _ = spirc.activate();
            let opts = carry_modes(app.shuffle, app.repeat, Default::default());
            let _ = spirc.load(LoadRequest::from_context_uri(track.uri, opts));
            let _ = spirc.play();
            app.context_label = Some("Liked Songs".to_string());
            app.nav.goto(Screen::NowPlaying);
        }
        // Matches the Devices screen's own Enter handler exactly --
        // stays wherever the user was, doesn't navigate away.
        QuickJumpKind::Device(device) => {
            if let Some(client) = spotify_client.clone() {
                app.status = Some(("transferring playback\u{2026}".to_string(), false));
                let tx = crud_tx.clone();
                let device_id = device.id;
                tokio::spawn(async move {
                    let result = api::devices::transfer_to(&client, &device_id).await;
                    let _ = tx.send(CrudResult::DeviceTransferred(result));
                });
            } else {
                app.status = Some(("Spotify client not ready yet".to_string(), true));
            }
        }
    }
}

/// `Up`/`Down` move the selection; `Char`/`Backspace` edit the filter
/// (resetting `selected` to 0, same as the picker); `Left`/`Right` move
/// the filter's text cursor; `Enter` activates; `Esc` or the trigger key
/// again (`Ctrl+P`) closes. Mirrors `handle_picker_key`'s shape exactly,
/// widened to take `modifiers` (not just `code`) -- the one thing none of
/// the three Phase 5 overlay handlers needed before this, since none of
/// them has a "press the same key again to close" convention.
fn handle_quick_jump_key(
    app: &mut AppState,
    code: KeyCode,
    modifiers: KeyModifiers,
    spirc: &Spirc,
    crud_tx: &mpsc::Sender<CrudResult>,
    library_tx: &mpsc::Sender<LibraryFetchResult>,
    spotify_client: &Option<AuthCodeSpotify>,
) {
    if is_quick_jump_trigger(code, modifiers) {
        app.quick_jump = None;
        return;
    }
    match code {
        KeyCode::Esc => {
            app.quick_jump = None;
        }
        KeyCode::Up => {
            if let Some(qj) = &mut app.quick_jump {
                qj.selected = qj.selected.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            let count = quick_jump_count(app);
            if let Some(qj) = &mut app.quick_jump
                && count > 0 {
                    qj.selected = (qj.selected + 1).min(count - 1);
                }
        }
        KeyCode::Left => {
            if let Some(qj) = &mut app.quick_jump {
                qj.filter.cursor_left();
            }
        }
        KeyCode::Right => {
            if let Some(qj) = &mut app.quick_jump {
                qj.filter.cursor_right();
            }
        }
        KeyCode::Backspace => {
            if let Some(qj) = &mut app.quick_jump {
                qj.filter.backspace_at_cursor();
                qj.selected = 0;
            }
        }
        KeyCode::Char(c) => {
            if let Some(qj) = &mut app.quick_jump {
                qj.filter.insert_at_cursor(c);
                qj.selected = 0;
            }
        }
        KeyCode::Enter => {
            let picked_kind = match &app.quick_jump {
                Some(qj) => {
                    let entries = quick_jump_entries(app, &qj.filter);
                    let label = |e: &QuickJumpEntry| e.label.clone();
                    filtered_sorted(&entries, &qj.filter, &label).get(qj.selected).map(|&(_, e)| e.kind.clone())
                }
                None => None,
            };
            app.quick_jump = None;
            if let Some(kind) = picked_kind {
                activate_quick_jump(app, kind, spirc, crud_tx, library_tx, spotify_client);
            }
        }
        _ => {}
    }
}

enum LoopExit {
    Quit,
    Disconnected,
}

/// Drains every currently-queued `PlayerEvent`, applying each exactly as
/// the main loop always has (`VolumeChanged` updates the gauge,
/// `TrackChanged` refreshes every piece of track state plus kicks off
/// the cover-art fetch and the debounced lyrics fetch, every event feeds
/// `tracker`). Extracted as its own function -- not just the main inner
/// loop's own inline block -- so the startup track-settle wait (right
/// after the very first `spirc.transfer`, see its own doc comment) can
/// call the identical logic instead of drifting out of sync with a
/// second, hand-copied version. Returns whether a `TrackChanged` was
/// among the drained events, which is the only thing the settle-wait
/// needs to know.
#[allow(clippy::too_many_arguments)]
fn drain_player_events(
    app: &mut AppState,
    images: &ui::ImageState,
    tracker: &mut PositionTracker,
    generation: &mut u64,
    cover_tx: &mpsc::Sender<(u64, image::DynamicImage)>,
    pending_fetch: &mut Option<(u64, TrackMeta, Instant)>,
    player_events: &mut librespot_playback::player::PlayerEventChannel,
) -> bool {
    let mut track_changed = false;
    while let Ok(event) = player_events.try_recv() {
        let now = Instant::now();

        if let PlayerEvent::VolumeChanged { volume } = &event {
            app.volume = *volume;
        }
        if let PlayerEvent::ShuffleChanged { shuffle } = &event {
            app.shuffle = *shuffle;
        }
        if let PlayerEvent::RepeatChanged { context, track } = &event {
            app.repeat = RepeatMode::from_flags(*context, *track);
        }

        if let PlayerEvent::TrackChanged { audio_item } = &event {
            let (artist, album) = match &audio_item.unique_fields {
                UniqueFields::Track { artists, album, .. } => (
                    artists.0.first().map(|a| a.name.clone()).unwrap_or_default(),
                    Some(album.clone()),
                ),
                _ => (String::new(), None),
            };
            app.track_title = Some(audio_item.name.clone());
            app.track_artist = if artist.is_empty() { None } else { Some(artist.clone()) };
            app.track_album = album.clone();
            app.current_track_uri = Some(audio_item.track_id.to_string());
            app.duration = Duration::from_millis(audio_item.duration_ms as u64);
            app.lyrics = LyricsState::Loading;
            app.lyrics_credit = None;
            *generation += 1;

            // Real album art, only worth fetching at all if a real
            // graphics protocol is actually in use -- `covers` is
            // already sorted largest-first by librespot itself.
            if images.picker.is_some()
                && let Some(cover_url) = audio_item.covers.first().map(|c| c.url.clone()) {
                    let tx = cover_tx.clone();
                    let cover_gen = *generation;
                    tokio::task::spawn_blocking(move || {
                        let fetched = ureq::get(&cover_url).call().ok().and_then(|resp| {
                            let mut bytes = Vec::new();
                            resp.into_reader().read_to_end(&mut bytes).ok()?;
                            image::load_from_memory(&bytes).ok()
                        });
                        if let Some(img) = fetched {
                            let _ = tx.send((cover_gen, img));
                        }
                    });
                }

            *pending_fetch = Some((
                *generation,
                TrackMeta {
                    track_id: audio_item.track_id.to_string(),
                    artist,
                    title: audio_item.name.clone(),
                    album,
                    duration_ms: audio_item.duration_ms,
                },
                Instant::now() + DEBOUNCE,
            ));
            track_changed = true;
        }

        tracker.on_event(&event, now);
    }
    // No player event carries smart shuffle, so read it from librespot's
    // connect state on every pass instead. Shuffle off always wins.
    app.smart_shuffle = app.shuffle && librespot_connect::smart_shuffle_active();
    track_changed
}

/// Full librespot/Connect bootstrap, extracted so it can be retried:
/// Tier 4 resilience -- a session drop (laptop sleep, wifi blip) used to
/// leave the app permanently in `SessionEnded` with nothing to recover
/// it. Returns `Err` instead of panicking on any failure, since a failed
/// reconnect attempt should trigger backoff-and-retry, not crash.
async fn connect_spirc() -> Result<
    (
        Spirc,
        tokio::task::JoinHandle<()>,
        librespot_playback::player::PlayerEventChannel,
        Session,
    ),
    String,
> {
    // credentials_path stays pointed at ncspot's cache -- that's the
    // actual auth-reuse point. volume_path/audio_path get our own
    // directory: librespot treats volume_path as a directory and writes
    // a file literally named `volume` inside it, which collided with
    // ncspot's own pre-existing `volume/` subdirectory at that path
    // (confirmed live: "Cannot save volume to cache: Is a directory").
    let ncspot_librespot_dir = dirs_home().join(".cache/ncspot/librespot");
    let own_librespot_cache_dir = cache_dir().join("librespot");
    let cache = Cache::new(
        Some(&ncspot_librespot_dir),
        Some(&own_librespot_cache_dir),
        Some(&own_librespot_cache_dir),
        None,
    )
    .map_err(|e| e.to_string())?;
    let credentials = cache
        .credentials()
        .ok_or("no cached credentials found in ncspot's cache")?;

    let session = Session::new(SessionConfig::default(), Some(cache));
    let mixer_fn = mixer::find(None).ok_or("no default mixer available")?;
    let mixer = mixer_fn(MixerConfig::default()).map_err(|e| e.to_string())?;
    let backend = audio_backend::find(None).ok_or("no default audio backend")?;
    let soft_volume = mixer.get_soft_volume();
    let player = Player::new(PlayerConfig::default(), session.clone(), soft_volume, move || {
        backend(None, AudioFormat::default())
    });
    let player_events = player.get_player_event_channel();

    let connect_config = ConnectConfig {
        name: "spot-tui".to_string(),
        device_type: DeviceType::Computer,
        // ConnectConfig::default() sets this to u16::MAX / 2 (50%), which
        // feeds the mixer's actual soft-volume attenuation -- a real
        // loudness drop on every fresh launch, not just a displayed
        // number. Start at max instead; loudness is controlled via the
        // system/global volume, matching how it's normally used.
        initial_volume: u16::MAX,
        ..ConnectConfig::default()
    };
    // Second handle to the same session (Session is a cheap Arc-style
    // clone), kept for Phase 17's first-party lyrics lookup
    // (session.spclient().get_lyrics) -- the original is consumed by
    // Spirc::new below, which owns driving the Connect session itself.
    let lyrics_session = session.clone();
    let (spirc, spirc_task) = Spirc::new(connect_config, session, credentials, player, mixer)
        .await
        .map_err(|e| e.to_string())?;
    let spirc_handle = tokio::spawn(spirc_task);

    Ok((spirc, spirc_handle, player_events, lyrics_session))
}

/// Spicy Lyrics for one track: a cached *synced* result first (so a replay
/// never spends rate-limit quota), then the API, storing whatever it returns.
/// A stale lrclib `Plain`/`NotFound` entry is deliberately not a hit (see
/// `cached_synced`), and a synced result overwrites it.
async fn spicy_lookup(client: &spicy::SpicyClient, track_uri: &str, base62_id: &str) -> Option<CachedLyrics> {
    let dir = cache_dir();
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    // Its own cache key: entries from before word timing existed sit under
    // the plain uri and are never read here, so they refresh on next play.
    let key = spicy_cache_key(track_uri);
    if let Some(hit) = cached_synced(&dir, &key, now_unix) {
        return Some(hit);
    }
    let spicy::SpicyLyrics { lines, words, credit } = client.lyrics(base62_id).await?;
    if let Err(e) = store_synced(&dir, &key, lines.clone(), words.clone(), Some(credit.clone()), now_unix) {
        log::warn!("spicy_lyrics[{base62_id}]: couldn't cache the result: {e}");
    }
    Some(CachedLyrics::Synced { lines, words, credit: Some(credit) })
}

fn to_lyrics_state(cached: CachedLyrics) -> LyricsState {
    match cached {
        CachedLyrics::Synced { lines, words, .. } => LyricsState::Synced(
            lines
                .into_iter()
                .enumerate()
                .map(|(i, (secs, text))| LyricLine {
                    timestamp: Duration::from_secs_f64(secs),
                    text,
                    // Parallel to `lines`; empty (or short, if a file is
                    // corrupt) just means "no word timing for this line".
                    words: words.get(i).cloned().unwrap_or_default(),
                })
                .collect(),
        ),
        CachedLyrics::Plain { text } => LyricsState::Plain(text),
        CachedLyrics::Instrumental => LyricsState::Instrumental,
        CachedLyrics::NotFound => LyricsState::NotFound,
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // Phase 0 design-scope spike: verify spirc.transfer/device-transfer and
    // the playlist-reorder endpoint before any UI gets built around them.
    // No TUI involved -- runs and exits.
    if std::env::args().any(|a| a == "--spike-phase0") {
        let token = api::load_or_refresh_token()
            .await
            .map_err(std::io::Error::other)?;
        let client = api::client_from_token(token).await;
        if let Err(e) = spike::run_phase0(&client).await {
            eprintln!("spike failed: {e}");
        }
        return Ok(());
    }
    // Phase 5's remove_track risk #1 tripwire: reported live (duplicate
    // track in a playlist, removing one removed both). Checks the
    // position-scoped removal call actually behaves before switching
    // remove_track over to it.
    if std::env::args().any(|a| a == "--spike-remove-occurrence") {
        let token = api::load_or_refresh_token()
            .await
            .map_err(std::io::Error::other)?;
        let client = api::client_from_token(token).await;
        if let Err(e) = spike::run_spike_remove_specific_occurrence(&client).await {
            eprintln!("spike failed: {e}");
        }
        return Ok(());
    }

    install_panic_hook();
    // env_logger defaults to stderr -- wrong assumption made earlier that
    // this "doesn't collide with the TUI since it only writes to stdout":
    // a real terminal interleaves both streams onto the same screen
    // regardless of which fd wrote what, confirmed live (raw log lines
    // spilling across the TUI when run as a bare command, no redirect).
    // Logging to a file instead avoids needing the user to know to
    // redirect stderr manually -- a real command should just work.
    let log_path = log_file_path();
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(log_file) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
        // "librespot=debug" alone (the original filter) sets everything
        // NOT explicitly named to "off" -- every log::info!/warn! call in
        // this crate's own code (main.rs, api/*.rs) has been silently
        // dropped this whole session as a result, independent of whatever
        // it was trying to report. "info" as the global default keeps
        // librespot's own verbosity at debug while actually letting the
        // app's own logging through.
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,librespot=debug"))
            .target(env_logger::Target::Pipe(Box::new(log_file)))
            .init();
    }

    // -- Web API client for search (Tier 1): bootstrapped eagerly so it's
    // ready by the time the user presses `/`, not fetched on first use.
    // A failure here (e.g. no refresh_token cached, or the shared
    // ncspot client_id rate-limited) disables search only -- playback
    // and lyrics don't depend on this at all.
    let (client_tx, client_rx) = mpsc::channel::<Option<AuthCodeSpotify>>();
    tokio::spawn(async move {
        match api::load_or_refresh_token().await {
            Ok(token) => {
                let _ = client_tx.send(Some(api::client_from_token(token).await));
            }
            Err(e) => {
                log::warn!("search unavailable: failed to load/refresh Spotify token: {e}");
                let _ = client_tx.send(None);
            }
        }
    });
    let mut spotify_client: Option<AuthCodeSpotify> = None;
    let mut client_checked = false;
    let (search_tx, search_rx) = mpsc::channel::<Result<Vec<TrackResult>, String>>();
    let (library_tx, library_rx) = mpsc::channel::<LibraryFetchResult>();
    let (crud_tx, crud_rx) = mpsc::channel::<CrudResult>();

    // -- terminal UI setup (same pattern as ncspot-lyrics) --
    let _guard = TerminalGuard::new()?;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(stdout()))?;

    // Query the terminal's real graphics-protocol capability exactly
    // once, here -- `Picker::from_query_stdio`'s own doc comment
    // requires this to run after entering the alternate screen (already
    // done above) but strictly before the main loop starts reading
    // terminal events below, so its own momentary stdio read never
    // races `event::read()`.
    let mut picker = ratatui_image::picker::Picker::from_query_stdio().ok();
    if let Some(p) = &mut picker {
        // Confirmed live: this crate version (11.0.8) has no Ghostty
        // handling at all (checked its source directly -- only
        // WezTerm/Konsole get an env-var check, both for *blacklisting*
        // a protocol, not detecting one) even though Ghostty fully
        // implements the kitty graphics protocol (ghostty.org/docs/
        // features). Its stdio capability probe still resolved to
        // `Halfblocks` here regardless -- most likely the probe's
        // "stop reading once the trailing device-status-report arrives"
        // heuristic races Ghostty's actual response order, dropping the
        // kitty-specific reply before it's parsed. Overriding via
        // `TERM`/`TERM_PROGRAM` mirrors exactly the pattern this crate
        // already uses for WezTerm/Konsole -- extending it to a
        // terminal this version doesn't special-case yet, not a hack
        // invented from nothing.
        if p.protocol_type() == ratatui_image::picker::ProtocolType::Halfblocks && is_ghostty() {
            log::info!("graphics protocol probe said Halfblocks but the real terminal is Ghostty -- overriding to Kitty");
            p.set_protocol_type(ratatui_image::picker::ProtocolType::Kitty);
        }
        log::info!("graphics protocol in use: {:?}", p.protocol_type());

        // The same stdio probe that missed Ghostty's kitty-graphics
        // reply also populates `font_size` -- and got that wrong here
        // too, confirmed live: this app's own art-sizing math worked
        // around it (computing cell counts from a separately-queried
        // real cell size), but `ratatui-image`'s own internal image
        // encoder *also* reads `Picker`'s stored `font_size` to decide
        // how many real pixels to transmit per cell -- so the two ended
        // up disagreeing with each other, each internally consistent
        // but not with the terminal's true cell size, and the
        // transmitted image came out lower-resolution than the cells it
        // was stretched across, rendering visibly pixelated. Correcting
        // the stored value itself, once, here, fixes both from one
        // source of truth instead of two separately-computed ones.
        // `Picker` has no public setter for `font_size` --
        // `from_fontsize` (deprecated upstream in favor of
        // `from_query_stdio`/`halfblocks`, neither of which allows
        // injecting a known-correct value) is still the only way to
        // build one with an explicit size, so this is an intentional,
        // justified use of a deprecated API, not an oversight.
        if let Ok(win) = crossterm::terminal::window_size()
            && win.width > 0 && win.height > 0 && win.columns > 0 && win.rows > 0 {
                let (real_w, real_h) = (win.width / win.columns, win.height / win.rows);
                if real_w > 0 && real_h > 0 {
                    let protocol = p.protocol_type();
                    #[allow(deprecated)]
                    let mut corrected =
                        ratatui_image::picker::Picker::from_fontsize(ratatui_image::FontSize::new(real_w, real_h));
                    corrected.set_protocol_type(protocol);
                    log::info!("corrected picker font size to {real_w}x{real_h}px/cell via window_size ioctl");
                    *p = corrected;
                }
            }
    } else {
        log::warn!("graphics protocol detection failed outright, falling back to text/placeholder art");
    }

    let (fetch_tx, fetch_rx, fetch_res_tx) = spawn_fetch_thread();
    // Real album art: fetched off the render path (spawn_blocking, since
    // it's a plain synchronous `ureq` GET + JPEG decode) and reported
    // back through this channel, generation-tagged the same way lyrics
    // fetches already are so a result for a track already skipped past
    // gets dropped rather than painted over the current one.
    let (cover_tx, cover_rx) = mpsc::channel::<(u64, image::DynamicImage)>();
    let cfg = config::load();
    // Spicy Lyrics' developer API: only when a key is configured. Without
    // one the lyrics chain is exactly what it was before this existed.
    let spicy = cfg.spicy_lyrics_key().map(spicy::SpicyClient::new);
    log::info!(
        "spicy_lyrics: {}",
        if spicy.is_some() { "key configured, used first" } else { "no key configured, skipped" }
    );
    let mut tracker: PositionTracker;
    let mut app = AppState {
        track_title: None,
        track_artist: None,
        track_album: None,
        current_track_uri: None,
        context_label: None,
        shuffle: false,
        smart_shuffle: false,
        repeat: RepeatMode::Off,
        lyrics: LyricsState::Idle,
        lyrics_credit: None,
        current_line: None,
        fullscreen: false,
        playing: None,
        position: Duration::ZERO,
        duration: Duration::ZERO,
        volume: u16::MAX, // matches the initial_volume set on connect_config above
        nav: Nav::new(),
        sidebar_sel: 0,
        library: LibraryState::new(),
        queue: ui::QueueState::new(),
        devices: ui::DevicesState::new(),
        playlist_detail: None,
        artist_detail: None,
        album_detail: None,
        pinned_playlists: pins::load("playlists"),
        pinned_tracks: pins::load("tracks"),
        playlist_membership: std::collections::HashMap::new(),
        search: SearchState::new(),
        pending_confirm: None,
        text_prompt: None,
        playlist_picker: None,
        quick_jump: None,
        status: None,
    };

    // Scroll offsets, one per list, persisted across frames and threaded
    // separately from `app` -- see `ui::ScrollState`'s own doc comment
    // for why this isn't just more fields on `AppState`.
    let mut scroll = ui::ScrollState::default();
    let mut images = ui::ImageState {
        picker,
        cover_image: None,
        sized_covers: Vec::new(),
        startup_retransmit_at: None,
        startup_retransmit_done: false,
    };

    let mut generation: u64 = 0;
    let mut pending_fetch: Option<(u64, TrackMeta, Instant)> = None;
    let mut synced_lines: Vec<LyricLine> = Vec::new();
    // Unlike every other fetched list, the queue reflects live playback
    // state that changes on its own (the current track finishes, another
    // device skips ahead) even when this app hasn't done anything -- so
    // it's refetched periodically while visible rather than once and
    // cached, gated by this timer rather than a one-shot NotStarted check.
    let mut queue_last_fetched: Option<Instant> = None;
    const QUEUE_POLL_INTERVAL: Duration = Duration::from_secs(5);

    // Tier 4 resilience: reconnect with capped exponential backoff instead
    // of leaving the app permanently dead after a session drop. Mirrors
    // the same backoff pattern ncspot-lyrics's socket reader already
    // proved out.
    let mut backoff = Duration::from_millis(500);
    const MAX_BACKOFF: Duration = Duration::from_secs(10);

    // Only the very first connect of this process's lifetime gets the
    // startup animation + minimum-visible-duration treatment below --
    // reconnects after a later drop should stay as fast/invisible as
    // they already are, not get padded every time.
    let mut first_connect = true;
    // Separate from `first_connect`/`STARTUP_MIN_VISIBLE` (which only
    // covers "is Spirc connected") -- see the real wait-loop below this
    // guards, right after the very first session's `spirc.transfer`.
    let mut shown_first_track = false;
    // Long enough to give Ghostty's kitty-graphics subsystem a real
    // window to finish initializing before the first real frame (with
    // real album art) ever gets drawn -- see `ui::render_startup`'s own
    // doc comment for the live bug this covers. Short enough that a
    // normal-speed connect (which already takes several real seconds
    // per the log) rarely even notices this floor; it only matters on
    // an unusually fast/cached reconnect that would otherwise skip the
    // warm-up window entirely.
    const STARTUP_MIN_VISIBLE: Duration = Duration::from_millis(600);
    const STARTUP_TICK: Duration = Duration::from_millis(80);
    // How long with no *further* TrackChanged before the first-ever
    // track is considered settled -- same debounce idea `DEBOUNCE`
    // already uses for lyrics fetches on rapid track changes, applied
    // here to what actually gets shown. librespot-connect's own
    // resume/autoplay-context handshake can emit more than one
    // TrackChanged in quick succession right after connect (confirmed
    // live from the log: a resumed track, then a failed Autoplay
    // context-resolve, then a retry that lands on the real one) --
    // waiting for events to stop arriving, not just for one to arrive,
    // is what actually skips past the intermediate track instead of
    // briefly showing it.
    const STARTUP_TRACK_SETTLE: Duration = Duration::from_millis(400);
    // Escape hatch for the real case where nothing ever settles (no
    // resumed session, nothing playing at all) -- without this, that
    // case would hang the splash forever waiting for a TrackChanged
    // that's never coming.
    const STARTUP_TRACK_MAX_WAIT: Duration = Duration::from_secs(4);

    'outer: loop {
        let connect_result = if first_connect {
            let start = Instant::now();
            let mut handle = tokio::spawn(connect_spirc());
            let mut tick: usize = 0;
            let result = loop {
                tokio::select! {
                    res = &mut handle => break res.expect("connect_spirc task panicked"),
                    _ = tokio::time::sleep(STARTUP_TICK) => {
                        tick = tick.wrapping_add(1);
                        terminal.draw(|f| ui::render_startup(f, tick))?;
                    }
                }
            };
            while start.elapsed() < STARTUP_MIN_VISIBLE {
                tick = tick.wrapping_add(1);
                terminal.draw(|f| ui::render_startup(f, tick))?;
                tokio::time::sleep(STARTUP_TICK).await;
            }
            first_connect = false;
            result
        } else {
            connect_spirc().await
        };
        let (spirc, spirc_handle, mut player_events, lyrics_session) = match connect_result {
            Ok(v) => {
                backoff = Duration::from_millis(500);
                v
            }
            Err(e) => {
                log::warn!("connect failed, retrying in {backoff:?}: {e}");
                app.track_title = None;
                app.track_artist = None;
                app.track_album = None;
                app.playing = None;
                app.lyrics = LyricsState::SessionEnded;
                terminal.draw(|f| ui::render(f, &app, &mut scroll, &mut images))?;
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue 'outer;
            }
        };

        // Reclaim whatever was last active, matching the official app's
        // "continue where you left off" on launch -- Spirc::transfer with
        // the same device on both ends of the underlying spclient call
        // (see spike.rs's finding) is a no-op if we're already active,
        // so this is safe to call on every (re)connect, not just the
        // first. Untested until run live: this is the first time this
        // codebase has called Spirc::transfer at all, as opposed to
        // Spirc::activate (which explicitly does NOT resume playback).
        match spirc.transfer(None) {
            Ok(()) => log::info!("spirc.transfer(None) sent -- attempting to reclaim last active session"),
            Err(e) => log::warn!("spirc.transfer(None) failed: {e}"),
        }

        // Fresh session: don't keep showing a frozen position/track from
        // whatever the last one was.
        app.lyrics = LyricsState::Idle;
        tracker = PositionTracker::new();

        // Real bug reported live: on boot, Now Playing briefly shows
        // "nothing playing yet," then flashes whatever track
        // librespot-connect's own resume/autoplay-context handshake
        // first lands on, before swapping to the actual settled track.
        // The startup splash above already exited by this point -- its
        // own "done" signal is just "Spirc connected," not "the first
        // real track is known" -- and there's a real, separate gap after
        // that: `spirc.transfer` above resumes the last session, and
        // librespot-connect then does its own async context-resolution
        // (confirmed live from the log: a resumed track, then a failed
        // Autoplay context-resolve, then a retry that lands on the real
        // one -- two real `TrackChanged` events, a couple of seconds
        // apart, both firing before either the idle screen or the main
        // loop would otherwise have started drawing). This bridges that
        // gap: keeps the splash up, applying every event exactly as the
        // main loop below would, until no further `TrackChanged` has
        // arrived for `STARTUP_TRACK_SETTLE` straight (the intermediate
        // track's own cover/lyrics fetches still fire and get
        // superseded harmlessly, same as any rapid track change already
        // handles). Runs at most once, ever, for this process -- not on
        // a later reconnect, matching `first_connect`'s own scoping.
        if !shown_first_track {
            shown_first_track = true;
            let wait_start = Instant::now();
            let mut last_track_change: Option<Instant> = None;
            let mut tick: usize = 0;
            loop {
                if spirc_handle.is_finished() {
                    break;
                }
                if drain_player_events(
                    &mut app,
                    &images,
                    &mut tracker,
                    &mut generation,
                    &cover_tx,
                    &mut pending_fetch,
                    &mut player_events,
                ) {
                    last_track_change = Some(Instant::now());
                }
                let settled = last_track_change.is_some_and(|t| t.elapsed() >= STARTUP_TRACK_SETTLE);
                if settled || wait_start.elapsed() >= STARTUP_TRACK_MAX_WAIT {
                    break;
                }
                tick = tick.wrapping_add(1);
                terminal.draw(|f| ui::render_startup(f, tick))?;
                tokio::time::sleep(STARTUP_TICK).await;
            }
        }

    let exit: LoopExit = 'inner: loop {
        if spirc_handle.is_finished() {
            log::warn!("Spirc task ended -- Connect session dropped, reconnecting");
            break 'inner LoopExit::Disconnected;
        }

        let _ = drain_player_events(
            &mut app,
            &images,
            &mut tracker,
            &mut generation,
            &cover_tx,
            &mut pending_fetch,
            &mut player_events,
        );

        if let Some((fetch_gen, meta, deadline)) = pending_fetch.clone()
            && Instant::now() >= deadline {
                pending_fetch = None;
                // Tried in order, each `None` meaning "no usable synced result
                // from this source" (not "no lyrics at all") and falling
                // through to the next: Spicy Lyrics' official developer API
                // (Phase 26; only when a key is configured; word-level and
                // Apple Music/community syncs, credited under the lyrics),
                // then Spotify's own first-party catalog (Phase 17,
                // session.spclient().get_lyrics), then YouTube Music
                // (Phase 22, a different licensing catalog), then lrclib via
                // the background thread below as the community-database
                // tier. The earlier reverse-engineered Spicy Lyrics attempt
                // (Phase 20) is gone for good; this is the sanctioned API.
                // A track_id that fails to parse (shouldn't happen for a
                // real spotify:track: uri, but this is exactly the kind of
                // external-shape assumption this codebase never trusts
                // blindly) skips straight to the lrclib fallback.
                let track_id = librespot_core::SpotifyUri::from_uri(&meta.track_id)
                    .ok()
                    .and_then(|uri| librespot_core::SpotifyId::try_from(&uri).ok());
                match track_id {
                    Some(track_id) => {
                        let session = lyrics_session.clone();
                        let fallback_tx = fetch_tx.clone();
                        let result_tx = fetch_res_tx.clone();
                        let spicy = spicy.clone();
                        tokio::spawn(async move {
                            if let (Some(client), Ok(base62)) = (spicy, track_id.to_base62())
                                && let Some(cached) = spicy_lookup(&client, &meta.track_id, &base62).await
                            {
                                let _ = result_tx.send((fetch_gen, cached));
                                return;
                            }
                            if let Some(cached) = spotify_lyrics(&session, track_id).await {
                                let _ = result_tx.send((fetch_gen, cached));
                                return;
                            }
                            let duration_secs = meta.duration_ms as f64 / 1000.0;
                            if let Some(cached) =
                                ytmusic::ytmusic_lyrics(&meta.artist, &meta.title, duration_secs).await
                            {
                                let _ = result_tx.send((fetch_gen, cached));
                                return;
                            }
                            let _ = fallback_tx.send((fetch_gen, meta));
                        });
                    }
                    None => {
                        let _ = fetch_tx.send((fetch_gen, meta));
                    }
                }
            }

        if !client_checked
            && let Ok(result) = client_rx.try_recv() {
                app.search.client_ready = result.is_some();
                spotify_client = result;
                client_checked = true;

                // Fetch playlists as soon as the client's ready, not
                // lazily on first Library visit -- the Sidebar shows
                // them directly (see ui::sidebar_rows) and shouldn't sit
                // empty until the user happens to drill into Library
                // first. Guarded the same way the lazy trigger elsewhere
                // is, so whichever fires first wins and the other is a
                // harmless no-op.
                if let (Some(client), true) =
                    (spotify_client.clone(), matches!(app.library.playlists, Fetch::NotStarted))
                {
                    app.library.playlists = Fetch::Loading;
                    let tx = library_tx.clone();
                    tokio::spawn(async move {
                        let result = api::library::your_playlists(&client).await.map_err(|e| e.to_string());
                        let _ = tx.send(LibraryFetchResult::Playlists(result));
                    });
                }
                // Same reasoning, now that Liked Songs is also a direct
                // Sidebar entry: fetch eagerly rather than leaving it
                // stuck on "loading..." forever, since the only other
                // trigger is Library home's own Enter handler, which the
                // Sidebar's direct entry bypasses entirely.
                if let (Some(client), true) =
                    (spotify_client.clone(), matches!(app.library.liked_songs, Fetch::NotStarted))
                {
                    app.library.liked_songs = Fetch::Loading;
                    let tx = library_tx.clone();
                    tokio::spawn(async move {
                        let result = api::library::liked_songs(&client).await.map_err(|e| e.to_string());
                        let _ = tx.send(LibraryFetchResult::LikedSongs(result));
                    });
                }
            }

        if *app.nav.top() == Screen::Queue {
            let due = queue_last_fetched.is_none_or(|t| t.elapsed() >= QUEUE_POLL_INTERVAL);
            if due
                && let Some(client) = spotify_client.clone() {
                    queue_last_fetched = Some(Instant::now());
                    let tx = library_tx.clone();
                    tokio::spawn(async move {
                        let result = api::queue::current_queue(&client).await;
                        let _ = tx.send(LibraryFetchResult::Queue(result));
                    });
                }
        } else {
            // Leaving the screen resets the timer so returning to it
            // later fetches immediately instead of waiting out whatever
            // was left of the previous interval.
            queue_last_fetched = None;
        }

        while let Ok(result) = search_rx.try_recv() {
            app.search.searching = false;
            match result {
                Ok(results) => {
                    app.search.results = results;
                    app.search.selected = 0;
                    app.search.error = None;
                }
                Err(e) => {
                    app.search.error = Some(e);
                }
            }
        }

        while let Ok(result) = library_rx.try_recv() {
            match result {
                LibraryFetchResult::LikedSongs(r) => {
                    app.library.liked_songs = r.map_or_else(Fetch::Failed, Fetch::Ready);
                }
                LibraryFetchResult::SavedAlbums(r) => {
                    app.library.saved_albums = r.map_or_else(Fetch::Failed, Fetch::Ready);
                }
                LibraryFetchResult::FollowedArtists(r) => {
                    app.library.followed_artists = r.map_or_else(Fetch::Failed, Fetch::Ready);
                }
                LibraryFetchResult::Playlists(r) => {
                    app.library.playlists = r.map_or_else(Fetch::Failed, Fetch::Ready);
                }
                LibraryFetchResult::PlaylistTracks { playlist_uri, result } => {
                    // Populated regardless of whether this playlist is
                    // the one currently open below -- the staleness
                    // guard below protects the *view* from a stale
                    // fetch, but the data itself is still correct for
                    // the URI it was fetched under. Every full track-
                    // list fetch, for any reason, is a free chance to
                    // populate the add-to-playlist picker's membership
                    // cache (see `AppState::playlist_membership`'s own
                    // doc comment).
                    if let Ok(tracks) = &result {
                        app.playlist_membership
                            .insert(playlist_uri.clone(), tracks.iter().map(|t| t.uri.clone()).collect());
                    }
                    // Guard against a stale fetch for a playlist the user
                    // has since backed out of overwriting whichever one
                    // is actually showing now.
                    if let Some(pd) = &mut app.playlist_detail
                        && pd.playlist.uri == playlist_uri {
                            pd.tracks = result.map_or_else(Fetch::Failed, Fetch::Ready);
                        }
                }
                LibraryFetchResult::Queue(result) => {
                    app.queue.fetch = result.map_or_else(Fetch::Failed, Fetch::Ready);
                }
                LibraryFetchResult::Devices(result) => {
                    app.devices.fetch = result.map_or_else(Fetch::Failed, Fetch::Ready);
                }
                LibraryFetchResult::ArtistDetail { artist_uri, result } => {
                    // Same staleness guard as PlaylistTracks -- discards a
                    // fetch for an artist the user has since backed out of.
                    if let Some(state) = &mut app.artist_detail
                        && state.artist_uri == artist_uri {
                            state.detail = result.map_or_else(Fetch::Failed, Fetch::Ready);
                        }
                }
                LibraryFetchResult::AlbumDetail { album_uri, result } => {
                    if let Some(state) = &mut app.album_detail
                        && state.album_uri == album_uri {
                            state.detail = result.map_or_else(Fetch::Failed, Fetch::Ready);
                        }
                }
            }
        }

        while let Ok(result) = crud_rx.try_recv() {
            match result {
                CrudResult::PlaylistCreated(Ok(summary)) => {
                    app.status = Some(("playlist created".to_string(), false));
                    // A just-created playlist provably contains nothing --
                    // free and correct, no fetch needed to know it.
                    app.playlist_membership.insert(summary.uri, std::collections::HashSet::new());
                    refetch_playlists(&mut app, &library_tx, &spotify_client);
                }
                CrudResult::PlaylistCreated(Err(e)) => {
                    app.status = Some((format!("create playlist failed: {e}"), true));
                }
                CrudResult::PlaylistRenamed { playlist_uri, new_name, result: Ok(()) } => {
                    app.status = Some(("playlist renamed".to_string(), false));
                    // Your Playlists/Sidebar both read `library.playlists`,
                    // refetched below -- but Playlist Detail's header reads
                    // its own separate `pd.playlist.name` copy, which a
                    // refetch of the *list* never touches. Patch it
                    // directly so an already-open detail view doesn't keep
                    // showing the old name until backed out and reopened.
                    if let Some(pd) = &mut app.playlist_detail
                        && pd.playlist.uri == playlist_uri {
                            pd.playlist.name = new_name;
                        }
                    refetch_playlists(&mut app, &library_tx, &spotify_client);
                }
                CrudResult::PlaylistRenamed { result: Err(e), .. } => {
                    app.status = Some((format!("rename failed: {e}"), true));
                }
                CrudResult::PlaylistDeleted { playlist_uri, result: Ok(()) } => {
                    app.status = Some(("playlist deleted".to_string(), false));
                    app.playlist_membership.remove(&playlist_uri);
                    // The detail screen for a playlist that no longer
                    // exists has nothing left to show -- back out to Your
                    // Playlists rather than leave stale tracks on screen.
                    if app.playlist_detail.as_ref().is_some_and(|pd| pd.playlist.uri == playlist_uri) {
                        app.playlist_detail = None;
                        app.nav.goto(Screen::YourPlaylists);
                    }
                    refetch_playlists(&mut app, &library_tx, &spotify_client);
                }
                CrudResult::PlaylistDeleted { result: Err(e), .. } => {
                    app.status = Some((format!("delete failed: {e}"), true));
                }
                CrudResult::TrackAdded { playlist_uri, track_uri, result: Ok(()) } => {
                    app.status = Some(("added to playlist".to_string(), false));
                    // Local, immediate -- see bump_track_count's own doc
                    // comment for why a refetch (tried first) isn't reliable
                    // here. Before consuming playlist_uri below.
                    bump_track_count(&mut app, &playlist_uri, 1);
                    // Same optimistic-write-through reasoning, applied to
                    // the picker's membership cache: without this, adding
                    // a track and immediately reopening the picker on it
                    // would show no checkmark for the playlist just added
                    // to, until something else happened to refetch it.
                    app.playlist_membership.entry(playlist_uri.clone()).or_default().insert(track_uri);
                    // Same staleness guard as TrackRemoved -- rare (the
                    // add-to-playlist target is usually a *different*
                    // playlist than whichever one's open), but if they
                    // happen to coincide, its track list shouldn't sit
                    // stale until backed out and reopened.
                    if app.playlist_detail.as_ref().is_some_and(|pd| pd.playlist.uri == playlist_uri) {
                        refetch_playlist_tracks(&mut app, &library_tx, &spotify_client, playlist_uri);
                    }
                }
                CrudResult::TrackAdded { result: Err(e), .. } => {
                    app.status = Some((format!("add to playlist failed: {e}"), true));
                }
                CrudResult::PlaylistAlreadyHasTrack { playlist_uri, track_uri, message } => {
                    app.pending_confirm =
                        Some(PendingConfirm { message, action: ConfirmAction::AddTrackAnyway { playlist_uri, track_uri } });
                }
                CrudResult::TrackRemoved { playlist_uri, track_uri, occurrences, result: Ok(()) } => {
                    app.status = Some(("removed from playlist".to_string(), false));
                    // remove_track deletes every occurrence in one call
                    // (see api::playlists::remove_track's own doc comment
                    // and spike::run_spike_remove_specific_occurrence --
                    // there's no reliable position-scoped alternative), so
                    // the count drops by however many copies existed, not
                    // always 1.
                    bump_track_count(&mut app, &playlist_uri, -(occurrences as i64));
                    if let Some(set) = app.playlist_membership.get_mut(&playlist_uri) {
                        set.remove(&track_uri);
                    }
                    if app.playlist_detail.as_ref().is_some_and(|pd| pd.playlist.uri == playlist_uri) {
                        refetch_playlist_tracks(&mut app, &library_tx, &spotify_client, playlist_uri);
                    }
                }
                CrudResult::TrackRemoved { result: Err(e), .. } => {
                    app.status = Some((format!("remove track failed: {e}"), true));
                }
                CrudResult::TrackReordered { playlist_uri, result: Ok(()) } => {
                    app.status = Some(("reordered".to_string(), false));
                    // Refetches to reconcile with the server's own
                    // snapshot_id even though the local Vec was already
                    // optimistically reordered during move-mode -- cheap
                    // insurance against drift, matching every other
                    // mutation's refetch-on-success convention.
                    if app.playlist_detail.as_ref().is_some_and(|pd| pd.playlist.uri == playlist_uri) {
                        refetch_playlist_tracks(&mut app, &library_tx, &spotify_client, playlist_uri);
                    }
                }
                CrudResult::TrackReordered { playlist_uri, result: Err(e) } => {
                    app.status = Some((format!("reorder failed: {e}"), true));
                    // Unlike every other mutation here, the local list was
                    // already optimistically reordered *before* this
                    // result arrived (that's the whole point of move-mode
                    // not hitting the network per keystroke) -- on
                    // failure that local order is now wrong and has to be
                    // refetched away, not just left showing a move that
                    // never actually happened server-side.
                    if app.playlist_detail.as_ref().is_some_and(|pd| pd.playlist.uri == playlist_uri) {
                        refetch_playlist_tracks(&mut app, &library_tx, &spotify_client, playlist_uri);
                    }
                }
                CrudResult::DeviceTransferred(Ok(())) => {
                    app.status = Some(("playback transferred".to_string(), false));
                    // Refetch so the active-device marker moves to the
                    // one just transferred to, rather than sitting stale
                    // until the screen is manually left and reopened.
                    refetch_devices(&mut app, &library_tx, &spotify_client);
                }
                CrudResult::DeviceTransferred(Err(e)) => {
                    app.status = Some((format!("transfer failed: {e}"), true));
                }
                CrudResult::QueueAdded { result: Ok(()), track_uri } => {
                    log::info!("add_to_queue[{track_uri}]: ok");
                    app.status = Some(("added to queue".to_string(), false));
                }
                CrudResult::QueueAdded { result: Err(e), track_uri } => {
                    log::warn!("add_to_queue[{track_uri}]: failed: {e}");
                    app.status = Some((format!("couldn't add to queue: {e}"), true));
                }
                CrudResult::LikeToggled { result: Ok(()), liked, track_uri } => {
                    log::info!("like_track[{track_uri}]: liked={liked}");
                    app.status = Some((if liked { "liked".to_string() } else { "unliked".to_string() }, false));
                    refetch_liked_songs(&mut app, &library_tx, &spotify_client);
                }
                CrudResult::LikeToggled { result: Err(e), liked, track_uri } => {
                    let verb = if liked { "like" } else { "unlike" };
                    log::warn!("like_track[{track_uri}]: {verb} failed: {e}");
                    app.status = Some((format!("{verb} failed: {e}"), true));
                }
                CrudResult::FollowToggled { result: Ok(()), followed, artist_uri } => {
                    log::info!("follow_artist[{artist_uri}]: followed={followed}");
                    app.status =
                        Some((if followed { "followed".to_string() } else { "unfollowed".to_string() }, false));
                    refetch_followed_artists(&mut app, &library_tx, &spotify_client);
                }
                CrudResult::FollowToggled { result: Err(e), followed, artist_uri } => {
                    let verb = if followed { "follow" } else { "unfollow" };
                    log::warn!("follow_artist[{artist_uri}]: {verb} failed: {e}");
                    app.status = Some((format!("{verb} failed: {e}"), true));
                }
                CrudResult::SaveToggled { result: Ok(()), saved, album_uri } => {
                    log::info!("save_album[{album_uri}]: saved={saved}");
                    app.status = Some((if saved { "saved".to_string() } else { "unsaved".to_string() }, false));
                    refetch_saved_albums(&mut app, &library_tx, &spotify_client);
                }
                CrudResult::SaveToggled { result: Err(e), saved, album_uri } => {
                    let verb = if saved { "save" } else { "unsave" };
                    log::warn!("save_album[{album_uri}]: {verb} failed: {e}");
                    app.status = Some((format!("{verb} failed: {e}"), true));
                }
            }
        }

        while let Ok((fetch_gen, result)) = fetch_rx.try_recv() {
            if fetch_gen == generation {
                app.lyrics_credit = match &result {
                    CachedLyrics::Synced { credit, .. } => credit.clone(),
                    _ => None,
                };
                app.lyrics = to_lyrics_state(result);
                if let LyricsState::Synced(lines) = &app.lyrics {
                    synced_lines = lines.clone();
                }
            }
        }

        while let Ok((cover_gen, dyn_image)) = cover_rx.try_recv() {
            // Same staleness guard as lyrics: a cover for a track already
            // skipped past gets dropped instead of painted over whatever
            // is playing now.
            if cover_gen == generation
                && let Some(uri) = &app.current_track_uri {
                    // Only the *decoded* image is stored here -- building
                    // the actual protocol-encoded `StatefulProtocol` per
                    // render size happens lazily in `ui::render_art`,
                    // which is what lets a stable set of sizes (compact,
                    // fullscreen) be cached instead of re-encoded and
                    // re-transmitted on every `f` toggle (see
                    // `ImageState`'s own doc comment).
                    images.cover_image = Some((uri.clone(), dyn_image));
                    images.sized_covers.clear();
                }
        }

        let now = Instant::now();
        app.position = Duration::from_millis(tracker.progress_ms(now) as u64);
        app.playing = if tracker.current_track_id().is_some() {
            Some(tracker.is_playing())
        } else {
            None
        };
        app.current_line = if matches!(app.lyrics, LyricsState::Synced(_)) {
            current_line_index(&synced_lines, app.position)
        } else {
            None
        };

        terminal.draw(|f| ui::render(f, &app, &mut scroll, &mut images))?;

        let tick = if ui::word_sweep_active(&app.lyrics, app.current_line, app.playing) { WORD_TICK } else { TICK };
        if event::poll(tick)? {
            let ev = event::read()?;
            // Real bug reported live: switching tmux *windows* away and
            // back drops a previously-placed kitty-graphics image --
            // tmux doesn't track image placements as part of its own
            // redrawable screen state (a real, documented tmux/kitty
            // limitation), so redrawing this pane on switch-back leaves
            // the image slot blank with no signal to this app that
            // anything needs to happen. `focus-events` (already on via
            // tmux-sensible, confirmed live) makes tmux emit a real
            // focus-gained sequence on exactly this transition, and
            // crossterm already parses it natively. Clearing the sized-
            // cover cache forces the very next render to rebuild and
            // retransmit fresh -- the same recovery a manual track
            // skip-then-back already provides, just triggered by the
            // actual event that causes the problem instead of a timer.
            if ev == Event::FocusGained {
                images.sized_covers.clear();
            }
            if let Event::Key(key) = ev {
                // A mutation's result message (success or failure) shows
                // until the next keypress, same lifetime a status line
                // conventionally gets.
                app.status = None;

                // Phase 5's transient overlays (name prompt, yes/no
                // confirm, playlist picker) fully own the keypress when
                // active -- checked before even `Tab`, so none of them
                // leak through to the screen underneath. See the doc
                // comment above `ui::TextPrompt` for why these are
                // sibling `Option`s here rather than `Screen` variants.
                if app.pending_confirm.is_some() {
                    if handle_confirm_key(&mut app, key.code, &crud_tx, &spotify_client) {
                        break 'inner LoopExit::Quit;
                    }
                } else if app.text_prompt.is_some() {
                    handle_text_prompt_key(&mut app, key.code, &crud_tx, &spotify_client);
                } else if app.playlist_picker.is_some() {
                    handle_picker_key(&mut app, key.code, &crud_tx, &library_tx, &spotify_client);
                // Phase 12's quick-jump palette -- same tier as the three
                // overlays above (it's a fourth sibling, not a `Screen`),
                // so it also owns the keypress outright while open,
                // including its own toggle-close on a repeat `Ctrl+P`.
                } else if app.quick_jump.is_some() {
                    handle_quick_jump_key(
                        &mut app,
                        key.code,
                        key.modifiers,
                        &spirc,
                        &crud_tx,
                        &library_tx,
                        &spotify_client,
                    );
                // Tab is a distinct KeyCode, never a `Char(_)` -- safe to
                // intercept before anything else without ever eating a
                // literal keystroke a text field might want.
                } else if key.code == KeyCode::Tab {
                    app.nav.toggle_focus();
                // Quick jump's own open trigger -- checked before `q` and
                // Sidebar/Main so it works globally, including mid-query
                // on Search (Ctrl+P is a distinct KeyEvent from a plain
                // 'p', so it never reaches Search's own char-insertion
                // arm). Eagerly kicks off a fetch for any of the three
                // lazy-loaded categories that haven't been visited yet
                // this session, mirroring the Devices sidebar entry's own
                // `if NotStarted { refetch }` precedent, so they show up
                // in results without needing to visit those screens first.
                } else if is_quick_jump_trigger(key.code, key.modifiers) {
                    if matches!(app.library.saved_albums, Fetch::NotStarted) {
                        refetch_saved_albums(&mut app, &library_tx, &spotify_client);
                    }
                    if matches!(app.library.followed_artists, Fetch::NotStarted) {
                        refetch_followed_artists(&mut app, &library_tx, &spotify_client);
                    }
                    if matches!(app.devices.fetch, Fetch::NotStarted) {
                        refetch_devices(&mut app, &library_tx, &spotify_client);
                    }
                    app.quick_jump = Some(QuickJump { filter: ListFilter::default(), selected: 0 });
                // Shuffle and repeat are global, so they live here in the one
                // top-level chain instead of being copied into every screen's
                // own arm the way space/n/p/+/- were (about twenty copies).
                // The guard is what makes that safe: while a text field owns
                // the keyboard, `z` is just a letter. Ctrl/Alt are excluded
                // so Ctrl+Z (or an Alt+Z chord) isn't mistaken for it.
                } else if key.code == KeyCode::Char('z')
                    && !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !text_input_active(&app)
                {
                    cycle_shuffle(&mut app, &spirc);
                // `Shift+R` via `is_shift_char`, like every other Shift+letter
                // here (some terminals report it as lowercase-plus-SHIFT). Plain
                // `r` is untouched -- it still means rename/refresh per screen.
                } else if is_shift_char(key.code, key.modifiers, 'R', 'r') && !text_input_active(&app) {
                    cycle_repeat(&mut app, &spirc);
                // `q` asks first, same as every other destructive action,
                // when enabled (default on; `confirm_quit = false` in
                // config.toml restores the old immediate-quit behavior).
                // Excludes Search specifically -- `q` isn't a quit key
                // there at all, it's a literal character the query box
                // needs, same standing exception as every other letter.
                } else if key.code == KeyCode::Char('q')
                    && cfg.confirm_quit
                    && !(app.nav.focus == Focus::Main && *app.nav.top() == Screen::Search)
                {
                    app.pending_confirm = Some(PendingConfirm {
                        message: "Quit spot-tui? y/n".to_string(),
                        action: ConfirmAction::Quit,
                    });
                } else if app.nav.focus == Focus::Sidebar {
                    match key.code {
                        KeyCode::Char('q') => break 'inner LoopExit::Quit,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break 'inner LoopExit::Quit
                        }
                        KeyCode::Char('?') => {
                            app.nav.push(Screen::Help);
                            // Every other Sidebar activation (Enter/Right)
                            // hands focus to Main -- this one didn't,
                            // leaving focus on Sidebar while Help showed
                            // in Main. Sidebar-focus has no Esc binding
                            // of its own, so Esc then did nothing at all.
                            app.nav.focus = Focus::Main;
                        }
                        KeyCode::Char('f') => {
                            toggle_or_enter_fullscreen(&mut app);
                            app.nav.focus = Focus::Main;
                        }
                        KeyCode::Char('/') => {
                            app.nav.goto(Screen::Search);
                            app.nav.focus = Focus::Main;
                            app.search.query.clear();
                            app.search.cursor = 0;
                            app.search.results.clear();
                            app.search.error = None;
                        }
                        KeyCode::Char('l') => {
                            app.nav.goto(Screen::Library);
                            app.nav.focus = Focus::Main;
                        }
                        KeyCode::Char(c) if is_pin_key(KeyCode::Char(c), key.modifiers) => {
                            let playlist_uri = match ui::sidebar_rows(&app).get(app.sidebar_sel) {
                                Some(ui::SidebarRow::Playlist(p)) => Some(p.uri.clone()),
                                _ => None,
                            };
                            if let Some(uri) = playlist_uri {
                                pins::toggle_in_place(&mut app.pinned_playlists, &uri);
                                pins::save("playlists", &app.pinned_playlists);
                            }
                        }
                        KeyCode::Char(' ') => {
                            let _ = spirc.play_pause();
                        }
                        KeyCode::Char('n') => {
                            let _ = spirc.next();
                        }
                        KeyCode::Char('p') => {
                            let _ = spirc.prev();
                        }
                        KeyCode::Char('+') => {
                            let _ = spirc.volume_up();
                        }
                        KeyCode::Char('-') => {
                            let _ = spirc.volume_down();
                        }
                        // Seek narrowed to just Now Playing (Main-focus) --
                        // it's the one screen seeking is actually about.
                        // Left/Right here instead mirror yazi/ranger-style
                        // pane navigation: Right activates the selected
                        // Sidebar row (same as Enter), matching "drill
                        // into the right pane." Left has nothing further
                        // left to go to from the Sidebar, so stays unbound.
                        KeyCode::Up => {
                            app.sidebar_sel = app.sidebar_sel.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            let row_count = ui::sidebar_rows(&app).len();
                            app.sidebar_sel = (app.sidebar_sel + 1).min(row_count.saturating_sub(1));
                        }
                        KeyCode::Enter | KeyCode::Right => {
                            let action = match ui::sidebar_rows(&app).get(app.sidebar_sel) {
                                Some(ui::SidebarRow::Menu(_, screen)) => Some(SidebarAction::Goto(*screen)),
                                Some(ui::SidebarRow::Playlist(p)) => {
                                    Some(SidebarAction::OpenPlaylist((*p).clone()))
                                }
                                None => None,
                            };
                            match action {
                                Some(SidebarAction::Goto(screen)) => {
                                    app.nav.goto(screen);
                                    app.nav.focus = Focus::Main;
                                    if screen == Screen::Search {
                                        app.search.query.clear();
                                        app.search.cursor = 0;
                                        app.search.results.clear();
                                        app.search.error = None;
                                    }
                                    if screen == Screen::Devices && matches!(app.devices.fetch, Fetch::NotStarted) {
                                        refetch_devices(&mut app, &library_tx, &spotify_client);
                                    }
                                }
                                Some(SidebarAction::OpenPlaylist(playlist)) => {
                                    open_playlist_detail(&mut app, &library_tx, &spotify_client, playlist);
                                    app.nav.focus = Focus::Main;
                                }
                                None => {}
                            }
                        }
                        _ => {}
                    }
                } else {
                    // focus == Main
                    match *app.nav.top() {
                        Screen::Search => match key.code {
                            // Not Left -- this is a text-input screen; a
                            // directional key needed for moving the
                            // cursor within the query can't also mean
                            // "leave," or editing becomes a minefield.
                            // Reported live: arrow-key cursor movement
                            // didn't work at all here (always had to
                            // backspace-and-retype for a mid-query fix).
                            KeyCode::Esc => {
                                app.nav.escape();
                            }
                            KeyCode::Backspace => {
                                app.search.backspace_at_cursor();
                                app.search.results.clear();
                                app.search.error = None;
                            }
                            KeyCode::Left => {
                                app.search.cursor_left();
                            }
                            // Ctrl+Right/Alt+Right, not plain letters -- every
                            // printable character here has to reach the query box,
                            // and plain Right already means "move the cursor," so
                            // these need keys that can't be typed and aren't already
                            // claimed. Album on Ctrl+Right (matches the `v` = album
                            // convention elsewhere -- the more common action from a
                            // plain track), artist on Alt+Right (matches `v`'s
                            // Shift+V pairing there).
                            KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                if let Some(track) = app.search.results.get(app.search.selected).cloned() {
                                    open_album_detail(&mut app, &library_tx, &spotify_client, track.album_uri);
                                }
                            }
                            KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
                                if let Some(track) = app.search.results.get(app.search.selected).cloned() {
                                    open_artist_detail(&mut app, &library_tx, &spotify_client, track.artist_uri);
                                }
                            }
                            KeyCode::Right => {
                                app.search.cursor_right();
                            }
                            // Ctrl+Up, not Ctrl+L -- Ctrl+L never reached the
                            // app at all (reported live), most likely
                            // because Ctrl+L (ASCII form-feed) is one of the
                            // most commonly terminal/multiplexer-reserved
                            // control codes historically ("clear/redraw"),
                            // unlike Ctrl+Down/Ctrl+Right/Alt+Right, which
                            // are already confirmed working here. Reusing
                            // the same proven Ctrl+arrow category instead
                            // of a fresh Ctrl+letter one, per this app's own
                            // standing fallback plan for exactly this risk.
                            // Always *like* (add) -- Search results aren't a
                            // "you already have this" list the way Liked
                            // Songs is, so there's no unlike direction to
                            // reach from here.
                            KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                if let Some(track) = app.search.results.get(app.search.selected).cloned()
                                    && let Some(client) = spotify_client.clone() {
                                        let track_uri = track.uri;
                                        let tx = crud_tx.clone();
                                        tokio::spawn(async move {
                                            let result = api::library::like_track(&client, &track_uri).await;
                                            let _ = tx.send(CrudResult::LikeToggled { track_uri, liked: true, result });
                                        });
                                    }
                            }
                            KeyCode::Up => {
                                app.search.selected = app.search.selected.saturating_sub(1);
                            }
                            // Ctrl+Down, not plain `a` -- same reasoning as
                            // Ctrl+Right/Alt+Right above. This is the "search to add
                            // to a playlist" flow itself: search normally, land on a
                            // result, add it without ever having to play it or leave
                            // Search first. Reported live as still missing even after
                            // add-to-playlist existed everywhere else a track shows up.
                            KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                if let Some(track) = app.search.results.get(app.search.selected).cloned() {
                                    app.playlist_picker =
                                        Some(PlaylistPicker { track_uri: track.uri, selected: 0, filter: ListFilter::default() });
                                }
                            }
                            // Alt+Down: add to the queue -- the sibling of Ctrl+Down
                            // (add to a playlist) just above, for the same reason:
                            // every plain letter has to reach the query box here. Has
                            // to sit before the plain `Down` arm below, which matches
                            // regardless of modifiers.
                            KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
                                if let Some(track) = app.search.results.get(app.search.selected) {
                                    let uri = track.uri.clone();
                                    fire_add_to_queue(&mut app, &crud_tx, &spotify_client, uri);
                                }
                            }
                            KeyCode::Down => {
                                if !app.search.results.is_empty() {
                                    app.search.selected =
                                        (app.search.selected + 1).min(app.search.results.len() - 1);
                                }
                            }
                            // Not Right -- Enter here can play a track and jump to Now Playing,
                            // a real "leave here" side effect. Right means "go deeper," not
                            // "commit and teleport" (reported live as feeling unnatural).
                            KeyCode::Enter => {
                                if app.search.results.is_empty() {
                                    if let Some(client) = spotify_client.clone()
                                        && !app.search.query.trim().is_empty() {
                                            let query = app.search.query.clone();
                                            let tx = search_tx.clone();
                                            app.search.searching = true;
                                            app.search.error = None;
                                            tokio::spawn(async move {
                                                // Dev Mode apps cap search at 10 results (down
                                                // from 50 as of Spotify's Feb 2026 migration) --
                                                // confirmed live, anything higher is a 400
                                                // "Invalid limit".
                                                let result = api::search::search_tracks(&client, &query, 10)
                                                    .await
                                                    .map_err(|e| e.to_string());
                                                let _ = tx.send(result);
                                            });
                                        }
                                } else if let Some(track) =
                                    app.search.results.get(app.search.selected).cloned()
                                {
                                    // activate() must precede load(): Spirc
                                    // ignores Load while not the active
                                    // device (confirmed live, logged plainly).
                                    let _ = spirc.activate();
                                    let opts = carry_modes(app.shuffle, app.repeat, Default::default());
                                    let _ = spirc.load(LoadRequest::from_context_uri(track.uri, opts));
                                    let _ = spirc.play();
                                    app.context_label = Some("Search".to_string());
                                    app.nav.goto(Screen::NowPlaying);
                                }
                            }
                            KeyCode::Char(c) => {
                                app.search.insert_at_cursor(c);
                                app.search.results.clear();
                                app.search.error = None;
                            }
                            _ => {}
                        },
                        Screen::NowPlaying => match key.code {
                            KeyCode::Esc => {
                                app.nav.escape();
                            }
                            KeyCode::Char('q') => break 'inner LoopExit::Quit,
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                break 'inner LoopExit::Quit
                            }
                            KeyCode::Char('?') => {
                                app.nav.push(Screen::Help);
                            }
                            KeyCode::Char('f') => {
                                toggle_or_enter_fullscreen(&mut app);
                            }
                            KeyCode::Char('/') => {
                                app.nav.push(Screen::Search);
                                app.search.query.clear();
                                app.search.cursor = 0;
                                app.search.results.clear();
                                app.search.error = None;
                            }
                            // Shift+L, not plain `l` -- `l` is already the
                            // universal "jump to Library" shortcut below, so
                            // this guard has to come first (a terminal
                            // reporting Shift+L as lowercase+modifier would
                            // otherwise never reach it, same encoding-
                            // robustness concern `is_shift_char` already
                            // exists for elsewhere in this file).
                            KeyCode::Char(c) if is_shift_char(KeyCode::Char(c), key.modifiers, 'L', 'l') => {
                                if let Some(track_id) = tracker.current_track_id() {
                                    let track_uri = track_id.to_string();
                                    if let Some(client) = spotify_client.clone() {
                                        let tx = crud_tx.clone();
                                        tokio::spawn(async move {
                                            let result = api::library::like_track(&client, &track_uri).await;
                                            let _ = tx.send(CrudResult::LikeToggled { track_uri, liked: true, result });
                                        });
                                    }
                                }
                            }
                            // `goto`, not `push` -- a universal "jump to Library" shortcut
                            // should always land on exactly [NowPlaying, Library], never pile
                            // up stack depth if pressed again from somewhere already nested
                            // under Library (LikedSongs, Playlist Detail, etc).
                            KeyCode::Char('l') => {
                                app.nav.goto(Screen::Library);
                            }
                            KeyCode::Char('c') => {
                                app.text_prompt =
                                    Some(TextPrompt::new("New playlist name", "", TextPromptAction::CreatePlaylist));
                            }
                            // Adds whatever's currently playing -- the one screen where
                            // "the selected track" means the track itself, not a list row.
                            KeyCode::Char('a') => {
                                if let Some(track_id) = tracker.current_track_id() {
                                    app.playlist_picker =
                                        Some(PlaylistPicker { track_uri: track_id.to_string(), selected: 0, filter: ListFilter::default() });
                                }
                            }
                            KeyCode::Char(' ') => {
                                let _ = spirc.play_pause();
                            }
                            KeyCode::Char('n') => {
                                let _ = spirc.next();
                            }
                            KeyCode::Char('p') => {
                                let _ = spirc.prev();
                            }
                            KeyCode::Char('+') | KeyCode::Up => {
                                let _ = spirc.volume_up();
                            }
                            KeyCode::Char('-') | KeyCode::Down => {
                                let _ = spirc.volume_down();
                            }
                            KeyCode::Left => {
                                let target = seek_target_ms(
                                    tracker.progress_ms(Instant::now()) as i64,
                                    -SEEK_STEP_MS,
                                    app.duration.as_millis() as i64,
                                );
                                let _ = spirc.set_position_ms(target);
                            }
                            KeyCode::Right => {
                                let target = seek_target_ms(
                                    tracker.progress_ms(Instant::now()) as i64,
                                    SEEK_STEP_MS,
                                    app.duration.as_millis() as i64,
                                );
                                let _ = spirc.set_position_ms(target);
                            }
                            _ => {}
                        },
                        Screen::Library => match key.code {
                            KeyCode::Char('q') => break 'inner LoopExit::Quit,
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                break 'inner LoopExit::Quit
                            }
                            KeyCode::Char('?') => {
                                app.nav.push(Screen::Help);
                            }
                            KeyCode::Char(' ') => {
                                let _ = spirc.play_pause();
                            }
                            KeyCode::Char('n') => {
                                let _ = spirc.next();
                            }
                            KeyCode::Char('p') => {
                                let _ = spirc.prev();
                            }
                            KeyCode::Char('+') => {
                                let _ = spirc.volume_up();
                            }
                            KeyCode::Char('-') => {
                                let _ = spirc.volume_down();
                            }
                            KeyCode::Char('f') => {
                                toggle_or_enter_fullscreen(&mut app);
                            }
                            KeyCode::Char('l') => {
                                app.nav.goto(Screen::Library);
                            }
                            KeyCode::Char('c') => {
                                app.text_prompt =
                                    Some(TextPrompt::new("New playlist name", "", TextPromptAction::CreatePlaylist));
                            }
                            KeyCode::Esc | KeyCode::Left => {
                                app.nav.escape();
                            }
                            KeyCode::Up => {
                                app.library.home_selected = app.library.home_selected.saturating_sub(1);
                            }
                            KeyCode::Down => {
                                app.library.home_selected =
                                    (app.library.home_selected + 1).min(LIBRARY_ENTRIES.len() - 1);
                            }
                            KeyCode::Enter | KeyCode::Right => {
                                let (_, screen) = LIBRARY_ENTRIES[app.library.home_selected];
                                app.nav.push(screen);
                                match screen {
                                    Screen::LikedSongs
                                        if matches!(app.library.liked_songs, Fetch::NotStarted) =>
                                    {
                                        app.library.liked_songs = Fetch::Loading;
                                        match spotify_client.clone() {
                                            Some(client) => {
                                                let tx = library_tx.clone();
                                                tokio::spawn(async move {
                                                    let result = api::library::liked_songs(&client)
                                                        .await
                                                        .map_err(|e| e.to_string());
                                                    let _ = tx.send(LibraryFetchResult::LikedSongs(result));
                                                });
                                            }
                                            None => {
                                                app.library.liked_songs =
                                                    Fetch::Failed("Spotify client not ready yet".into());
                                            }
                                        }
                                    }
                                    Screen::SavedAlbums
                                        if matches!(app.library.saved_albums, Fetch::NotStarted) =>
                                    {
                                        app.library.saved_albums = Fetch::Loading;
                                        match spotify_client.clone() {
                                            Some(client) => {
                                                let tx = library_tx.clone();
                                                tokio::spawn(async move {
                                                    let result = api::library::saved_albums(&client)
                                                        .await
                                                        .map_err(|e| e.to_string());
                                                    let _ = tx.send(LibraryFetchResult::SavedAlbums(result));
                                                });
                                            }
                                            None => {
                                                app.library.saved_albums =
                                                    Fetch::Failed("Spotify client not ready yet".into());
                                            }
                                        }
                                    }
                                    Screen::FollowedArtists
                                        if matches!(app.library.followed_artists, Fetch::NotStarted) =>
                                    {
                                        app.library.followed_artists = Fetch::Loading;
                                        match spotify_client.clone() {
                                            Some(client) => {
                                                let tx = library_tx.clone();
                                                tokio::spawn(async move {
                                                    let result = api::library::followed_artists(&client)
                                                        .await
                                                        .map_err(|e| e.to_string());
                                                    let _ =
                                                        tx.send(LibraryFetchResult::FollowedArtists(result));
                                                });
                                            }
                                            None => {
                                                app.library.followed_artists =
                                                    Fetch::Failed("Spotify client not ready yet".into());
                                            }
                                        }
                                    }
                                    Screen::YourPlaylists
                                        if matches!(app.library.playlists, Fetch::NotStarted) =>
                                    {
                                        app.library.playlists = Fetch::Loading;
                                        match spotify_client.clone() {
                                            Some(client) => {
                                                let tx = library_tx.clone();
                                                tokio::spawn(async move {
                                                    let result = api::library::your_playlists(&client)
                                                        .await
                                                        .map_err(|e| e.to_string());
                                                    let _ = tx.send(LibraryFetchResult::Playlists(result));
                                                });
                                            }
                                            None => {
                                                app.library.playlists =
                                                    Fetch::Failed("Spotify client not ready yet".into());
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        },
                        Screen::LikedSongs => {
                            let label = |t: &TrackResult| format!("{} \u{2014} {}", t.artist, t.title);
                            match key.code {
                                // Filter editing intercepts everything typing-related --
                                // Left/Right move the cursor mid-string (same as Search),
                                // Up/Down fall through unguarded below so they still move
                                // the highlighted track live while typing, reported live
                                // as wanted.
                                KeyCode::Char(c) if app.library.liked_songs_filter.editing => {
                                    app.library.liked_songs_filter.insert_at_cursor(c);
                                    app.library.liked_songs_selected = 0;
                                }
                                KeyCode::Backspace if app.library.liked_songs_filter.editing => {
                                    app.library.liked_songs_filter.backspace_at_cursor();
                                    app.library.liked_songs_selected = 0;
                                }
                                KeyCode::Enter if app.library.liked_songs_filter.editing => {
                                    app.library.liked_songs_filter.editing = false;
                                }
                                KeyCode::Esc if app.library.liked_songs_filter.editing => {
                                    app.library.liked_songs_filter.cancel_editing();
                                    app.library.liked_songs_selected = 0;
                                }
                                // Left/Right move the cursor mid-string while typing (same as
                                // Search) -- two other designs (pane-nav passthrough, no-op)
                                // were tried and rejected live as feeling broken.
                                KeyCode::Left if app.library.liked_songs_filter.editing => {
                                    app.library.liked_songs_filter.cursor_left();
                                }
                                KeyCode::Right if app.library.liked_songs_filter.editing => {
                                    app.library.liked_songs_filter.cursor_right();
                                }
                                KeyCode::Char('q') => break 'inner LoopExit::Quit,
                                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                    break 'inner LoopExit::Quit
                                }
                                KeyCode::Char('?') => {
                                    app.nav.push(Screen::Help);
                                }
                                KeyCode::Char(' ') => {
                                    let _ = spirc.play_pause();
                                }
                                KeyCode::Char('n') => {
                                    let _ = spirc.next();
                                }
                                KeyCode::Char('p') => {
                                    let _ = spirc.prev();
                                }
                                KeyCode::Char('+') => {
                                    let _ = spirc.volume_up();
                                }
                                KeyCode::Char('-') => {
                                    let _ = spirc.volume_down();
                                }
                                KeyCode::Char('f') => {
                                    toggle_or_enter_fullscreen(&mut app);
                                }
                                // Shift+L before plain `l` -- same encoding-
                                // robustness reason as every other Shift+
                                // guard in this file. Every track shown
                                // here is already liked by definition, so
                                // this is always the *unlike* direction --
                                // Now Playing's own Shift+L (the only other
                                // place this key is bound) is always *like*
                                // instead, for the same reason.
                                // Confirms first -- reported live as wanted,
                                // same standing rule `d`/`Shift+D` already
                                // established for anything that removes an
                                // item from the very list you're looking at.
                                KeyCode::Char(c) if is_shift_char(KeyCode::Char(c), key.modifiers, 'L', 'l') => {
                                    if let Fetch::Ready(items) = &app.library.liked_songs {
                                        let display = filtered_sorted(items, &app.library.liked_songs_filter, &label);
                                        if let Some(track) =
                                            display.get(app.library.liked_songs_selected).map(|&(_, t)| t.clone())
                                        {
                                            app.pending_confirm = Some(PendingConfirm {
                                                message: format!(
                                                    "Unlike \"{} \u{2014} {}\"? y/n",
                                                    track.artist, track.title
                                                ),
                                                action: ConfirmAction::UnlikeTrack { track_uri: track.uri },
                                            });
                                        }
                                    }
                                }
                                // Add the selected track to the playback queue. Matches
                                // the literal 'Q' only, not `is_shift_char` like the
                                // other Shift+letter keys: that helper also accepts
                                // lowercase-plus-SHIFT, and lowercase `q` means quit.
                                KeyCode::Char('Q') => {
                                    let track_uri = match &app.library.liked_songs {
                                        Fetch::Ready(items) => {
                                            filtered_sorted(items, &app.library.liked_songs_filter, &label)
                                                .get(app.library.liked_songs_selected)
                                                .map(|&(_, t)| t.uri.clone())
                                        }
                                        _ => None,
                                    };
                                    if let Some(uri) = track_uri {
                                        fire_add_to_queue(&mut app, &crud_tx, &spotify_client, uri);
                                    }
                                }
                                KeyCode::Char('l') => {
                                    app.nav.goto(Screen::Library);
                                }
                                KeyCode::Char('c') => {
                                    app.text_prompt = Some(TextPrompt::new(
                                        "New playlist name",
                                        "",
                                        TextPromptAction::CreatePlaylist,
                                    ));
                                }
                                // A filter that's applied but no longer being typed still
                                // has to be dismissed with its own Esc first -- otherwise
                                // Esc immediately leaves the screen with the filter
                                // silently still narrowing the list next time it's opened.
                                KeyCode::Esc if !app.library.liked_songs_filter.query.is_empty() => {
                                    app.library.liked_songs_filter.query.clear();
                                    app.library.liked_songs_filter.cursor = 0;
                                    app.library.liked_songs_selected = 0;
                                }
                                KeyCode::Esc | KeyCode::Left => {
                                    app.nav.escape();
                                }
                                KeyCode::Char('/') => {
                                    app.library.liked_songs_filter.start_editing();
                                }
                                KeyCode::Char('o') => {
                                    app.library.liked_songs_filter.sort_alpha =
                                        !app.library.liked_songs_filter.sort_alpha;
                                }
                                KeyCode::Char('a') => {
                                    if let Fetch::Ready(items) = &app.library.liked_songs {
                                        let display = filtered_sorted(items, &app.library.liked_songs_filter, &label);
                                        if let Some(track) =
                                            display.get(app.library.liked_songs_selected).map(|&(_, t)| t.clone())
                                        {
                                            app.playlist_picker =
                                                Some(PlaylistPicker { track_uri: track.uri, selected: 0, filter: ListFilter::default() });
                                        }
                                    }
                                }
                                // Shift+V (artist) has to be checked before plain `v`
                                // (album) -- some terminals report Shift+V as lowercase
                                // 'v' plus a SHIFT modifier flag rather than literal
                                // uppercase 'V', same encoding gap `is_pin_key`/Shift+D
                                // already exist for; an unguarded plain-`v` arm placed
                                // first would swallow that case before this one ever saw it.
                                KeyCode::Char(c) if is_shift_char(KeyCode::Char(c), key.modifiers, 'V', 'v') => {
                                    let artist_uri = match &app.library.liked_songs {
                                        Fetch::Ready(items) => {
                                            let display = filtered_sorted(items, &app.library.liked_songs_filter, &label);
                                            display
                                                .get(app.library.liked_songs_selected)
                                                .map(|&(_, t)| t.artist_uri.clone())
                                        }
                                        _ => None,
                                    };
                                    if let Some(artist_uri) = artist_uri {
                                        open_artist_detail(&mut app, &library_tx, &spotify_client, artist_uri);
                                    }
                                }
                                // `v` = open this track's album -- the more common
                                // action from a plain track list. Reported live as
                                // backwards from the original (artist-on-`v`) design.
                                KeyCode::Char('v') => {
                                    let album_uri = match &app.library.liked_songs {
                                        Fetch::Ready(items) => {
                                            let display = filtered_sorted(items, &app.library.liked_songs_filter, &label);
                                            display
                                                .get(app.library.liked_songs_selected)
                                                .map(|&(_, t)| t.album_uri.clone())
                                        }
                                        _ => None,
                                    };
                                    if let Some(album_uri) = album_uri {
                                        open_album_detail(&mut app, &library_tx, &spotify_client, album_uri);
                                    }
                                }
                                KeyCode::Up => {
                                    app.library.liked_songs_selected =
                                        app.library.liked_songs_selected.saturating_sub(1);
                                }
                                KeyCode::Down => {
                                    if let Fetch::Ready(items) = &app.library.liked_songs {
                                        let display = filtered_sorted(items, &app.library.liked_songs_filter, &label);
                                        if !display.is_empty() {
                                            app.library.liked_songs_selected =
                                                (app.library.liked_songs_selected + 1).min(display.len() - 1);
                                        }
                                    }
                                }
                                // Not Right -- Enter here can play a track and jump to Now Playing,
                                // a real "leave here" side effect. Right means "go deeper," not
                                // "commit and teleport" (reported live as feeling unnatural).
                                KeyCode::Enter => {
                                    if let Fetch::Ready(items) = &app.library.liked_songs {
                                        let display = filtered_sorted(items, &app.library.liked_songs_filter, &label);
                                        if let Some(track) =
                                            display.get(app.library.liked_songs_selected).map(|&(_, t)| t.clone())
                                        {
                                            let _ = spirc.activate();
                                            let opts = carry_modes(app.shuffle, app.repeat, Default::default());
                                            let _ = spirc.load(LoadRequest::from_context_uri(track.uri, opts));
                                            let _ = spirc.play();
                                            app.context_label = Some("Liked Songs".to_string());
                                            app.nav.goto(Screen::NowPlaying);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        Screen::SavedAlbums => {
                            let label = |a: &SavedAlbumSummary| format!("{} \u{2014} {}", a.name, a.artist);
                            match key.code {
                                KeyCode::Char(c) if app.library.saved_albums_filter.editing => {
                                    app.library.saved_albums_filter.insert_at_cursor(c);
                                    app.library.saved_albums_selected = 0;
                                }
                                KeyCode::Backspace if app.library.saved_albums_filter.editing => {
                                    app.library.saved_albums_filter.backspace_at_cursor();
                                    app.library.saved_albums_selected = 0;
                                }
                                KeyCode::Enter if app.library.saved_albums_filter.editing => {
                                    app.library.saved_albums_filter.editing = false;
                                }
                                KeyCode::Esc if app.library.saved_albums_filter.editing => {
                                    app.library.saved_albums_filter.cancel_editing();
                                    app.library.saved_albums_selected = 0;
                                }
                                KeyCode::Left if app.library.saved_albums_filter.editing => {
                                    app.library.saved_albums_filter.cursor_left();
                                }
                                KeyCode::Right if app.library.saved_albums_filter.editing => {
                                    app.library.saved_albums_filter.cursor_right();
                                }
                                KeyCode::Char('q') => break 'inner LoopExit::Quit,
                                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                    break 'inner LoopExit::Quit
                                }
                                KeyCode::Char('?') => {
                                    app.nav.push(Screen::Help);
                                }
                                KeyCode::Char(' ') => {
                                    let _ = spirc.play_pause();
                                }
                                KeyCode::Char('n') => {
                                    let _ = spirc.next();
                                }
                                KeyCode::Char('p') => {
                                    let _ = spirc.prev();
                                }
                                KeyCode::Char('+') => {
                                    let _ = spirc.volume_up();
                                }
                                KeyCode::Char('-') => {
                                    let _ = spirc.volume_down();
                                }
                                KeyCode::Char('f') => {
                                    toggle_or_enter_fullscreen(&mut app);
                                }
                                KeyCode::Char('l') => {
                                    app.nav.goto(Screen::Library);
                                }
                                // Every album shown here is already saved by
                                // definition, so this is always the *unsave*
                                // direction -- Album Detail's own `s` (the only
                                // other place this key is bound) is always *save*.
                                // Confirms first -- same standing rule as
                                // Liked Songs' own Shift+L above.
                                KeyCode::Char('s') => {
                                    if let Fetch::Ready(items) = &app.library.saved_albums {
                                        let display = filtered_sorted(items, &app.library.saved_albums_filter, &label);
                                        if let Some(album) =
                                            display.get(app.library.saved_albums_selected).map(|&(_, a)| a.clone())
                                        {
                                            app.pending_confirm = Some(PendingConfirm {
                                                message: format!(
                                                    "Unsave \"{} \u{2014} {}\"? y/n",
                                                    album.name, album.artist
                                                ),
                                                action: ConfirmAction::UnsaveAlbum { album_uri: album.uri },
                                            });
                                        }
                                    }
                                }
                                KeyCode::Char('c') => {
                                    app.text_prompt = Some(TextPrompt::new(
                                        "New playlist name",
                                        "",
                                        TextPromptAction::CreatePlaylist,
                                    ));
                                }
                                KeyCode::Esc if !app.library.saved_albums_filter.query.is_empty() => {
                                    app.library.saved_albums_filter.query.clear();
                                    app.library.saved_albums_filter.cursor = 0;
                                    app.library.saved_albums_selected = 0;
                                }
                                KeyCode::Esc | KeyCode::Left => {
                                    app.nav.escape();
                                }
                                KeyCode::Char('/') => {
                                    app.library.saved_albums_filter.start_editing();
                                }
                                KeyCode::Char('o') => {
                                    app.library.saved_albums_filter.sort_alpha =
                                        !app.library.saved_albums_filter.sort_alpha;
                                }
                                KeyCode::Up => {
                                    app.library.saved_albums_selected =
                                        app.library.saved_albums_selected.saturating_sub(1);
                                }
                                KeyCode::Down => {
                                    if let Fetch::Ready(items) = &app.library.saved_albums {
                                        let display = filtered_sorted(items, &app.library.saved_albums_filter, &label);
                                        if !display.is_empty() {
                                            app.library.saved_albums_selected =
                                                (app.library.saved_albums_selected + 1).min(display.len() - 1);
                                        }
                                    }
                                }
                                // Pure navigation, no playback -- opening an album
                                // doesn't play anything.
                                KeyCode::Enter | KeyCode::Right => {
                                    let album_uri = match &app.library.saved_albums {
                                        Fetch::Ready(items) => {
                                            let display =
                                                filtered_sorted(items, &app.library.saved_albums_filter, &label);
                                            display
                                                .get(app.library.saved_albums_selected)
                                                .map(|&(_, a)| a.uri.clone())
                                        }
                                        _ => None,
                                    };
                                    if let Some(album_uri) = album_uri {
                                        open_album_detail(&mut app, &library_tx, &spotify_client, album_uri);
                                    }
                                }
                                _ => {}
                            }
                        }
                        Screen::FollowedArtists => {
                            let label = |a: &FollowedArtist| a.name.clone();
                            match key.code {
                                KeyCode::Char(c) if app.library.followed_artists_filter.editing => {
                                    app.library.followed_artists_filter.insert_at_cursor(c);
                                    app.library.followed_artists_selected = 0;
                                }
                                KeyCode::Backspace if app.library.followed_artists_filter.editing => {
                                    app.library.followed_artists_filter.backspace_at_cursor();
                                    app.library.followed_artists_selected = 0;
                                }
                                KeyCode::Enter if app.library.followed_artists_filter.editing => {
                                    app.library.followed_artists_filter.editing = false;
                                }
                                KeyCode::Esc if app.library.followed_artists_filter.editing => {
                                    app.library.followed_artists_filter.cancel_editing();
                                    app.library.followed_artists_selected = 0;
                                }
                                KeyCode::Left if app.library.followed_artists_filter.editing => {
                                    app.library.followed_artists_filter.cursor_left();
                                }
                                KeyCode::Right if app.library.followed_artists_filter.editing => {
                                    app.library.followed_artists_filter.cursor_right();
                                }
                                KeyCode::Char('q') => break 'inner LoopExit::Quit,
                                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                    break 'inner LoopExit::Quit
                                }
                                KeyCode::Char('?') => {
                                    app.nav.push(Screen::Help);
                                }
                                KeyCode::Char(' ') => {
                                    let _ = spirc.play_pause();
                                }
                                KeyCode::Char('n') => {
                                    let _ = spirc.next();
                                }
                                KeyCode::Char('p') => {
                                    let _ = spirc.prev();
                                }
                                KeyCode::Char('+') => {
                                    let _ = spirc.volume_up();
                                }
                                KeyCode::Char('-') => {
                                    let _ = spirc.volume_down();
                                }
                                // Shift+F before plain `f` (fullscreen) --
                                // same encoding-robustness reason as every
                                // other Shift+ guard in this file. Every
                                // artist shown here is already followed by
                                // definition, so this is always *unfollow*
                                // -- Artist Detail's own Shift+F (the only
                                // other place this key is bound) is always
                                // *follow*.
                                // Confirms first -- same standing rule as
                                // Liked Songs' own Shift+L above.
                                KeyCode::Char(c) if is_shift_char(KeyCode::Char(c), key.modifiers, 'F', 'f') => {
                                    if let Fetch::Ready(items) = &app.library.followed_artists {
                                        let display =
                                            filtered_sorted(items, &app.library.followed_artists_filter, &label);
                                        if let Some(artist) = display
                                            .get(app.library.followed_artists_selected)
                                            .map(|&(_, a)| a.clone())
                                        {
                                            app.pending_confirm = Some(PendingConfirm {
                                                message: format!("Unfollow \"{}\"? y/n", artist.name),
                                                action: ConfirmAction::UnfollowArtist { artist_uri: artist.uri },
                                            });
                                        }
                                    }
                                }
                                KeyCode::Char('f') => {
                                    toggle_or_enter_fullscreen(&mut app);
                                }
                                KeyCode::Char('l') => {
                                    app.nav.goto(Screen::Library);
                                }
                                KeyCode::Char('c') => {
                                    app.text_prompt = Some(TextPrompt::new(
                                        "New playlist name",
                                        "",
                                        TextPromptAction::CreatePlaylist,
                                    ));
                                }
                                KeyCode::Esc if !app.library.followed_artists_filter.query.is_empty() => {
                                    app.library.followed_artists_filter.query.clear();
                                    app.library.followed_artists_filter.cursor = 0;
                                    app.library.followed_artists_selected = 0;
                                }
                                KeyCode::Esc | KeyCode::Left => {
                                    app.nav.escape();
                                }
                                KeyCode::Char('/') => {
                                    app.library.followed_artists_filter.start_editing();
                                }
                                KeyCode::Char('o') => {
                                    app.library.followed_artists_filter.sort_alpha =
                                        !app.library.followed_artists_filter.sort_alpha;
                                }
                                KeyCode::Up => {
                                    app.library.followed_artists_selected =
                                        app.library.followed_artists_selected.saturating_sub(1);
                                }
                                KeyCode::Down => {
                                    if let Fetch::Ready(items) = &app.library.followed_artists {
                                        let display = filtered_sorted(items, &app.library.followed_artists_filter, &label);
                                        if !display.is_empty() {
                                            app.library.followed_artists_selected =
                                                (app.library.followed_artists_selected + 1).min(display.len() - 1);
                                        }
                                    }
                                }
                                // Pure navigation, no playback -- opening an artist
                                // doesn't play anything.
                                KeyCode::Enter | KeyCode::Right => {
                                    let artist_uri = match &app.library.followed_artists {
                                        Fetch::Ready(items) => {
                                            let display =
                                                filtered_sorted(items, &app.library.followed_artists_filter, &label);
                                            display
                                                .get(app.library.followed_artists_selected)
                                                .map(|&(_, a)| a.uri.clone())
                                        }
                                        _ => None,
                                    };
                                    if let Some(artist_uri) = artist_uri {
                                        open_artist_detail(&mut app, &library_tx, &spotify_client, artist_uri);
                                    }
                                }
                                _ => {}
                            }
                        }
                        Screen::YourPlaylists => {
                            let label =
                                |p: &PlaylistSummary| format!("{} ({} tracks)", p.name, p.track_count);
                            match key.code {
                                KeyCode::Char(c) if app.library.playlists_filter.editing => {
                                    app.library.playlists_filter.insert_at_cursor(c);
                                    app.library.playlists_selected = 0;
                                }
                                KeyCode::Backspace if app.library.playlists_filter.editing => {
                                    app.library.playlists_filter.backspace_at_cursor();
                                    app.library.playlists_selected = 0;
                                }
                                KeyCode::Enter if app.library.playlists_filter.editing => {
                                    app.library.playlists_filter.editing = false;
                                }
                                KeyCode::Esc if app.library.playlists_filter.editing => {
                                    app.library.playlists_filter.cancel_editing();
                                    app.library.playlists_selected = 0;
                                }
                                KeyCode::Left if app.library.playlists_filter.editing => {
                                    app.library.playlists_filter.cursor_left();
                                }
                                KeyCode::Right if app.library.playlists_filter.editing => {
                                    app.library.playlists_filter.cursor_right();
                                }
                                KeyCode::Char('q') => break 'inner LoopExit::Quit,
                                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                    break 'inner LoopExit::Quit
                                }
                                KeyCode::Char('?') => {
                                    app.nav.push(Screen::Help);
                                }
                                KeyCode::Char(' ') => {
                                    let _ = spirc.play_pause();
                                }
                                KeyCode::Char('n') => {
                                    let _ = spirc.next();
                                }
                                KeyCode::Char('p') => {
                                    let _ = spirc.prev();
                                }
                                KeyCode::Char('+') => {
                                    let _ = spirc.volume_up();
                                }
                                KeyCode::Char('-') => {
                                    let _ = spirc.volume_down();
                                }
                                KeyCode::Char('f') => {
                                    toggle_or_enter_fullscreen(&mut app);
                                }
                                KeyCode::Char('l') => {
                                    app.nav.goto(Screen::Library);
                                }
                                KeyCode::Esc if !app.library.playlists_filter.query.is_empty() => {
                                    app.library.playlists_filter.query.clear();
                                    app.library.playlists_filter.cursor = 0;
                                    app.library.playlists_selected = 0;
                                }
                                KeyCode::Esc | KeyCode::Left => {
                                    app.nav.escape();
                                }
                                KeyCode::Char('/') => {
                                    app.library.playlists_filter.start_editing();
                                }
                                KeyCode::Char('o') => {
                                    app.library.playlists_filter.sort_alpha =
                                        !app.library.playlists_filter.sort_alpha;
                                }
                                KeyCode::Char('c') => {
                                    app.text_prompt =
                                        Some(TextPrompt::new("New playlist name", "", TextPromptAction::CreatePlaylist));
                                }
                                KeyCode::Char('r') => {
                                    if let Fetch::Ready(items) = &app.library.playlists {
                                        let display = pinned_first(
                                            filtered_sorted(items, &app.library.playlists_filter, &label),
                                            &app.pinned_playlists,
                                            |p| p.uri.as_str(),
                                        );
                                        if let Some(playlist) =
                                            display.get(app.library.playlists_selected).map(|&(_, p)| p.clone())
                                        {
                                            app.text_prompt = Some(TextPrompt::new(
                                                "Rename playlist",
                                                playlist.name.clone(),
                                                TextPromptAction::RenamePlaylist(playlist),
                                            ));
                                        }
                                    }
                                }
                                // Always confirms first, no exceptions -- deleting a playlist
                                // is hard to reverse.
                                KeyCode::Char('d') => {
                                    if let Fetch::Ready(items) = &app.library.playlists {
                                        let display = pinned_first(
                                            filtered_sorted(items, &app.library.playlists_filter, &label),
                                            &app.pinned_playlists,
                                            |p| p.uri.as_str(),
                                        );
                                        if let Some(playlist) =
                                            display.get(app.library.playlists_selected).map(|&(_, p)| p.clone())
                                        {
                                            app.pending_confirm = Some(PendingConfirm {
                                                message: format!("Delete playlist \"{}\"? y/n", playlist.name),
                                                action: ConfirmAction::DeletePlaylist(playlist),
                                            });
                                        }
                                    }
                                }
                                KeyCode::Char(c) if is_pin_key(KeyCode::Char(c), key.modifiers) => {
                                    if let Fetch::Ready(items) = &app.library.playlists {
                                        let display = pinned_first(
                                            filtered_sorted(items, &app.library.playlists_filter, &label),
                                            &app.pinned_playlists,
                                            |p| p.uri.as_str(),
                                        );
                                        if let Some(uri) =
                                            display.get(app.library.playlists_selected).map(|&(_, p)| p.uri.clone())
                                        {
                                            pins::toggle_in_place(&mut app.pinned_playlists, &uri);
                                            pins::save("playlists", &app.pinned_playlists);
                                        }
                                    }
                                }
                                KeyCode::Up => {
                                    app.library.playlists_selected =
                                        app.library.playlists_selected.saturating_sub(1);
                                }
                                KeyCode::Down => {
                                    if let Fetch::Ready(items) = &app.library.playlists {
                                        let display = pinned_first(
                                            filtered_sorted(items, &app.library.playlists_filter, &label),
                                            &app.pinned_playlists,
                                            |p| p.uri.as_str(),
                                        );
                                        if !display.is_empty() {
                                            app.library.playlists_selected =
                                                (app.library.playlists_selected + 1).min(display.len() - 1);
                                        }
                                    }
                                }
                                KeyCode::Enter | KeyCode::Right => {
                                    let playlist = match &app.library.playlists {
                                        Fetch::Ready(items) => {
                                            let display = pinned_first(
                                                filtered_sorted(items, &app.library.playlists_filter, &label),
                                                &app.pinned_playlists,
                                                |p| p.uri.as_str(),
                                            );
                                            display
                                                .get(app.library.playlists_selected)
                                                .map(|&(_, p)| p.clone())
                                        }
                                        _ => None,
                                    };
                                    if let Some(playlist) = playlist {
                                        open_playlist_detail(&mut app, &library_tx, &spotify_client, playlist);
                                    }
                                }
                                _ => {}
                            }
                        }
                        Screen::PlaylistDetail => {
                            let label = |t: &TrackResult| format!("{} \u{2014} {}", t.artist, t.title);
                            let filter_editing =
                                app.playlist_detail.as_ref().is_some_and(|pd| pd.filter.editing);
                            let move_mode_active =
                                app.playlist_detail.as_ref().is_some_and(|pd| pd.move_mode.is_some());
                            match key.code {
                                KeyCode::Char(c) if filter_editing => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.filter.insert_at_cursor(c);
                                        pd.selected = 0;
                                    }
                                }
                                KeyCode::Backspace if filter_editing => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.filter.backspace_at_cursor();
                                        pd.selected = 0;
                                    }
                                }
                                KeyCode::Enter if filter_editing => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.filter.editing = false;
                                    }
                                }
                                KeyCode::Esc if filter_editing => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.filter.cancel_editing();
                                        pd.selected = 0;
                                    }
                                }
                                KeyCode::Left if filter_editing => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.filter.cursor_left();
                                    }
                                }
                                KeyCode::Right if filter_editing => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.filter.cursor_right();
                                    }
                                }
                                // Move-mode: `Up`/`Down` relocate the track locally (no
                                // network call per keystroke), `Enter` confirms with a
                                // single `reorder_track` call reflecting the net
                                // displacement, `Esc` walks it back to where it started.
                                // Every other key is swallowed while active -- same
                                // reasoning as `filter_editing`'s own catch-all: editing
                                // playlist content mid-reorder (remove/add/rename/pin)
                                // would race the pending local move.
                                KeyCode::Up if move_mode_active => {
                                    if let Some(pd) = &mut app.playlist_detail
                                        && let Fetch::Ready(items) = &mut pd.tracks {
                                            pd.selected = ui::move_item_up(items, pd.selected);
                                        }
                                }
                                KeyCode::Down if move_mode_active => {
                                    if let Some(pd) = &mut app.playlist_detail
                                        && let Fetch::Ready(items) = &mut pd.tracks {
                                            pd.selected = ui::move_item_down(items, pd.selected);
                                        }
                                }
                                KeyCode::Enter if move_mode_active => {
                                    if let Some(pd) = &mut app.playlist_detail
                                        && let Some(start) = pd.move_mode.take() {
                                            let end = pd.selected;
                                            if start != end {
                                                if let Some(client) = spotify_client.clone() {
                                                    app.status = Some(("reordering\u{2026}".to_string(), false));
                                                    let tx = crud_tx.clone();
                                                    let playlist_uri = pd.playlist.uri.clone();
                                                    let playlist_uri_for_result = playlist_uri.clone();
                                                    tokio::spawn(async move {
                                                        let result = api::playlists::reorder_track(
                                                            &client,
                                                            &playlist_uri,
                                                            start,
                                                            end,
                                                        )
                                                        .await;
                                                        let _ = tx.send(CrudResult::TrackReordered {
                                                            playlist_uri: playlist_uri_for_result,
                                                            result,
                                                        });
                                                    });
                                                } else {
                                                    app.status =
                                                        Some(("Spotify client not ready yet".to_string(), true));
                                                }
                                            }
                                        }
                                    resume_normal_display_index(&mut app);
                                }
                                KeyCode::Esc if move_mode_active => {
                                    if let Some(pd) = &mut app.playlist_detail
                                        && let Some(start) = pd.move_mode.take()
                                            && let Fetch::Ready(items) = &mut pd.tracks {
                                                pd.selected = ui::move_item_to(items, pd.selected, start);
                                            }
                                    resume_normal_display_index(&mut app);
                                }
                                // Must sit ahead of the catch-all just below, which
                                // swallows every other character while moving.
                                KeyCode::Char('g') if move_mode_active => {
                                    let len = match app.playlist_detail.as_ref().map(|pd| &pd.tracks) {
                                        Some(Fetch::Ready(items)) => items.len(),
                                        _ => 0,
                                    };
                                    if len > 1 {
                                        app.text_prompt = Some(TextPrompt::new(
                                            format!("Move to position (1-{len})"),
                                            "",
                                            TextPromptAction::MoveToPosition,
                                        ));
                                    } else {
                                        app.status = Some(("only one track -- nowhere to move it".to_string(), true));
                                    }
                                }
                                KeyCode::Char(_) if move_mode_active => {}
                                KeyCode::Char('m') => {
                                    // Pins don't block this -- move-mode simply stops
                                    // applying the pinned-first bubbling to the display
                                    // while active (see render_playlist_detail), so display
                                    // position always equals real array position
                                    // regardless of what's pinned. Filter/sort still have
                                    // to be off for the same reason: either one changes
                                    // display order in a way this doesn't (yet) undo.
                                    let ineligible_reason = match &app.playlist_detail {
                                        Some(pd) if pd.filter.editing || !pd.filter.query.is_empty() || pd.filter.sort_alpha => {
                                            Some("clear the filter and turn off sort to reorder")
                                        }
                                        Some(pd) => match &pd.tracks {
                                            Fetch::Ready(_) => None,
                                            _ => Some("still loading"),
                                        },
                                        None => Some("no playlist open"),
                                    };
                                    match ineligible_reason {
                                        None => {
                                            // `pd.selected` right now is a position in the
                                            // normal pinned-first DISPLAY, not necessarily
                                            // the real array -- has to be converted before
                                            // move-mode starts treating it as a raw index,
                                            // or entering move-mode with a pin active would
                                            // silently start moving the wrong track.
                                            let real_index = if let Some(pd) = &app.playlist_detail {
                                                if let Fetch::Ready(items) = &pd.tracks {
                                                    let display = pinned_first(
                                                        filtered_sorted(items, &pd.filter, &label),
                                                        &app.pinned_tracks,
                                                        |t| t.uri.as_str(),
                                                    );
                                                    display.get(pd.selected).map(|&(i, _)| i)
                                                } else {
                                                    None
                                                }
                                            } else {
                                                None
                                            };
                                            if let Some(real_index) = real_index
                                                && let Some(pd) = &mut app.playlist_detail {
                                                    pd.selected = real_index;
                                                    pd.move_mode = Some(real_index);
                                                }
                                        }
                                        Some(reason) => {
                                            app.status = Some((reason.to_string(), true));
                                        }
                                    }
                                }
                                KeyCode::Char('q') => break 'inner LoopExit::Quit,
                                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                    break 'inner LoopExit::Quit
                                }
                                KeyCode::Char('?') => {
                                    app.nav.push(Screen::Help);
                                }
                                KeyCode::Char(' ') => {
                                    let _ = spirc.play_pause();
                                }
                                KeyCode::Char('n') => {
                                    let _ = spirc.next();
                                }
                                KeyCode::Char('p') => {
                                    let _ = spirc.prev();
                                }
                                KeyCode::Char('+') => {
                                    let _ = spirc.volume_up();
                                }
                                KeyCode::Char('-') => {
                                    let _ = spirc.volume_down();
                                }
                                KeyCode::Char('f') => {
                                    toggle_or_enter_fullscreen(&mut app);
                                }
                                // Shift+L before plain `l` -- same encoding-
                                // robustness reason as every other Shift+
                                // guard in this file. Not every track in a
                                // playlist is necessarily already liked, so
                                // unlike Liked Songs' own Shift+L, this is
                                // always the *like* direction.
                                KeyCode::Char(c) if is_shift_char(KeyCode::Char(c), key.modifiers, 'L', 'l') => {
                                    let track_uri = app.playlist_detail.as_ref().and_then(|pd| {
                                        if let Fetch::Ready(items) = &pd.tracks {
                                            let display = pinned_first(
                                                filtered_sorted(items, &pd.filter, &label),
                                                &app.pinned_tracks,
                                                |t| t.uri.as_str(),
                                            );
                                            display.get(pd.selected).map(|&(_, t)| t.uri.clone())
                                        } else {
                                            None
                                        }
                                    });
                                    if let (Some(track_uri), Some(client)) = (track_uri, spotify_client.clone()) {
                                        let tx = crud_tx.clone();
                                        tokio::spawn(async move {
                                            let result = api::library::like_track(&client, &track_uri).await;
                                            let _ = tx.send(CrudResult::LikeToggled { track_uri, liked: true, result });
                                        });
                                    }
                                }
                                // Add to queue -- literal 'Q' only, see Liked Songs' arm.
                                // Sits after the move-mode catch-all above, so it can't
                                // fire mid-reorder.
                                KeyCode::Char('Q') => {
                                    let track_uri = app.playlist_detail.as_ref().and_then(|pd| {
                                        if let Fetch::Ready(items) = &pd.tracks {
                                            let display = pinned_first(
                                                filtered_sorted(items, &pd.filter, &label),
                                                &app.pinned_tracks,
                                                |t| t.uri.as_str(),
                                            );
                                            display.get(pd.selected).map(|&(_, t)| t.uri.clone())
                                        } else {
                                            None
                                        }
                                    });
                                    if let Some(uri) = track_uri {
                                        fire_add_to_queue(&mut app, &crud_tx, &spotify_client, uri);
                                    }
                                }
                                KeyCode::Char('l') => {
                                    app.nav.goto(Screen::Library);
                                }
                                // A different playlist than the one open here -- `r` already
                                // owns renaming *this* one.
                                KeyCode::Char('c') => {
                                    app.text_prompt = Some(TextPrompt::new(
                                        "New playlist name",
                                        "",
                                        TextPromptAction::CreatePlaylist,
                                    ));
                                }
                                KeyCode::Esc
                                    if app
                                        .playlist_detail
                                        .as_ref()
                                        .is_some_and(|pd| !pd.filter.query.is_empty()) =>
                                {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.filter.query.clear();
                                        pd.filter.cursor = 0;
                                        pd.selected = 0;
                                    }
                                }
                                KeyCode::Esc | KeyCode::Left => {
                                    app.nav.escape();
                                }
                                KeyCode::Char('/') => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.filter.start_editing();
                                    }
                                }
                                KeyCode::Char('o') => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.filter.sort_alpha = !pd.filter.sort_alpha;
                                    }
                                }
                                // Renames the open playlist itself, not the selected track.
                                KeyCode::Char('r') => {
                                    if let Some(pd) = &app.playlist_detail {
                                        app.text_prompt = Some(TextPrompt::new(
                                            "Rename playlist",
                                            pd.playlist.name.clone(),
                                            TextPromptAction::RenamePlaylist(pd.playlist.clone()),
                                        ));
                                    }
                                }
                                // Always confirms first, no exceptions -- removing a track
                                // is hard to reverse.
                                KeyCode::Char('d') => {
                                    if let Some(pd) = &app.playlist_detail
                                        && let Fetch::Ready(items) = &pd.tracks {
                                            let display = pinned_first(
                                                filtered_sorted(items, &pd.filter, &label),
                                                &app.pinned_tracks,
                                                |t| t.uri.as_str(),
                                            );
                                            if let Some((_, track)) = display.get(pd.selected) {
                                                let track = (*track).clone();
                                                let playlist_uri = pd.playlist.uri.clone();
                                                // Spotify's own remove-tracks endpoint has no
                                                // reliable way to remove just one copy of a
                                                // duplicated track (spiked both variants --
                                                // `spike::run_spike_remove_specific_occurrence`
                                                // -- one removed every copy regardless of the
                                                // position given, the other silently removed
                                                // none). Warn honestly rather than surprise
                                                // the user with a bigger deletion than they
                                                // asked for.
                                                let occurrences =
                                                    items.iter().filter(|t| t.uri == track.uri).count();
                                                let message = if occurrences > 1 {
                                                    format!(
                                                        "\"{} \u{2014} {}\" appears {occurrences} times in this playlist -- Spotify's API can only remove ALL copies at once, not a single one. Remove all {occurrences}? y/n",
                                                        track.artist, track.title
                                                    )
                                                } else {
                                                    format!(
                                                        "Remove \"{} \u{2014} {}\" from this playlist? y/n",
                                                        track.artist, track.title
                                                    )
                                                };
                                                app.pending_confirm = Some(PendingConfirm {
                                                    message,
                                                    action: ConfirmAction::RemoveTrack {
                                                        playlist_uri,
                                                        track_uri: track.uri,
                                                        occurrences,
                                                    },
                                                });
                                            }
                                        }
                                }
                                // Shift+D deletes the *open playlist itself* -- plain `d`
                                // already means "remove the selected track" here, so
                                // deleting the playlist needs a different key. Reported
                                // live: pressing `d` inside a playlist only ever removed a
                                // track, with no way to delete the playlist from within it.
                                KeyCode::Char(c) if is_shift_char(KeyCode::Char(c), key.modifiers, 'D', 'd') => {
                                    if let Some(pd) = &app.playlist_detail {
                                        app.pending_confirm = Some(PendingConfirm {
                                            message: format!("Delete playlist \"{}\"? y/n", pd.playlist.name),
                                            action: ConfirmAction::DeletePlaylist(pd.playlist.clone()),
                                        });
                                    }
                                }
                                KeyCode::Char('a') => {
                                    if let Some(pd) = &app.playlist_detail
                                        && let Fetch::Ready(items) = &pd.tracks {
                                            let display = pinned_first(
                                                filtered_sorted(items, &pd.filter, &label),
                                                &app.pinned_tracks,
                                                |t| t.uri.as_str(),
                                            );
                                            if let Some((_, track)) = display.get(pd.selected) {
                                                app.playlist_picker =
                                                    Some(PlaylistPicker { track_uri: track.uri.clone(), selected: 0, filter: ListFilter::default() });
                                            }
                                        }
                                }
                                // Shift+V (artist) before plain `v` (album) -- same
                                // terminal-encoding reasoning as every other
                                // is_shift_char guard in this file.
                                KeyCode::Char(c) if is_shift_char(KeyCode::Char(c), key.modifiers, 'V', 'v') => {
                                    let artist_uri = app.playlist_detail.as_ref().and_then(|pd| {
                                        if let Fetch::Ready(items) = &pd.tracks {
                                            let display = pinned_first(
                                                filtered_sorted(items, &pd.filter, &label),
                                                &app.pinned_tracks,
                                                |t| t.uri.as_str(),
                                            );
                                            display.get(pd.selected).map(|&(_, t)| t.artist_uri.clone())
                                        } else {
                                            None
                                        }
                                    });
                                    if let Some(artist_uri) = artist_uri {
                                        open_artist_detail(&mut app, &library_tx, &spotify_client, artist_uri);
                                    }
                                }
                                KeyCode::Char('v') => {
                                    let album_uri = app.playlist_detail.as_ref().and_then(|pd| {
                                        if let Fetch::Ready(items) = &pd.tracks {
                                            let display = pinned_first(
                                                filtered_sorted(items, &pd.filter, &label),
                                                &app.pinned_tracks,
                                                |t| t.uri.as_str(),
                                            );
                                            display.get(pd.selected).map(|&(_, t)| t.album_uri.clone())
                                        } else {
                                            None
                                        }
                                    });
                                    if let Some(album_uri) = album_uri {
                                        open_album_detail(&mut app, &library_tx, &spotify_client, album_uri);
                                    }
                                }
                                KeyCode::Char(c) if is_pin_key(KeyCode::Char(c), key.modifiers) => {
                                    if let Some(pd) = &app.playlist_detail
                                        && let Fetch::Ready(items) = &pd.tracks {
                                            let display = pinned_first(
                                                filtered_sorted(items, &pd.filter, &label),
                                                &app.pinned_tracks,
                                                |t| t.uri.as_str(),
                                            );
                                            if let Some(uri) =
                                                display.get(pd.selected).map(|&(_, t)| t.uri.clone())
                                            {
                                                pins::toggle_in_place(&mut app.pinned_tracks, &uri);
                                                pins::save("tracks", &app.pinned_tracks);
                                            }
                                        }
                                }
                                KeyCode::Up => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.selected = pd.selected.saturating_sub(1);
                                    }
                                }
                                KeyCode::Down => {
                                    if let Some(pd) = &mut app.playlist_detail
                                        && let Fetch::Ready(items) = &pd.tracks {
                                            let display = pinned_first(
                                                filtered_sorted(items, &pd.filter, &label),
                                                &app.pinned_tracks,
                                                |t| t.uri.as_str(),
                                            );
                                            if !display.is_empty() {
                                                pd.selected = (pd.selected + 1).min(display.len() - 1);
                                            }
                                        }
                                }
                                // Not Right -- Enter here can play a track and jump to Now Playing,
                                // a real "leave here" side effect. Right means "go deeper," not
                                // "commit and teleport" (reported live as feeling unnatural).
                                KeyCode::Enter => {
                                    if let Some(pd) = &app.playlist_detail
                                        && let Fetch::Ready(items) = &pd.tracks {
                                            let display = pinned_first(
                                                filtered_sorted(items, &pd.filter, &label),
                                                &app.pinned_tracks,
                                                |t| t.uri.as_str(),
                                            );
                                            if let Some(&(original_index, _)) = display.get(pd.selected) {
                                                // Loads the whole playlist as
                                                // context starting at the
                                                // selected track, not just
                                                // that one track in isolation
                                                // -- n/p then walk the real
                                                // playlist, matching "play
                                                // playlist as context" in the
                                                // plan. original_index (not
                                                // pd.selected) since the real
                                                // context is indexed against
                                                // the unfiltered playlist --
                                                // filtered_sorted carries the
                                                // original position precisely
                                                // so this stays correct even
                                                // with a filter/sort active.
                                                let opts = carry_modes(app.shuffle, app.repeat, LoadRequestOptions {
                                                    playing_track: Some(PlayingTrack::Index(original_index as u32)),
                                                    ..Default::default()
                                                });
                                                let _ = spirc.activate();
                                                let _ = spirc.load(LoadRequest::from_context_uri(
                                                    pd.playlist.uri.clone(),
                                                    opts,
                                                ));
                                                let _ = spirc.play();
                                                app.context_label = Some(pd.playlist.name.clone());
                                                app.nav.goto(Screen::NowPlaying);
                                            }
                                        }
                                }
                                _ => {}
                            }
                        }
                        // The one confirmed behavior change in an
                        // otherwise styling-only pass: Help had no
                        // scrolling at all, and with 14 sections the tail
                        // was permanently unreachable on most terminals.
                        // Offset is clamped inside `render_help` itself
                        // against the real rendered height, so these
                        // handlers can move it blindly.
                        Screen::Help => match key.code {
                            KeyCode::Char('q') => break 'inner LoopExit::Quit,
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                break 'inner LoopExit::Quit
                            }
                            KeyCode::Esc | KeyCode::Left => {
                                scroll.help = 0;
                                app.nav.escape();
                            }
                            KeyCode::Char(' ') => {
                                let _ = spirc.play_pause();
                            }
                            KeyCode::Char('n') => {
                                let _ = spirc.next();
                            }
                            KeyCode::Char('p') => {
                                let _ = spirc.prev();
                            }
                            KeyCode::Char('+') => {
                                let _ = spirc.volume_up();
                            }
                            KeyCode::Char('-') => {
                                let _ = spirc.volume_down();
                            }
                            KeyCode::Char('f') => {
                                toggle_or_enter_fullscreen(&mut app);
                            }
                            KeyCode::Char('l') => {
                                scroll.help = 0;
                                app.nav.goto(Screen::Library);
                            }
                            KeyCode::Char('c') => {
                                app.text_prompt =
                                    Some(TextPrompt::new("New playlist name", "", TextPromptAction::CreatePlaylist));
                            }
                            KeyCode::Up => {
                                scroll.help = scroll.help.saturating_sub(1);
                            }
                            KeyCode::Down => {
                                scroll.help = scroll.help.saturating_add(1);
                            }
                            KeyCode::PageUp => {
                                scroll.help = scroll.help.saturating_sub(10);
                            }
                            KeyCode::PageDown => {
                                scroll.help = scroll.help.saturating_add(10);
                            }
                            _ => {}
                        },
                        Screen::Queue => match key.code {
                            KeyCode::Char('q') => break 'inner LoopExit::Quit,
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                break 'inner LoopExit::Quit
                            }
                            KeyCode::Char('?') => {
                                app.nav.push(Screen::Help);
                            }
                            KeyCode::Char(' ') => {
                                let _ = spirc.play_pause();
                            }
                            KeyCode::Char('n') => {
                                let _ = spirc.next();
                            }
                            KeyCode::Char('p') => {
                                let _ = spirc.prev();
                            }
                            KeyCode::Char('+') => {
                                let _ = spirc.volume_up();
                            }
                            KeyCode::Char('-') => {
                                let _ = spirc.volume_down();
                            }
                            KeyCode::Char('f') => {
                                toggle_or_enter_fullscreen(&mut app);
                            }
                            // Shift+L before plain `l` -- same encoding-
                            // robustness reason as every other Shift+
                            // guard in this file. Always the *like*
                            // direction -- the queue isn't a "you already
                            // liked this" list the way Liked Songs is.
                            KeyCode::Char(c) if is_shift_char(KeyCode::Char(c), key.modifiers, 'L', 'l') => {
                                let track_uri = match &app.queue.fetch {
                                    Fetch::Ready(summary) => summary.queue.get(app.queue.selected).map(|t| t.uri.clone()),
                                    _ => None,
                                };
                                if let (Some(track_uri), Some(client)) = (track_uri, spotify_client.clone()) {
                                    let tx = crud_tx.clone();
                                    tokio::spawn(async move {
                                        let result = api::library::like_track(&client, &track_uri).await;
                                        let _ = tx.send(CrudResult::LikeToggled { track_uri, liked: true, result });
                                    });
                                }
                            }
                            KeyCode::Char('l') => {
                                app.nav.goto(Screen::Library);
                            }
                            KeyCode::Char('c') => {
                                app.text_prompt =
                                    Some(TextPrompt::new("New playlist name", "", TextPromptAction::CreatePlaylist));
                            }
                            KeyCode::Esc | KeyCode::Left => {
                                app.nav.escape();
                            }
                            KeyCode::Up => {
                                app.queue.selected = app.queue.selected.saturating_sub(1);
                            }
                            KeyCode::Down => {
                                if let Fetch::Ready(summary) = &app.queue.fetch
                                    && !summary.queue.is_empty() {
                                        app.queue.selected =
                                            (app.queue.selected + 1).min(summary.queue.len() - 1);
                                    }
                            }
                            // No d/r/m here -- the public Web API has no
                            // remove or reorder endpoint for the queue at
                            // all (see api::queue's own doc comment).
                            // Adding to a playlist is the one real
                            // mutation available for a queued track.
                            KeyCode::Char('a') => {
                                if let Fetch::Ready(summary) = &app.queue.fetch
                                    && let Some(track) = summary.queue.get(app.queue.selected) {
                                        app.playlist_picker =
                                            Some(PlaylistPicker { track_uri: track.uri.clone(), selected: 0, filter: ListFilter::default() });
                                    }
                            }
                            KeyCode::Char(c) if is_shift_char(KeyCode::Char(c), key.modifiers, 'V', 'v') => {
                                let artist_uri = match &app.queue.fetch {
                                    Fetch::Ready(summary) => {
                                        summary.queue.get(app.queue.selected).map(|t| t.artist_uri.clone())
                                    }
                                    _ => None,
                                };
                                if let Some(artist_uri) = artist_uri {
                                    open_artist_detail(&mut app, &library_tx, &spotify_client, artist_uri);
                                }
                            }
                            KeyCode::Char('v') => {
                                let album_uri = match &app.queue.fetch {
                                    Fetch::Ready(summary) => {
                                        summary.queue.get(app.queue.selected).map(|t| t.album_uri.clone())
                                    }
                                    _ => None,
                                };
                                if let Some(album_uri) = album_uri {
                                    open_album_detail(&mut app, &library_tx, &spotify_client, album_uri);
                                }
                            }
                            _ => {}
                        },
                        Screen::Devices => match key.code {
                            KeyCode::Char('q') => break 'inner LoopExit::Quit,
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                break 'inner LoopExit::Quit
                            }
                            KeyCode::Char('?') => {
                                app.nav.push(Screen::Help);
                            }
                            KeyCode::Char(' ') => {
                                let _ = spirc.play_pause();
                            }
                            KeyCode::Char('n') => {
                                let _ = spirc.next();
                            }
                            KeyCode::Char('p') => {
                                let _ = spirc.prev();
                            }
                            KeyCode::Char('+') => {
                                let _ = spirc.volume_up();
                            }
                            KeyCode::Char('-') => {
                                let _ = spirc.volume_down();
                            }
                            KeyCode::Char('f') => {
                                toggle_or_enter_fullscreen(&mut app);
                            }
                            KeyCode::Char('l') => {
                                app.nav.goto(Screen::Library);
                            }
                            KeyCode::Char('c') => {
                                app.text_prompt =
                                    Some(TextPrompt::new("New playlist name", "", TextPromptAction::CreatePlaylist));
                            }
                            KeyCode::Esc | KeyCode::Left => {
                                app.nav.escape();
                            }
                            KeyCode::Char('r') => {
                                refetch_devices(&mut app, &library_tx, &spotify_client);
                            }
                            KeyCode::Up => {
                                app.devices.selected = app.devices.selected.saturating_sub(1);
                            }
                            KeyCode::Down => {
                                if let Fetch::Ready(items) = &app.devices.fetch
                                    && !items.is_empty() {
                                        app.devices.selected = (app.devices.selected + 1).min(items.len() - 1);
                                    }
                            }
                            KeyCode::Enter => {
                                let device_id = match &app.devices.fetch {
                                    Fetch::Ready(items) => items.get(app.devices.selected).map(|d| d.id.clone()),
                                    _ => None,
                                };
                                if let Some(device_id) = device_id {
                                    if let Some(client) = spotify_client.clone() {
                                        app.status = Some(("transferring playback\u{2026}".to_string(), false));
                                        let tx = crud_tx.clone();
                                        tokio::spawn(async move {
                                            let result = api::devices::transfer_to(&client, &device_id).await;
                                            let _ = tx.send(CrudResult::DeviceTransferred(result));
                                        });
                                    } else {
                                        app.status = Some(("Spotify client not ready yet".to_string(), true));
                                    }
                                }
                            }
                            _ => {}
                        },
                        Screen::ArtistDetail => match key.code {
                            KeyCode::Char('q') => break 'inner LoopExit::Quit,
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                break 'inner LoopExit::Quit
                            }
                            KeyCode::Char('?') => {
                                app.nav.push(Screen::Help);
                            }
                            KeyCode::Char(' ') => {
                                let _ = spirc.play_pause();
                            }
                            KeyCode::Char('n') => {
                                let _ = spirc.next();
                            }
                            KeyCode::Char('p') => {
                                let _ = spirc.prev();
                            }
                            KeyCode::Char('+') => {
                                let _ = spirc.volume_up();
                            }
                            KeyCode::Char('-') => {
                                let _ = spirc.volume_down();
                            }
                            // Shift+F before plain `f` (fullscreen) -- same
                            // encoding-robustness reason as every other
                            // Shift+ guard in this file. Follows the
                            // artist this screen is itself about; the
                            // reverse (unfollow) lives on the Followed
                            // Artists list screen instead.
                            KeyCode::Char(c) if is_shift_char(KeyCode::Char(c), key.modifiers, 'F', 'f') => {
                                if let Some(state) = &app.artist_detail {
                                    let artist_uri = state.artist_uri.clone();
                                    if let Some(client) = spotify_client.clone() {
                                        let tx = crud_tx.clone();
                                        tokio::spawn(async move {
                                            let result = api::library::follow_artist(&client, &artist_uri).await;
                                            let _ = tx.send(CrudResult::FollowToggled { artist_uri, followed: true, result });
                                        });
                                    }
                                }
                            }
                            KeyCode::Char('f') => {
                                toggle_or_enter_fullscreen(&mut app);
                            }
                            KeyCode::Char('l') => {
                                app.nav.goto(Screen::Library);
                            }
                            KeyCode::Char('c') => {
                                app.text_prompt =
                                    Some(TextPrompt::new("New playlist name", "", TextPromptAction::CreatePlaylist));
                            }
                            KeyCode::Esc | KeyCode::Left => {
                                app.nav.escape();
                            }
                            KeyCode::Up => {
                                if let Some(state) = &mut app.artist_detail {
                                    state.selected = state.selected.saturating_sub(1);
                                }
                            }
                            KeyCode::Down => {
                                if let Some(state) = &app.artist_detail
                                    && let Fetch::Ready(artist) = &state.detail
                                        && !artist.albums.is_empty() {
                                            let next = (state.selected + 1).min(artist.albums.len() - 1);
                                            app.artist_detail.as_mut().unwrap().selected = next;
                                        }
                            }
                            // Pure navigation, no playback -- opening an album doesn't
                            // play anything, matching the "Right = go deeper" rule.
                            KeyCode::Enter | KeyCode::Right => {
                                let album_uri = app.artist_detail.as_ref().and_then(|state| match &state.detail {
                                    Fetch::Ready(artist) => artist.albums.get(state.selected).map(|a| a.uri.clone()),
                                    _ => None,
                                });
                                if let Some(album_uri) = album_uri {
                                    open_album_detail(&mut app, &library_tx, &spotify_client, album_uri);
                                }
                            }
                            _ => {}
                        },
                        Screen::AlbumDetail => match key.code {
                            KeyCode::Char('q') => break 'inner LoopExit::Quit,
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                break 'inner LoopExit::Quit
                            }
                            KeyCode::Char('?') => {
                                app.nav.push(Screen::Help);
                            }
                            KeyCode::Char(' ') => {
                                let _ = spirc.play_pause();
                            }
                            KeyCode::Char('n') => {
                                let _ = spirc.next();
                            }
                            KeyCode::Char('p') => {
                                let _ = spirc.prev();
                            }
                            KeyCode::Char('+') => {
                                let _ = spirc.volume_up();
                            }
                            KeyCode::Char('-') => {
                                let _ = spirc.volume_down();
                            }
                            KeyCode::Char('f') => {
                                toggle_or_enter_fullscreen(&mut app);
                            }
                            // Shift+L before plain `l` -- same encoding-
                            // robustness reason as every other Shift+
                            // guard in this file. Likes the selected
                            // track, not the album -- `s` (below) is the
                            // album-level save.
                            KeyCode::Char(c) if is_shift_char(KeyCode::Char(c), key.modifiers, 'L', 'l') => {
                                let track_uri = app.album_detail.as_ref().and_then(|state| match &state.detail {
                                    Fetch::Ready(album) => album.tracks.get(state.selected).map(|t| t.uri.clone()),
                                    _ => None,
                                });
                                if let (Some(track_uri), Some(client)) = (track_uri, spotify_client.clone()) {
                                    let tx = crud_tx.clone();
                                    tokio::spawn(async move {
                                        let result = api::library::like_track(&client, &track_uri).await;
                                        let _ = tx.send(CrudResult::LikeToggled { track_uri, liked: true, result });
                                    });
                                }
                            }
                            // Add the selected track to the queue -- literal 'Q'
                            // only, see Liked Songs' arm.
                            KeyCode::Char('Q') => {
                                let track_uri = app.album_detail.as_ref().and_then(|state| match &state.detail {
                                    Fetch::Ready(album) => album.tracks.get(state.selected).map(|t| t.uri.clone()),
                                    _ => None,
                                });
                                if let Some(uri) = track_uri {
                                    fire_add_to_queue(&mut app, &crud_tx, &spotify_client, uri);
                                }
                            }
                            KeyCode::Char('l') => {
                                app.nav.goto(Screen::Library);
                            }
                            // Saves the album itself, viewed here -- the reverse
                            // (unsave) lives on the Saved Albums list screen instead.
                            KeyCode::Char('s') => {
                                if let Some(state) = &app.album_detail {
                                    let album_uri = state.album_uri.clone();
                                    if let Some(client) = spotify_client.clone() {
                                        let tx = crud_tx.clone();
                                        tokio::spawn(async move {
                                            let result = api::library::save_album(&client, &album_uri).await;
                                            let _ = tx.send(CrudResult::SaveToggled { album_uri, saved: true, result });
                                        });
                                    }
                                }
                            }
                            KeyCode::Char('c') => {
                                app.text_prompt =
                                    Some(TextPrompt::new("New playlist name", "", TextPromptAction::CreatePlaylist));
                            }
                            KeyCode::Esc | KeyCode::Left => {
                                app.nav.escape();
                            }
                            // Views the album's own (first-listed) artist -- plain `v`,
                            // not Ctrl+Right, since Album Detail has no text-input
                            // constraint stopping a normal letter key here.
                            KeyCode::Char('v') => {
                                let artist_uri =
                                    app.album_detail.as_ref().and_then(|state| match &state.detail {
                                        Fetch::Ready(album) => Some(album.artist_uri.clone()),
                                        _ => None,
                                    });
                                if let Some(artist_uri) = artist_uri {
                                    open_artist_detail(&mut app, &library_tx, &spotify_client, artist_uri);
                                }
                            }
                            KeyCode::Char('a') => {
                                let track_uri = app.album_detail.as_ref().and_then(|state| match &state.detail {
                                    Fetch::Ready(album) => album.tracks.get(state.selected).map(|t| t.uri.clone()),
                                    _ => None,
                                });
                                if let Some(track_uri) = track_uri {
                                    app.playlist_picker =
                                        Some(PlaylistPicker { track_uri, selected: 0, filter: ListFilter::default() });
                                }
                            }
                            KeyCode::Up => {
                                if let Some(state) = &mut app.album_detail {
                                    state.selected = state.selected.saturating_sub(1);
                                }
                            }
                            KeyCode::Down => {
                                if let Some(state) = &app.album_detail
                                    && let Fetch::Ready(album) = &state.detail
                                        && !album.tracks.is_empty() {
                                            let next = (state.selected + 1).min(album.tracks.len() - 1);
                                            app.album_detail.as_mut().unwrap().selected = next;
                                        }
                            }
                            // Not Right -- this plays and jumps to Now Playing, a real
                            // "leave here" side effect, same rule as everywhere else.
                            KeyCode::Enter => {
                                if let Some(state) = &app.album_detail
                                    && let Fetch::Ready(album) = &state.detail
                                        && state.selected < album.tracks.len() {
                                            let opts = carry_modes(app.shuffle, app.repeat, LoadRequestOptions {
                                                playing_track: Some(PlayingTrack::Index(state.selected as u32)),
                                                ..Default::default()
                                            });
                                            let _ = spirc.activate();
                                            let _ = spirc.load(LoadRequest::from_context_uri(album.uri.clone(), opts));
                                            let _ = spirc.play();
                                            app.context_label = Some(album.name.clone());
                                            app.nav.goto(Screen::NowPlaying);
                                        }
                            }
                            _ => {}
                        },
                    }
                }
            }
        }
    };

        let _ = spirc.shutdown();

        match exit {
            LoopExit::Quit => break 'outer,
            LoopExit::Disconnected => {
                app.track_title = None;
                app.track_artist = None;
                app.track_album = None;
                app.playing = None;
                app.lyrics = LyricsState::SessionEnded;
                terminal.draw(|f| ui::render(f, &app, &mut scroll, &mut images))?;
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }

    Ok(())
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

#[cfg(test)]
mod lyric_words_tests {
    use super::*;
    use lyrics::WordSeg;

    fn seg(text: &str, start: f64, end: f64) -> WordSeg {
        WordSeg { text: text.to_string(), start, end }
    }

    fn lines_of(state: LyricsState) -> Vec<LyricLine> {
        match state {
            LyricsState::Synced(lines) => lines,
            _ => panic!("expected Synced lyrics"),
        }
    }

    #[test]
    fn each_line_gets_its_own_words() {
        let state = to_lyrics_state(CachedLyrics::Synced {
            lines: vec![(1.0, "hi there".to_string()), (5.0, "bye".to_string())],
            words: vec![vec![seg("hi ", 1.0, 1.4), seg("there", 1.4, 2.0)], vec![seg("bye", 5.0, 5.5)]],
            credit: None,
        });
        let lines = lines_of(state);
        assert_eq!(lines[0].words, vec![seg("hi ", 1.0, 1.4), seg("there", 1.4, 2.0)]);
        assert_eq!(lines[1].words, vec![seg("bye", 5.0, 5.5)]);
    }

    #[test]
    fn a_line_level_sync_leaves_every_line_without_words() {
        let state = to_lyrics_state(CachedLyrics::Synced {
            lines: vec![(1.0, "a".to_string()), (2.0, "b".to_string())],
            words: Vec::new(),
            credit: None,
        });
        assert!(lines_of(state).iter().all(|l| l.words.is_empty()));
    }

    #[test]
    fn a_short_words_list_never_panics_or_misaligns() {
        // Defensive: a corrupt cache file with fewer word entries than lines.
        let state = to_lyrics_state(CachedLyrics::Synced {
            lines: vec![(1.0, "a".to_string()), (2.0, "b".to_string())],
            words: vec![vec![seg("a", 1.0, 1.5)]],
            credit: None,
        });
        let lines = lines_of(state);
        assert_eq!(lines[0].words.len(), 1);
        assert!(lines[1].words.is_empty());
    }
}
