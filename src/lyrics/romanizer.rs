//! Romanizing the current lyric sheet off the render thread (the first
//! Japanese line loads a ~47 MB dictionary).

use super::romanize::{self, RomanLine};
use crate::state::{AppState, LyricsState};
use std::sync::mpsc::{self, Receiver, Sender};

pub type RomanizedLines = Vec<Option<RomanLine>>;

/// Starts romanization jobs and receives their results, generation-tagged
/// like every other lyric result. Until a result arrives the native text
/// shows.
pub struct Romanizer {
    tx: Sender<(u64, RomanizedLines)>,
    pub rx: Receiver<(u64, RomanizedLines)>,
    /// The generation a job was last started for, so each track is
    /// romanized at most once.
    requested: Option<u64>,
}

impl Romanizer {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx,
            requested: None,
        }
    }

    /// Starts romanizing the current sheet if romanization is on, the sheet
    /// has CJK text, and it has not been started for this track yet.
    pub fn request(&mut self, app: &AppState, generation: u64) {
        if !app.romanize_lyrics
            || app.romanized_lines.is_some()
            || self.requested == Some(generation)
        {
            return;
        }
        let tx = self.tx.clone();
        match &app.lyrics {
            LyricsState::Synced(lines) if romanize::sheet_has_cjk(lines) => {
                self.requested = Some(generation);
                let lines = lines.clone();
                std::thread::spawn(move || {
                    let _ = tx.send((generation, romanize::romanize_lyric_lines(&lines)));
                });
            }
            // Unsynced lyrics romanize the same way, one entry per text line.
            LyricsState::Plain(text) if romanize::has_cjk(text) => {
                self.requested = Some(generation);
                let text = text.clone();
                std::thread::spawn(move || {
                    let _ = tx.send((generation, romanize::romanize_plain_lines(&text)));
                });
            }
            _ => {}
        }
    }
}

/// The status-bar text after toggling romanization. When there is nothing
/// to romanize it says why, rather than leaving a toggle that appears to do
/// nothing.
pub fn status(on: bool, lyrics: &LyricsState) -> String {
    if !on {
        return "romanized lyrics off".to_string();
    }
    let no_cjk = "romanized lyrics on (this track has no Japanese, Chinese or Korean lyrics)";
    match lyrics {
        LyricsState::Synced(lines) if romanize::sheet_has_cjk(lines) => {
            "romanized lyrics on".to_string()
        }
        LyricsState::Plain(text) if romanize::has_cjk(text) => "romanized lyrics on".to_string(),
        LyricsState::Synced(_) | LyricsState::Plain(_) => no_cjk.to_string(),
        LyricsState::Loading => "romanized lyrics on (applies when the lyrics load)".to_string(),
        _ => "romanized lyrics on (no lyrics to romanize)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::LyricLine;
    use std::time::Duration;

    fn synced(text: &str) -> LyricsState {
        LyricsState::Synced(vec![LyricLine {
            timestamp: Duration::from_secs(1),
            text: text.to_string(),
            words: Vec::new(),
        }])
    }

    #[test]
    fn turning_it_off_just_says_so() {
        assert_eq!(status(false, &synced("\u{541b}")), "romanized lyrics off");
    }

    #[test]
    fn turning_it_on_for_a_cjk_track_just_says_so() {
        assert_eq!(status(true, &synced("\u{541b}")), "romanized lyrics on");
    }

    #[test]
    fn a_track_with_nothing_to_romanize_says_why_instead_of_silently_doing_nothing() {
        let status = status(true, &synced("Stay in the middle"));
        assert!(status.starts_with("romanized lyrics on"), "{status}");
        assert!(
            status.contains("no Japanese, Chinese or Korean"),
            "{status}"
        );
    }

    #[test]
    fn unsynced_lyrics_with_cjk_romanize_just_like_synced_ones() {
        assert_eq!(
            status(true, &LyricsState::Plain("\u{541b}\nStay".to_string())),
            "romanized lyrics on"
        );
    }

    #[test]
    fn unsynced_lyrics_without_cjk_say_there_is_nothing_to_romanize() {
        let status = status(true, &LyricsState::Plain("Stay in the middle".to_string()));
        assert!(
            status.contains("no Japanese, Chinese or Korean"),
            "{status}"
        );
    }

    #[test]
    fn lyrics_still_loading_say_it_will_apply_when_they_arrive() {
        assert!(status(true, &LyricsState::Loading).contains("applies when the lyrics load"));
    }

    #[test]
    fn no_lyrics_at_all_says_there_is_nothing_to_romanize() {
        for lyrics in [
            LyricsState::NotFound,
            LyricsState::Idle,
            LyricsState::Instrumental,
        ] {
            assert!(status(true, &lyrics).contains("no lyrics to romanize"));
        }
    }
}
