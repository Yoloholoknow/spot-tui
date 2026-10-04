//! Lyrics: the data types every source produces, plus one submodule per
//! source (`lrclib`, `spotify`, `spicy`, `ytmusic`), the shared `cache`,
//! LRC parsing, and romanization of CJK text.

mod cache;
mod lrc;
mod lrclib;
pub mod pipeline;
pub mod romanize;
pub mod romanizer;
pub mod spicy;
mod spotify;
pub mod ytmusic;

pub use cache::{cached_synced, spicy_cache_key, store_synced};
pub use lrclib::LyricsClient;
pub use spotify::spotify_lyrics;

use std::time::Duration;

/// One timed piece of a lyric line -- a word, or a syllable of one -- for
/// word-by-word highlighting. `text` carries its own trailing space, so a
/// line's segments concatenate to exactly the line's text (the renderer
/// relies on that: styling per segment must never change what is drawn or
/// how it wraps). Times are seconds from the start of the track.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WordSeg {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LyricLine {
    pub timestamp: Duration,
    pub text: String,
    /// Word timing for word-by-word highlighting; empty when this line is
    /// only timed as a whole (every source but a Spicy Lyrics syllable sync).
    pub words: Vec<WordSeg>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum CachedLyrics {
    /// `credit` is the "where these came from" line shown under the lyrics (Spicy
    /// Lyrics asks for its uploaders to be credited). Optional in the file so older
    /// cache entries still load, and omitted when absent so other sources' files are
    /// unchanged.
    Synced {
        lines: Vec<(f64, String)>,
        /// Word timing, parallel to `lines` (one entry per line), for
        /// word-by-word highlighting; empty when the sync is line-level
        /// only, which is every source but a Spicy Lyrics syllable sync.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        words: Vec<Vec<WordSeg>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credit: Option<String>,
    },
    Plain {
        text: String,
    },
    Instrumental,
    NotFound,
}

/// Index of the currently-active line: the last line whose timestamp has
/// already passed. `lines` must be sorted ascending (guaranteed by
/// `parse_lrc`'s output and by how callers reconstruct it from the cache).
pub fn current_line_index(lines: &[LyricLine], position: Duration) -> Option<usize> {
    match lines.partition_point(|l| l.timestamp <= position) {
        0 => None,
        n => Some(n - 1),
    }
}

#[cfg(test)]
mod current_line_tests {
    use super::*;

    fn lines() -> Vec<LyricLine> {
        vec![
            LyricLine {
                timestamp: Duration::from_secs(10),
                text: "a".into(),
                words: Vec::new(),
            },
            LyricLine {
                timestamp: Duration::from_secs(20),
                text: "b".into(),
                words: Vec::new(),
            },
            LyricLine {
                timestamp: Duration::from_secs(30),
                text: "c".into(),
                words: Vec::new(),
            },
        ]
    }

    #[test]
    fn before_first_line_is_none() {
        assert_eq!(current_line_index(&lines(), Duration::from_secs(5)), None);
    }

    #[test]
    fn exactly_on_a_timestamp_selects_that_line() {
        assert_eq!(
            current_line_index(&lines(), Duration::from_secs(20)),
            Some(1)
        );
    }

    #[test]
    fn between_timestamps_selects_the_earlier_line() {
        assert_eq!(
            current_line_index(&lines(), Duration::from_secs(25)),
            Some(1)
        );
    }

    #[test]
    fn after_last_line_selects_the_last_line() {
        assert_eq!(
            current_line_index(&lines(), Duration::from_secs(999)),
            Some(2)
        );
    }

    #[test]
    fn empty_lines_is_always_none() {
        assert_eq!(current_line_index(&[], Duration::from_secs(5)), None);
    }
}
