use super::*;
use crate::api::search::TrackResult;

/// The Connect queue (Phase 7). Kept separate from `LibraryState` --
/// unlike Liked Songs/Saved Albums/etc, this reflects live playback
/// state that changes on its own even when this app hasn't done
/// anything (the current track finishes, another device skips ahead),
/// so it's periodically refetched while visible rather than fetched once
/// and cached indefinitely -- see `main.rs`'s own polling logic.
pub struct QueueState {
    pub fetch: Fetch<crate::api::queue::QueueSummary>,
    pub selected: usize,
}

impl QueueState {
    pub fn new() -> Self {
        Self { fetch: Fetch::NotStarted, selected: 0 }
    }
}

impl Default for QueueState {
    fn default() -> Self {
        Self::new()
    }
}

/// Connect devices (Phase 8). Fetched once on entry (like the Library
/// lists), not periodically like `QueueState` -- a device coming online
/// or offline is a discrete, comparatively rare event, not something
/// changing every few seconds during normal use. `r` refetches manually.
pub struct DevicesState {
    pub fetch: Fetch<Vec<crate::api::devices::DeviceSummary>>,
    pub selected: usize,
}

impl DevicesState {
    pub fn new() -> Self {
        Self { fetch: Fetch::NotStarted, selected: 0 }
    }
}

impl Default for DevicesState {
    fn default() -> Self {
        Self::new()
    }
}

pub struct LibraryState {
    pub home_selected: usize,
    pub liked_songs: Fetch<Vec<TrackResult>>,
    pub saved_albums: Fetch<Vec<crate::api::library::SavedAlbumSummary>>,
    pub followed_artists: Fetch<Vec<crate::api::library::FollowedArtist>>,
    pub playlists: Fetch<Vec<crate::api::library::PlaylistSummary>>,
    pub liked_songs_selected: usize,
    pub saved_albums_selected: usize,
    pub followed_artists_selected: usize,
    pub playlists_selected: usize,
    pub liked_songs_filter: ListFilter,
    pub saved_albums_filter: ListFilter,
    pub followed_artists_filter: ListFilter,
    pub playlists_filter: ListFilter,
}

impl LibraryState {
    pub fn new() -> Self {
        Self {
            home_selected: 0,
            liked_songs: Fetch::NotStarted,
            saved_albums: Fetch::NotStarted,
            followed_artists: Fetch::NotStarted,
            playlists: Fetch::NotStarted,
            liked_songs_selected: 0,
            saved_albums_selected: 0,
            followed_artists_selected: 0,
            playlists_selected: 0,
            liked_songs_filter: ListFilter::default(),
            saved_albums_filter: ListFilter::default(),
            followed_artists_filter: ListFilter::default(),
            playlists_filter: ListFilter::default(),
        }
    }
}

impl Default for LibraryState {
    fn default() -> Self {
        Self::new()
    }
}

/// Which playlist `Screen::PlaylistDetail` is currently showing, and its
/// fetch state. A single `Option` rather than a per-playlist cache --
/// only one can be on top of the stack at a time, matching `Nav`'s own
/// one-at-a-time `top()`. Fetch results carry the playlist's URI so a
/// stale in-flight fetch from a playlist the user has since backed out
/// of can't overwrite whichever one is showing now.
pub struct PlaylistDetailState {
    pub playlist: crate::api::library::PlaylistSummary,
    pub tracks: Fetch<Vec<TrackResult>>,
    pub selected: usize,
    pub filter: ListFilter,
    /// `Some(start_index)` while move-mode (Phase 6, `m`) is active --
    /// the index the moving track started at, so confirming (`Enter`)
    /// knows the net displacement regardless of how many times it moved
    /// up and down in between.
    pub move_mode: Option<usize>,
}

/// Phase 9, read-only, no filter/sort/pin concept -- just enough state
/// to show one artist's albums and remember which real artist this is,
/// so a stale in-flight fetch from an artist backed out of can't
/// overwrite whichever one is showing now (same guard idiom
/// `PlaylistDetailState` already uses).
pub struct ArtistDetailState {
    pub artist_uri: String,
    pub detail: Fetch<crate::api::artist::ArtistDetail>,
    pub selected: usize,
}

pub struct AlbumDetailState {
    pub album_uri: String,
    pub detail: Fetch<crate::api::album::AlbumDetail>,
    pub selected: usize,
}

// Phase 5's transient overlays (name prompt, yes/no confirm, playlist
// picker) live as sibling `Option<_>` fields on `AppState` rather than
// new `Screen` stack variants -- `Screen::Help`'s own addition (the most
// recent precedent) touched 8+ separate call sites (the exhaustive
// `render` match, the exhaustive Main-focus `match key.code`, and a
// `nav.push` binding hand-added to every one of 8 screens individually,
// each re-implementing the global keys by hand since there's no shared
// fallthrough). None of that is right for something transient anyway --
// a yes/no confirm isn't a destination with its own `Esc`-back semantics,
// it's a gate on top of wherever the user already was. One interception
// point at the very top of the key loop (same place `Tab` is already
// intercepted) and one draw call at the end of `render` covers all
// three, and the screen underneath is untouched -- its list position,
// filter, nav depth all just resume once the overlay closes.

