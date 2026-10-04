use super::*;
use crate::api::library::{FollowedArtist, PlaylistSummary, SavedAlbumSummary};
use crate::api::search::TrackResult;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

pub struct AppState {
    pub track_title: Option<String>,
    pub track_artist: Option<String>,
    pub track_album: Option<String>,
    /// Set with the three fields above. The cover cache is keyed on it,
    /// since artist and title alone can collide.
    pub current_track_uri: Option<String>,
    /// Where playback was started from: "Liked Songs", a playlist or album
    /// name, "Search". Display only. `n`/`p` skip within Spotify's own
    /// context and leave it alone, so it changes only on the next
    /// deliberate play.
    pub context_label: Option<String>,
    /// Follows the player's `ShuffleChanged`/`RepeatChanged` events (also
    /// emitted at connect and when another device changes them), plus an
    /// optimistic update at the keypress so a quick second press cycles from
    /// what was just requested.
    pub shuffle: bool,
    /// Smart shuffle: shuffle with Spotify's recommendations mixed in. A
    /// third state of `shuffle`, read from librespot's connect state, and
    /// only ever true while `shuffle` is.
    pub smart_shuffle: bool,
    pub repeat: RepeatMode,
    pub lyrics: LyricsState,
    /// "Where these lyrics came from" (e.g. `Apple Music via Spicy Lyrics`),
    /// shown dim under the lyrics. `Some` only for a result whose source
    /// asks to be credited; cleared with `lyrics` on every track change.
    pub lyrics_credit: Option<String>,
    /// Show lyrics romanized (`t`). Global: applies to every track with
    /// Japanese, Chinese or Korean lyrics until toggled off.
    pub romanize_lyrics: bool,
    /// The current sheet's romanization, in step with its lines, once it has
    /// been computed off the render thread (`None` until then, and for a
    /// sheet with nothing to romanize). Cleared with `lyrics` on track change.
    pub romanized_lines: Option<Vec<Option<crate::lyrics::romanize::RomanLine>>>,
    pub current_line: Option<usize>,
    pub fullscreen: bool,
    /// `None`: no track loaded (not the same as disconnected). `Some(true)`
    /// playing, `Some(false)` paused, so "broken" and "paused" look different.
    pub playing: Option<bool>,
    pub position: Duration,
    pub duration: Duration,
    /// Raw librespot volume (0..=u16::MAX), updated from `VolumeChanged`
    /// events -- reflects changes from any source, not just our own
    /// up/down keys (e.g. adjusting it from the phone shows up here too).
    pub volume: u16,
    /// The volume to restore when `m` unmutes; `None` when not muted. Kept
    /// across track changes, since mute is a device state.
    pub muted_volume: Option<u16>,
    pub nav: Nav,
    pub sidebar_sel: usize,
    pub search: SearchState,
    pub library: LibraryState,
    pub queue: QueueState,
    pub devices: DevicesState,
    pub playlist_detail: Option<PlaylistDetailState>,
    pub artist_detail: Option<ArtistDetailState>,
    pub album_detail: Option<AlbumDetailState>,
    pub pinned_playlists: std::collections::HashSet<String>,
    pub pinned_tracks: std::collections::HashSet<String>,
    /// Which playlists are known to contain which tracks, for the picker's
    /// membership marker. Filled from track lists fetched for other reasons,
    /// and written through when this app adds or removes a track (Spotify
    /// reads lag writes, so a refetch can't be trusted to show it).
    ///
    /// Deliberately incomplete: a missing entry means "unknown", never
    /// "absent", so the marker only ever makes a positive claim. The
    /// duplicate check before an add always fetches live, so a stale entry
    /// can never suppress a legitimate add. In-memory only, unlike pins.
    pub playlist_membership: std::collections::HashMap<String, std::collections::HashSet<String>>,
    /// Transient overlays; see the note above `TextPrompt`. At most one is
    /// open in practice, but they are checked in this order, so confirm
    /// wins if that ever stops being true.
    pub pending_confirm: Option<PendingConfirm>,
    pub text_prompt: Option<TextPrompt>,
    pub playlist_picker: Option<PlaylistPicker>,
    /// The quick-jump palette (`Ctrl+P`), a fourth overlay.
    pub quick_jump: Option<QuickJump>,
    /// `(message, is_error)` for the status line, cleared on the next key
    /// press. Every mutation's outcome surfaces here.
    pub status: Option<(String, bool)>,
}


pub fn track_label(t: &TrackResult) -> String {
    format!("{} \u{2014} {}", t.artist, t.title)
}

pub fn album_label(a: &SavedAlbumSummary) -> String {
    format!("{} \u{2014} {}", a.name, a.artist)
}

pub fn artist_label(a: &FollowedArtist) -> String {
    a.name.clone()
}

pub fn playlist_label(p: &PlaylistSummary) -> String {
    format!("{} ({} tracks)", p.name, p.track_count)
}

impl AppState {
    pub fn new(
        romanize_lyrics: bool,
        pinned_playlists: HashSet<String>,
        pinned_tracks: HashSet<String>,
        initial_volume: u16,
    ) -> Self {
        Self {
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
            romanize_lyrics,
            romanized_lines: None,
            current_line: None,
            fullscreen: false,
            playing: None,
            position: Duration::ZERO,
            duration: Duration::ZERO,
            volume: initial_volume,
            muted_volume: None,
            nav: Nav::new(),
            sidebar_sel: 0,
            library: LibraryState::new(),
            queue: QueueState::new(),
            devices: DevicesState::new(),
            playlist_detail: None,
            artist_detail: None,
            album_detail: None,
            pinned_playlists,
            pinned_tracks,
            playlist_membership: HashMap::new(),
            search: SearchState::new(),
            pending_confirm: None,
            text_prompt: None,
            playlist_picker: None,
            quick_jump: None,
            status: None,
        }
    }

    // The rows each list screen shows, in display order, as
    // `(index into the fetched list, item)`; empty until loaded. Key handlers
    // read these, so they must match what the renderer shows.

    pub fn liked_display(&self) -> Vec<(usize, &TrackResult)> {
        match &self.library.liked_songs {
            Fetch::Ready(items) => filtered_sorted(items, &self.library.liked_songs_filter, &track_label),
            _ => Vec::new(),
        }
    }

    pub fn saved_albums_display(&self) -> Vec<(usize, &SavedAlbumSummary)> {
        match &self.library.saved_albums {
            Fetch::Ready(items) => filtered_sorted(items, &self.library.saved_albums_filter, &album_label),
            _ => Vec::new(),
        }
    }

    pub fn followed_artists_display(&self) -> Vec<(usize, &FollowedArtist)> {
        match &self.library.followed_artists {
            Fetch::Ready(items) => filtered_sorted(items, &self.library.followed_artists_filter, &artist_label),
            _ => Vec::new(),
        }
    }

    /// Pinned playlists first.
    pub fn playlists_display(&self) -> Vec<(usize, &PlaylistSummary)> {
        match &self.library.playlists {
            Fetch::Ready(items) => pinned_first(
                filtered_sorted(items, &self.library.playlists_filter, &playlist_label),
                &self.pinned_playlists,
                |p| p.uri.as_str(),
            ),
            _ => Vec::new(),
        }
    }

    /// Pinned tracks first, except during move mode, which needs display
    /// position to equal array position (see `PlaylistDetailState::move_mode`).
    pub fn playlist_detail_display(&self) -> Vec<(usize, &TrackResult)> {
        let Some(pd) = &self.playlist_detail else { return Vec::new() };
        match &pd.tracks {
            Fetch::Ready(items) => {
                let natural = filtered_sorted(items, &pd.filter, &track_label);
                if pd.move_mode.is_some() {
                    natural
                } else {
                    pinned_first(natural, &self.pinned_tracks, |t| t.uri.as_str())
                }
            }
            _ => Vec::new(),
        }
    }

    /// The track under the cursor on the current screen, if it is a screen
    /// that lists tracks.
    pub fn selected_track(&self) -> Option<TrackResult> {
        match self.nav.top() {
            Screen::LikedSongs => self.liked_display().get(self.library.liked_songs_selected).map(|&(_, t)| t.clone()),
            Screen::PlaylistDetail => {
                let selected = self.playlist_detail.as_ref()?.selected;
                self.playlist_detail_display().get(selected).map(|&(_, t)| t.clone())
            }
            Screen::Search => self.search.results.get(self.search.selected).cloned(),
            Screen::Queue => match &self.queue.fetch {
                Fetch::Ready(summary) => summary.queue.get(self.queue.selected).cloned(),
                _ => None,
            },
            Screen::AlbumDetail => {
                let state = self.album_detail.as_ref()?;
                match &state.detail {
                    Fetch::Ready(album) => album.tracks.get(state.selected).cloned(),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The playlists the add-to-playlist picker shows, pinned first.
    pub fn picker_display(&self) -> Vec<(usize, &PlaylistSummary)> {
        let (Some(picker), Fetch::Ready(items)) = (&self.playlist_picker, &self.library.playlists) else {
            return Vec::new();
        };
        let label = |p: &PlaylistSummary| p.name.clone();
        pinned_first(filtered_sorted(items, &picker.filter, &label), &self.pinned_playlists, |p| p.uri.as_str())
    }

    /// The entries the quick-jump palette currently shows.
    pub fn quick_jump_matches(&self) -> Vec<QuickJumpEntry> {
        let Some(qj) = &self.quick_jump else { return Vec::new() };
        let entries = quick_jump_entries(self, &qj.filter);
        let label = |e: &QuickJumpEntry| e.label.clone();
        filtered_sorted(&entries, &qj.filter, &label).into_iter().map(|(_, e)| e.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(uri: &str, title: &str) -> TrackResult {
        TrackResult {
            uri: uri.to_string(),
            title: title.to_string(),
            artist: "artist".into(),
            album: "album".into(),
            artist_uri: format!("{uri}:artist"),
            album_uri: format!("{uri}:album"),
        }
    }

    fn app_on_playlist(tracks: Vec<TrackResult>, pinned: &[&str]) -> AppState {
        let mut app =
            AppState::new(false, HashSet::new(), pinned.iter().map(|s| s.to_string()).collect(), 0);
        app.playlist_detail = Some(PlaylistDetailState {
            playlist: PlaylistSummary { uri: "p".into(), name: "p".into(), track_count: tracks.len() as u32 },
            tracks: Fetch::Ready(tracks),
            selected: 0,
            filter: ListFilter::default(),
            move_mode: None,
        });
        app.nav.push(Screen::PlaylistDetail);
        app
    }

    #[test]
    fn pinned_tracks_come_first_in_the_playlist_display() {
        let app = app_on_playlist(vec![track("a", "A"), track("b", "B"), track("c", "C")], &["c"]);
        let order: Vec<_> = app.playlist_detail_display().iter().map(|&(i, _)| i).collect();
        assert_eq!(order, vec![2, 0, 1]);
    }

    #[test]
    fn move_mode_keeps_array_order_even_with_a_pin() {
        let mut app = app_on_playlist(vec![track("a", "A"), track("b", "B"), track("c", "C")], &["c"]);
        app.playlist_detail.as_mut().unwrap().move_mode = Some(0);
        let order: Vec<_> = app.playlist_detail_display().iter().map(|&(i, _)| i).collect();
        assert_eq!(order, vec![0, 1, 2]);
    }

    #[test]
    fn the_selected_track_is_the_display_row_not_the_array_index() {
        let mut app = app_on_playlist(vec![track("a", "A"), track("b", "B"), track("c", "C")], &["c"]);
        app.playlist_detail.as_mut().unwrap().selected = 0;
        assert_eq!(app.selected_track().unwrap().uri, "c");
    }

    #[test]
    fn a_filter_keeps_the_original_index_for_playback() {
        let mut app = app_on_playlist(vec![track("a", "alpha"), track("b", "beta")], &[]);
        app.playlist_detail.as_mut().unwrap().filter.query = "beta".into();
        let display = app.playlist_detail_display();
        assert_eq!(display.len(), 1);
        assert_eq!(display[0].0, 1);
    }

    #[test]
    fn no_track_is_selected_before_the_list_has_loaded() {
        let mut app = AppState::new(false, HashSet::new(), HashSet::new(), 0);
        app.nav.push(Screen::LikedSongs);
        assert!(app.selected_track().is_none());
        assert!(app.liked_display().is_empty());
    }

    #[test]
    fn screens_that_list_no_tracks_have_no_selected_track() {
        let app = AppState::new(false, HashSet::new(), HashSet::new(), 0);
        assert!(app.selected_track().is_none());
    }
}
