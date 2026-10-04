use super::*;
use crate::api::search::TrackResult;

/// The Connect queue. Unlike the library lists it changes on its own (a track
/// ends, another device skips), so it is polled while visible rather than
/// fetched once.
pub struct QueueState {
    pub fetch: Fetch<crate::api::queue::QueueSummary>,
    pub selected: usize,
}

impl QueueState {
    pub fn new() -> Self {
        Self {
            fetch: Fetch::NotStarted,
            selected: 0,
        }
    }
}

impl Default for QueueState {
    fn default() -> Self {
        Self::new()
    }
}

/// Connect devices. Fetched on entry; devices come and go rarely, so
/// `Shift+R` refetches manually instead of polling.
pub struct DevicesState {
    pub fetch: Fetch<Vec<crate::api::devices::DeviceSummary>>,
    pub selected: usize,
}

impl DevicesState {
    pub fn new() -> Self {
        Self {
            fetch: Fetch::NotStarted,
            selected: 0,
        }
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

/// The playlist `Screen::PlaylistDetail` is showing, and its tracks. Fetch
/// results carry the playlist's URI so a late response for a playlist the
/// user has left cannot overwrite the one showing now.
pub struct PlaylistDetailState {
    pub playlist: crate::api::library::PlaylistSummary,
    pub tracks: Fetch<Vec<TrackResult>>,
    pub selected: usize,
    pub filter: ListFilter,
    /// `Some(start_index)` while move mode (`m`) is active: where the moving
    /// track started, so `Enter` can send the net displacement. While it is
    /// set, `selected` is a raw array index, not a display row.
    pub move_mode: Option<usize>,
}

/// One artist's albums. The URI guards against late responses, as in
/// `PlaylistDetailState`.
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

// The transient overlays (confirm, text prompt, playlist picker, quick jump)
// are `Option` fields on `AppState`, not `Screen` variants. A confirm is a
// gate on top of wherever the user already is, not a destination with its own
// Esc-back semantics; one interception at the top of key handling and one
// draw at the end of `render` cover them, and the screen underneath resumes
// untouched when the overlay closes.
