use super::*;
use crate::api::library::{FollowedArtist, PlaylistSummary, SavedAlbumSummary};
use crate::api::search::TrackResult;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

pub struct AppState {
    pub track_title: Option<String>,
    pub track_artist: Option<String>,
    pub track_album: Option<String>,
    /// Set alongside the three fields above, at the same `PlayerEvent::
    /// TrackChanged` handler -- lets `render_art` know which track a
    /// cached `StatefulProtocol` cover image actually belongs to, since
    /// artist+title alone isn't a reliable cache key (two different
    /// tracks can share both).
    pub current_track_uri: Option<String>,
    /// What list/screen the currently-playing track was started from --
    /// "Liked Songs", a playlist's name, "Search", an album's name. Set
    /// at every existing "start playback" call site alongside the
    /// `LoadRequest`/`spirc.load` call, never a new one. Display-only:
    /// `n`/`p` skip within whatever context Spotify itself is already
    /// using and don't touch this field, so it correctly persists across
    /// a skip and only changes on the next deliberate play action.
    pub context_label: Option<String>,
    /// Kept in step with the player via `ShuffleChanged`/`RepeatChanged`
    /// events (which librespot also emits once at connect with the real
    /// resumed state, and when another device changes them), plus an
    /// optimistic update at the keypress so a quick second press cycles
    /// from the state just requested rather than a stale one.
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
    /// `None` = no track loaded yet (device is connected regardless --
    /// this doesn't mean the Connect session is down). `Some(true)`
    /// = playing, `Some(false)` = paused. Distinguishing these explicitly
    /// (icon + frozen-vs-advancing gauge) is what closes the "is this
    /// broken or just paused" gap a real report ran into.
    pub playing: Option<bool>,
    pub position: Duration,
    pub duration: Duration,
    /// Raw librespot volume (0..=u16::MAX), updated from `VolumeChanged`
    /// events -- reflects changes from any source, not just our own
    /// up/down keys (e.g. adjusting it from the phone shows up here too).
    pub volume: u16,
    /// The volume `m` remembered when it last muted, so it can restore
    /// exactly what was there rather than a fixed default. `None` means
    /// not currently muted (via `m`) -- survives a track change on
    /// purpose, since mute is a device-level state, not a per-track one.
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
    /// Which playlists are already known to contain which tracks --
    /// populated only from track lists this app fetched for some other
    /// reason (opening Playlist Detail, the picker's own pre-add
    /// duplicate check), plus an optimistic write-through whenever a
    /// track is actually added to or removed from a playlist through
    /// this app (the same "trust a local update, not a refetch, to
    /// reflect a just-completed write" lesson `bump_track_count` already
    /// established -- Spotify's own write-propagation lag proved a
    /// refetch unreliable for exactly this once already this session).
    /// Deliberately incomplete: a missing entry means "not known," never
    /// "confirmed absent" -- the picker's marker only ever makes a
    /// positive claim. Never consulted for the real duplicate check
    /// before an add, which stays a live fetch; a stale cache saying
    /// "already in it" must never suppress a legitimate add. Session-
    /// only, not persisted to disk (unlike pins).
    pub playlist_membership: std::collections::HashMap<String, std::collections::HashSet<String>>,
    /// Phase 5's transient overlays -- see the doc comment above
    /// `TextPrompt` for why these are sibling `Option`s here rather than
    /// `Screen` stack variants. Checked in this order (confirm gates
    /// hardest, a picker is "just" a list): only one is ever `Some` at a
    /// time in practice, but the order matters if that invariant is ever
    /// violated by a future bug -- confirm should always win.
    pub pending_confirm: Option<PendingConfirm>,
    pub text_prompt: Option<TextPrompt>,
    pub playlist_picker: Option<PlaylistPicker>,
    /// Phase 12's global quick-jump palette (`Ctrl+P`) -- a fourth
    /// sibling overlay at the same tier as the three above.
    pub quick_jump: Option<QuickJump>,
    /// Transient (message, is_error) shown in the status line, cleared on
    /// the next keypress. Every Phase 5 mutation's result -- success or
    /// failure -- surfaces here; there was no general status/toast field
    /// before this phase; every prior error surface was feature-specific
    /// (`SearchState::error`, `Fetch::Failed`).
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
    // `(index into the fetched list, item)`. Empty until the list is loaded.
    // Key handlers and renderers both read these, so "row N" can never mean
    // two different things.

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
