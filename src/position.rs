// Playback position as a two-state machine (paused at a position, or playing
// since an instant). librespot's `PlayerEvent`s carry the absolute `position_ms`,
// so every event replaces the state wholesale and nothing ever accumulates.

use librespot_playback::player::PlayerEvent;
use std::time::Instant;

enum PlaybackState {
    Paused(u32),
    Playing { position_ms: u32, since: Instant },
}

pub struct PositionTracker {
    state: PlaybackState,
    current_track_id: Option<String>,
}

impl PositionTracker {
    pub fn new() -> Self {
        Self {
            state: PlaybackState::Paused(0),
            current_track_id: None,
        }
    }

    pub fn current_track_id(&self) -> Option<&str> {
        self.current_track_id.as_deref()
    }

    pub fn is_playing(&self) -> bool {
        matches!(self.state, PlaybackState::Playing { .. })
    }

    pub fn on_event(&mut self, event: &PlayerEvent, now: Instant) {
        match event {
            PlayerEvent::Playing {
                track_id,
                position_ms,
                ..
            } => {
                self.current_track_id = Some(track_id.to_string());
                self.state = PlaybackState::Playing {
                    position_ms: *position_ms,
                    since: now,
                };
            }
            PlayerEvent::Paused {
                track_id,
                position_ms,
                ..
            }
            | PlayerEvent::Loading {
                track_id,
                position_ms,
                ..
            } => {
                self.current_track_id = Some(track_id.to_string());
                self.state = PlaybackState::Paused(*position_ms);
            }
            PlayerEvent::Stopped { track_id, .. } => {
                self.current_track_id = Some(track_id.to_string());
                self.state = PlaybackState::Paused(0);
            }
            PlayerEvent::EndOfTrack { .. } => {
                self.state = PlaybackState::Paused(0);
            }
            PlayerEvent::Seeked {
                track_id,
                position_ms,
                ..
            }
            | PlayerEvent::PositionCorrection {
                track_id,
                position_ms,
                ..
            }
            | PlayerEvent::PositionChanged {
                track_id,
                position_ms,
                ..
            } => {
                // A seek within the same track (Connect's own "prev
                // restarts the track if you're a few seconds in"
                // behavior included) doesn't fire `Playing`/`Paused` --
                // resync the anchor to the new position without forcing
                // a playing/paused transition that didn't happen.
                self.current_track_id = Some(track_id.to_string());
                self.state = match self.state {
                    PlaybackState::Playing { .. } => PlaybackState::Playing {
                        position_ms: *position_ms,
                        since: now,
                    },
                    PlaybackState::Paused(_) => PlaybackState::Paused(*position_ms),
                };
            }
            _ => {}
        }
    }

    pub fn progress_ms(&self, now: Instant) -> u32 {
        match self.state {
            PlaybackState::Paused(ms) => ms,
            PlaybackState::Playing { position_ms, since } => {
                position_ms.saturating_add(now.duration_since(since).as_millis() as u32)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_core::spotify_uri::SpotifyUri;

    // Real base62 track IDs (captured live -- see the conversation this was
    // calibrated in). SpotifyUri validates ID shape, so placeholders like
    // TRACK_A are rejected as InvalidId.
    const TRACK_A: &str = "spotify:track:4WFgvKVfEhb3IUAFGrutTR";
    const TRACK_B: &str = "spotify:track:1Fid2jjqsHViMX6xNH70hE";

    fn track_id(s: &str) -> SpotifyUri {
        SpotifyUri::from_uri(s).unwrap()
    }

    fn playing(track: &str, position_ms: u32) -> PlayerEvent {
        PlayerEvent::Playing {
            play_request_id: 0,
            track_id: track_id(track),
            position_ms,
        }
    }

    fn paused(track: &str, position_ms: u32) -> PlayerEvent {
        PlayerEvent::Paused {
            play_request_id: 0,
            track_id: track_id(track),
            position_ms,
        }
    }

    fn seeked(track: &str, position_ms: u32) -> PlayerEvent {
        PlayerEvent::Seeked {
            play_request_id: 0,
            track_id: track_id(track),
            position_ms,
        }
    }

    fn position_correction(track: &str, position_ms: u32) -> PlayerEvent {
        PlayerEvent::PositionCorrection {
            play_request_id: 0,
            track_id: track_id(track),
            position_ms,
        }
    }

    #[test]
    fn fresh_playing_advances_with_wall_clock() {
        let t0 = Instant::now();
        let mut tracker = PositionTracker::new();
        tracker.on_event(&playing(TRACK_A, 1000), t0);

        assert_eq!(tracker.progress_ms(t0), 1000);
        assert_eq!(tracker.progress_ms(t0 + std::time::Duration::from_millis(500)), 1500);
    }

    #[test]
    fn paused_freezes_regardless_of_wall_clock() {
        let t0 = Instant::now();
        let mut tracker = PositionTracker::new();
        tracker.on_event(&playing(TRACK_A, 1000), t0);
        let t_pause = t0 + std::time::Duration::from_secs(1);
        tracker.on_event(&paused(TRACK_A, 2000), t_pause);

        assert_eq!(tracker.progress_ms(t_pause), 2000);
        assert_eq!(
            tracker.progress_ms(t_pause + std::time::Duration::from_secs(10)),
            2000
        );
    }

    #[test]
    fn resume_continues_from_the_position_playing_reports_not_from_stale_state() {
        // Real captured behavior: Paused{156004} then Playing{156004} on
        // resume -- Playing's own position_ms is already correct, no
        // stale-elapsed accumulation risk like ncspot's wire format had.
        let t0 = Instant::now();
        let mut tracker = PositionTracker::new();
        tracker.on_event(&playing(TRACK_A, 100), t0);
        let t_pause = t0 + std::time::Duration::from_secs(5);
        tracker.on_event(&paused(TRACK_A, 5100), t_pause);
        let t_resume = t_pause + std::time::Duration::from_secs(20);
        tracker.on_event(&playing(TRACK_A, 5100), t_resume);

        assert_eq!(
            tracker.progress_ms(t_resume + std::time::Duration::from_millis(500)),
            5600
        );
    }

    #[test]
    fn end_of_track_resets_to_zero() {
        let t0 = Instant::now();
        let mut tracker = PositionTracker::new();
        tracker.on_event(&playing(TRACK_A, 1000), t0);
        tracker.on_event(
            &PlayerEvent::EndOfTrack {
                play_request_id: 0,
                track_id: track_id(TRACK_A),
            },
            t0,
        );

        assert_eq!(tracker.progress_ms(t0), 0);
    }

    #[test]
    fn tracks_current_track_id() {
        let t0 = Instant::now();
        let mut tracker = PositionTracker::new();
        assert_eq!(tracker.current_track_id(), None);
        tracker.on_event(&playing(TRACK_A, 0), t0);
        assert_eq!(tracker.current_track_id(), Some(TRACK_A));
    }

    #[test]
    fn track_change_is_not_confused_with_a_stale_position() {
        let t0 = Instant::now();
        let mut tracker = PositionTracker::new();
        tracker.on_event(&playing(TRACK_A, 200_000), t0);
        let t_next = t0 + std::time::Duration::from_secs(1);
        tracker.on_event(&playing(TRACK_B, 0), t_next);

        assert_eq!(tracker.current_track_id(), Some(TRACK_B));
        assert_eq!(tracker.progress_ms(t_next), 0);
    }

    #[test]
    fn seek_while_playing_resets_the_position_anchor_without_stopping_playback() {
        // `p` mid-track restarts the track (Connect's "prev restarts if you are
        // a few seconds in") via a seek to 0, and librespot emits `Seeked`, not
        // a fresh `Playing`. Ignoring `Seeked` left the old wall-clock anchor
        // ticking as if nothing happened until the next pause.
        let t0 = Instant::now();
        let mut tracker = PositionTracker::new();
        tracker.on_event(&playing(TRACK_A, 150_000), t0);
        let t_seek = t0 + std::time::Duration::from_secs(1);
        tracker.on_event(&seeked(TRACK_A, 0), t_seek);

        assert!(tracker.is_playing());
        assert_eq!(tracker.progress_ms(t_seek), 0);
        assert_eq!(
            tracker.progress_ms(t_seek + std::time::Duration::from_millis(500)),
            500
        );
    }

    #[test]
    fn seek_while_paused_updates_position_without_starting_playback() {
        let t0 = Instant::now();
        let mut tracker = PositionTracker::new();
        tracker.on_event(&paused(TRACK_A, 50_000), t0);
        tracker.on_event(&seeked(TRACK_A, 10_000), t0);

        assert!(!tracker.is_playing());
        assert_eq!(tracker.progress_ms(t0), 10_000);
    }

    #[test]
    fn position_correction_resyncs_the_same_way_as_seeked() {
        let t0 = Instant::now();
        let mut tracker = PositionTracker::new();
        tracker.on_event(&playing(TRACK_A, 150_000), t0);
        let t_corr = t0 + std::time::Duration::from_secs(1);
        tracker.on_event(&position_correction(TRACK_A, 90_000), t_corr);

        assert!(tracker.is_playing());
        assert_eq!(tracker.progress_ms(t_corr), 90_000);
    }

    #[test]
    fn is_playing_reflects_state() {
        let t0 = Instant::now();
        let mut tracker = PositionTracker::new();
        assert!(!tracker.is_playing());
        tracker.on_event(&playing(TRACK_A, 0), t0);
        assert!(tracker.is_playing());
        tracker.on_event(&paused(TRACK_A, 0), t0);
        assert!(!tracker.is_playing());
    }
}
