use crate::lyrics::{CachedLyrics, LyricLine};
use std::time::Duration;

/// Repeat as the three states a person cycles through (off / the whole
/// album or playlist / this song), collapsed from the player's two
/// independent flags, whose four combinations only mean three things.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RepeatMode {
    #[default]
    Off,
    Context,
    Track,
}

impl RepeatMode {
    /// The track flag wins: repeat-one is on whenever it's set, whether or
    /// not the context flag came along with it.
    pub fn from_flags(context: bool, track: bool) -> Self {
        if track {
            RepeatMode::Track
        } else if context {
            RepeatMode::Context
        } else {
            RepeatMode::Off
        }
    }

    pub fn next(self) -> Self {
        match self {
            RepeatMode::Off => RepeatMode::Context,
            RepeatMode::Context => RepeatMode::Track,
            RepeatMode::Track => RepeatMode::Off,
        }
    }

    /// `(repeat context, repeat track)`. Repeat-one sets both, matching how
    /// Spotify's own clients report it.
    pub fn flags(self) -> (bool, bool) {
        match self {
            RepeatMode::Off => (false, false),
            RepeatMode::Context => (true, false),
            RepeatMode::Track => (true, true),
        }
    }

    pub fn status_label(self) -> &'static str {
        match self {
            RepeatMode::Off => "off",
            RepeatMode::Context => "album/playlist",
            RepeatMode::Track => "this song",
        }
    }
}

/// The three states the `s` key cycles through, mirroring Spotify's own
/// shuffle button: off, shuffle, smart shuffle. Smart shuffle is shuffle plus
/// a `context_enhancement` mode, so it always implies shuffle is on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShuffleMode {
    #[default]
    Off,
    On,
    Smart,
}

impl ShuffleMode {
    pub fn from_flags(shuffle: bool, smart: bool) -> Self {
        match (shuffle, smart) {
            (false, _) => Self::Off,
            (true, false) => Self::On,
            (true, true) => Self::Smart,
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::On,
            Self::On => Self::Smart,
            Self::Smart => Self::Off,
        }
    }

    pub fn shuffle(self) -> bool {
        self != Self::Off
    }

    /// Status-bar text after switching to this mode.
    pub fn status_label(self) -> &'static str {
        match self {
            Self::Off => "shuffle off",
            Self::On => "shuffle on",
            Self::Smart => "smart shuffle on",
        }
    }
}

/// The playbar's shuffle and repeat toggles. Every glyph is drawn in every
/// state; the `bool` says whether it is on (accent) or off (dim), and the
/// caller colours it. The readout is a fixed width so the title's truncation
/// point doesn't jump as modes change. Smart shuffle is a state of shuffle,
/// not a separate toggle: its sparkle sits in the gap between the two
/// toggles and only lights while shuffle is on.
pub fn playback_modes(shuffle: bool, smart: bool, repeat: RepeatMode) -> [(&'static str, bool); 3] {
    let repeat_glyph = match repeat {
        RepeatMode::Track => "\u{21bb}1",
        RepeatMode::Off | RepeatMode::Context => "\u{21bb} ",
    };
    let smart = shuffle && smart;
    [
        ("\u{21c4}", shuffle),
        (if smart { "\u{2726}" } else { " " }, smart),
        (repeat_glyph, repeat != RepeatMode::Off),
    ]
}

/// Cells `playback_modes` always occupies: shuffle (1), the smart-shuffle
/// gap (1), repeat (2).
pub const PLAYBACK_MODES_WIDTH: usize = 4;

pub enum LyricsState {
    Idle,
    /// The Connect session dropped (network, sleep) and is being
    /// reconnected. Distinct from `Idle` so a real drop is never mistaken
    /// for "not connected yet".
    SessionEnded,
    Loading,
    Synced(Vec<LyricLine>),
    Plain(String),
    Instrumental,
    NotFound,
}

#[cfg(test)]
mod repeat_mode_tests {
    use super::*;

    #[test]
    fn flags_map_to_the_mode_the_player_is_really_in() {
        assert_eq!(RepeatMode::from_flags(false, false), RepeatMode::Off);
        assert_eq!(RepeatMode::from_flags(true, false), RepeatMode::Context);
        assert_eq!(RepeatMode::from_flags(true, true), RepeatMode::Track);
    }

    #[test]
    fn a_track_flag_alone_still_means_repeat_song() {
        // Another device (or a mid-toggle event) can report the track flag
        // without the context flag; the track flag wins either way.
        assert_eq!(RepeatMode::from_flags(false, true), RepeatMode::Track);
    }

    #[test]
    fn next_cycles_off_album_song_off() {
        assert_eq!(RepeatMode::Off.next(), RepeatMode::Context);
        assert_eq!(RepeatMode::Context.next(), RepeatMode::Track);
        assert_eq!(RepeatMode::Track.next(), RepeatMode::Off);
    }

    #[test]
    fn each_mode_survives_a_round_trip_through_its_own_flags() {
        for mode in [RepeatMode::Off, RepeatMode::Context, RepeatMode::Track] {
            let (context, track) = mode.flags();
            assert_eq!(RepeatMode::from_flags(context, track), mode);
        }
    }
}

#[cfg(test)]
mod shuffle_mode_tests {
    use super::*;

    #[test]
    fn the_flags_map_to_the_three_states() {
        assert_eq!(ShuffleMode::from_flags(false, false), ShuffleMode::Off);
        assert_eq!(ShuffleMode::from_flags(true, false), ShuffleMode::On);
        assert_eq!(ShuffleMode::from_flags(true, true), ShuffleMode::Smart);
    }

    #[test]
    fn smart_without_shuffle_is_just_off() {
        // A stale smart flag can't outlive shuffle itself.
        assert_eq!(ShuffleMode::from_flags(false, true), ShuffleMode::Off);
    }

    #[test]
    fn the_key_cycles_off_then_shuffle_then_smart_then_off() {
        assert_eq!(ShuffleMode::Off.next(), ShuffleMode::On);
        assert_eq!(ShuffleMode::On.next(), ShuffleMode::Smart);
        assert_eq!(ShuffleMode::Smart.next(), ShuffleMode::Off);
    }

    #[test]
    fn a_full_cycle_returns_to_the_start() {
        let start = ShuffleMode::On;
        assert_eq!(start.next().next().next(), start);
    }

    #[test]
    fn smart_shuffle_implies_shuffle() {
        assert!(!ShuffleMode::Off.shuffle());
        assert!(ShuffleMode::On.shuffle());
        assert!(ShuffleMode::Smart.shuffle());
    }

    #[test]
    fn status_labels_name_the_mode() {
        assert_eq!(ShuffleMode::Off.status_label(), "shuffle off");
        assert_eq!(ShuffleMode::On.status_label(), "shuffle on");
        assert!(ShuffleMode::Smart.status_label().starts_with("smart shuffle on"));
    }
}

#[cfg(test)]
mod playback_modes_tests {
    use super::*;

    #[test]
    fn both_glyphs_are_always_present_even_with_everything_off() {
        let [shuffle, smart, repeat] = playback_modes(false, false, RepeatMode::Off);
        assert_eq!(shuffle, ("\u{21c4}", false));
        assert_eq!(smart, (" ", false));
        assert_eq!(repeat, ("\u{21bb} ", false));
    }

    #[test]
    fn shuffle_lights_up_on_its_own() {
        let [shuffle, smart, repeat] = playback_modes(true, false, RepeatMode::Off);
        assert!(shuffle.1);
        assert!(!smart.1);
        assert!(!repeat.1);
    }

    #[test]
    fn smart_shuffle_fills_the_gap_between_the_toggles_with_a_lit_sparkle() {
        let [shuffle, smart, _] = playback_modes(true, true, RepeatMode::Off);
        assert!(shuffle.1, "smart shuffle is still shuffle");
        assert_eq!(smart, ("\u{2726}", true));
    }

    #[test]
    fn the_sparkle_needs_shuffle_itself_to_be_on() {
        // A stale smart flag with shuffle off must not draw a sparkle.
        let [_, smart, _] = playback_modes(false, true, RepeatMode::Off);
        assert_eq!(smart, (" ", false));
    }

    #[test]
    fn repeat_album_and_repeat_song_are_both_active_but_only_song_gets_the_one() {
        let [_, _, context] = playback_modes(false, false, RepeatMode::Context);
        let [_, _, track] = playback_modes(false, false, RepeatMode::Track);
        assert_eq!(context, ("\u{21bb} ", true));
        assert_eq!(track, ("\u{21bb}1", true));
    }

    #[test]
    fn the_readout_is_the_same_width_in_every_state() {
        // A fixed-width readout is what lets the playbar reserve its room
        // once -- otherwise the track title's truncation point would jump
        // every time shuffle or repeat changed.
        let width = |shuffle, smart, repeat| {
            playback_modes(shuffle, smart, repeat).iter().map(|(text, _)| text.chars().count()).sum::<usize>()
        };
        let expected = width(false, false, RepeatMode::Off);
        for shuffle in [false, true] {
            for smart in [false, true] {
                for repeat in [RepeatMode::Off, RepeatMode::Context, RepeatMode::Track] {
                    assert_eq!(
                        width(shuffle, smart, repeat),
                        expected,
                        "shuffle={shuffle} smart={smart} repeat={repeat:?}"
                    );
                }
            }
        }
    }
}


impl From<CachedLyrics> for LyricsState {
    fn from(cached: CachedLyrics) -> Self {
        match cached {
            CachedLyrics::Synced { lines, words, .. } => LyricsState::Synced(
                lines
                    .into_iter()
                    .enumerate()
                    .map(|(i, (secs, text))| LyricLine {
                        // Cached data came from a remote source; a bad value becomes 0
                        // rather than a panic.
                        timestamp: Duration::try_from_secs_f64(secs).unwrap_or_default(),
                        text,
                        // `words` runs parallel to `lines`; empty (or short,
                        // if a cache file is corrupt) means no word timing.
                        words: words.get(i).cloned().unwrap_or_default(),
                    })
                    .collect(),
            ),
            CachedLyrics::Plain { text } => LyricsState::Plain(text),
            CachedLyrics::Instrumental => LyricsState::Instrumental,
            CachedLyrics::NotFound => LyricsState::NotFound,
        }
    }
}

#[cfg(test)]
mod lyric_words_tests {
    use super::*;
    use crate::lyrics::WordSeg;

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
    fn a_corrupt_timestamp_never_panics() {
        for bad in [f64::NAN, f64::INFINITY, -1.0, 1e300] {
            let state = LyricsState::from(CachedLyrics::Synced {
                lines: vec![(bad, "x".to_string())],
                words: Vec::new(),
                credit: None,
            });
            assert_eq!(lines_of(state)[0].timestamp, Duration::ZERO);
        }
    }

    #[test]
    fn each_line_gets_its_own_words() {
        let state = LyricsState::from(CachedLyrics::Synced {
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
        let state = LyricsState::from(CachedLyrics::Synced {
            lines: vec![(1.0, "a".to_string()), (2.0, "b".to_string())],
            words: Vec::new(),
            credit: None,
        });
        assert!(lines_of(state).iter().all(|l| l.words.is_empty()));
    }

    #[test]
    fn a_short_words_list_never_panics_or_misaligns() {
        // Defensive: a corrupt cache file with fewer word entries than lines.
        let state = LyricsState::from(CachedLyrics::Synced {
            lines: vec![(1.0, "a".to_string()), (2.0, "b".to_string())],
            words: vec![vec![seg("a", 1.0, 1.5)]],
            credit: None,
        });
        let lines = lines_of(state);
        assert_eq!(lines[0].words.len(), 1);
        assert!(lines[1].words.is_empty());
    }
}

