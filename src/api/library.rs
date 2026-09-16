//! Library reads (Phase 2/3 of the design-scope plan): Liked Songs, Saved
//! Albums, Followed Artists, Your Playlists, Playlist Detail tracks.
//! Read-only for now -- CRUD on playlists comes in Phase 5. Client/token
//! bootstrap lives in the parent `api` module.
//!
//! Fully paginates each endpoint (up to `MAX_ITEMS`, a generous safety
//! cap against a pathological runaway -- not a realistic ceiling for
//! actual use). Reported live that the original single-50-item-page
//! version cut off real playlists mid-list; the in-list filter (`/`)
//! shipped alongside this makes browsing a large fetched list workable,
//! which is what makes a few-thousand-item cap a reasonable tradeoff
//! rather than a real limitation.

use rspotify::clients::{BaseClient, OAuthClient};
use rspotify::model::{AlbumId, ArtistId, LibraryId, Market, PlayableItem, PlaylistId, TrackId};
use rspotify::prelude::Id;
use rspotify::{AuthCodeSpotify, ClientResult};

use super::ensure_fresh;
use super::search::TrackResult;

const PAGE_LIMIT: u32 = 50;
const MAX_ITEMS: u32 = 2000;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SavedAlbumSummary {
    pub uri: String,
    pub name: String,
    pub artist: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FollowedArtist {
    pub uri: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlaylistSummary {
    pub uri: String,
    pub name: String,
    pub track_count: u32,
}

pub async fn liked_songs(client: &AuthCodeSpotify) -> ClientResult<Vec<TrackResult>> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before liked_songs failed, trying with existing token anyway: {e}");
    }
    let mut out = Vec::new();
    let mut offset: u32 = 0;
    loop {
        let page = client
            .current_user_saved_tracks_manual(None, Some(PAGE_LIMIT), Some(offset))
            .await?;
        let got = page.items.len() as u32;
        out.extend(page.items.into_iter().map(|saved| {
            let t = saved.track;
            let first_artist = t.artists.first();
            TrackResult {
                uri: t.id.map(|id| id.uri()).unwrap_or_default(),
                title: t.name,
                artist: first_artist.map(|a| a.name.clone()).unwrap_or_default(),
                artist_uri: first_artist.and_then(|a| a.id.clone()).map(|id| id.uri()).unwrap_or_default(),
                album_uri: t.album.id.clone().map(|id| id.uri()).unwrap_or_default(),
                album: t.album.name,
            }
        }));
        if got < PAGE_LIMIT || out.len() as u32 >= MAX_ITEMS {
            break;
        }
        offset += PAGE_LIMIT;
    }
    Ok(out)
}

pub async fn saved_albums(client: &AuthCodeSpotify) -> ClientResult<Vec<SavedAlbumSummary>> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before saved_albums failed, trying with existing token anyway: {e}");
    }
    let mut out = Vec::new();
    let mut offset: u32 = 0;
    loop {
        let page = client
            .current_user_saved_albums_manual(None, Some(PAGE_LIMIT), Some(offset))
            .await?;
        let got = page.items.len() as u32;
        out.extend(page.items.into_iter().map(|saved| SavedAlbumSummary {
            uri: saved.album.id.uri(),
            name: saved.album.name,
            artist: saved.album.artists.first().map(|a| a.name.clone()).unwrap_or_default(),
        }));
        if got < PAGE_LIMIT || out.len() as u32 >= MAX_ITEMS {
            break;
        }
        offset += PAGE_LIMIT;
    }
    Ok(out)
}

pub async fn followed_artists(client: &AuthCodeSpotify) -> ClientResult<Vec<FollowedArtist>> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before followed_artists failed, trying with existing token anyway: {e}");
    }
    let mut out = Vec::new();
    // Cursor-based, not offset-based: the next page's cursor is the last
    // artist's own ID from the current page (per Spotify's documented
    // contract for this endpoint), not a numeric offset.
    let mut after: Option<String> = None;
    loop {
        let page = client.current_user_followed_artists(after.as_deref(), Some(PAGE_LIMIT)).await?;
        let got = page.items.len() as u32;
        let next_after = page.items.last().map(|a| a.id.id().to_string());
        out.extend(page.items.into_iter().map(|artist| FollowedArtist {
            uri: artist.id.uri(),
            name: artist.name,
        }));
        if got < PAGE_LIMIT || out.len() as u32 >= MAX_ITEMS || next_after.is_none() {
            break;
        }
        after = next_after;
    }
    Ok(out)
}

pub async fn your_playlists(client: &AuthCodeSpotify) -> ClientResult<Vec<PlaylistSummary>> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before your_playlists failed, trying with existing token anyway: {e}");
    }
    let mut out = Vec::new();
    let mut offset: u32 = 0;
    loop {
        let page = client.current_user_playlists_manual(Some(PAGE_LIMIT), Some(offset)).await?;
        let got = page.items.len() as u32;
        out.extend(page.items.into_iter().map(|p| PlaylistSummary {
            uri: p.id.uri(),
            name: p.name,
            track_count: p.items.total,
        }));
        if got < PAGE_LIMIT || out.len() as u32 >= MAX_ITEMS {
            break;
        }
        offset += PAGE_LIMIT;
    }
    Ok(out)
}

/// Track listing for one playlist (Phase 3). `playlist_uri` is a full
/// `spotify:playlist:...` URI, matching what `PlaylistSummary::uri`
/// already stores. Local tracks and podcast episodes are skipped --
/// neither has a playable track URI this app's `LoadRequest` can use,
/// and podcasts are an explicit non-goal.
///
/// Returns `String` rather than `ClientResult` like the other functions
/// here: this one can also fail on bad ID input, a distinct failure mode
/// rspotify's `ClientError` has no variant for -- every caller already
/// stringifies the other functions' errors immediately anyway.
pub async fn playlist_tracks(client: &AuthCodeSpotify, playlist_uri: &str) -> Result<Vec<TrackResult>, String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before playlist_tracks failed, trying with existing token anyway: {e}");
    }
    let playlist_id = PlaylistId::from_id_or_uri(playlist_uri).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut offset: u32 = 0;
    loop {
        let page = client
            .playlist_items_manual(playlist_id.as_ref(), None, None::<Market>, Some(PAGE_LIMIT), Some(offset))
            .await
            .map_err(|e| e.to_string())?;
        // Bounded on raw items fetched, not the filtered output below --
        // a playlist heavy on local files/podcast episodes (filtered out
        // entirely) would otherwise keep paging well past MAX_ITEMS'
        // intended cap on API calls, since `out` grows slower than what
        // was actually fetched.
        let got = page.items.len() as u32;
        out.extend(page.items.into_iter().filter_map(|item| match item.item {
            Some(PlayableItem::Track(t)) => {
                let first_artist = t.artists.first();
                let artist = first_artist.map(|a| a.name.clone()).unwrap_or_default();
                let artist_uri = first_artist.and_then(|a| a.id.clone()).map(|id| id.uri()).unwrap_or_default();
                let album_uri = t.album.id.clone().map(|id| id.uri()).unwrap_or_default();
                Some(TrackResult { uri: t.id?.uri(), title: t.name, artist, artist_uri, album: t.album.name, album_uri })
            }
            _ => None,
        }));
        offset += got;
        if got < PAGE_LIMIT || offset >= MAX_ITEMS {
            break;
        }
    }
    Ok(out)
}

// Phase 13: like/follow/save -- confirmed against rspotify 0.16.1's real
// `LibraryId` enum (`rspotify-model/src/idtypes.rs`) before writing any of
// these, per this project's own standing "check the real API surface
// first" discipline: `Track`/`Artist`/`Album` variants exist exactly as
// assumed, each wrapping that type's own `Id`. `library_add`/
// `library_remove` are the same two calls `api::playlists::delete_playlist`
// already uses for `LibraryId::Playlist` -- Spotify's Feb-2026 library
// consolidation covers all of these through one endpoint family, not a
// separate one per item kind.

fn track_id_for_library(track_uri: &str) -> Result<TrackId<'_>, String> {
    TrackId::from_id_or_uri(track_uri).map_err(|e| e.to_string())
}

fn artist_id(artist_uri: &str) -> Result<ArtistId<'_>, String> {
    ArtistId::from_id_or_uri(artist_uri).map_err(|e| e.to_string())
}

fn album_id(album_uri: &str) -> Result<AlbumId<'_>, String> {
    AlbumId::from_id_or_uri(album_uri).map_err(|e| e.to_string())
}

pub async fn like_track(client: &AuthCodeSpotify, track_uri: &str) -> Result<(), String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before like_track failed, trying with existing token anyway: {e}");
    }
    let id = track_id_for_library(track_uri)?;
    client.library_add([LibraryId::Track(id)]).await.map_err(|e| e.to_string())
}

pub async fn unlike_track(client: &AuthCodeSpotify, track_uri: &str) -> Result<(), String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before unlike_track failed, trying with existing token anyway: {e}");
    }
    let id = track_id_for_library(track_uri)?;
    client.library_remove([LibraryId::Track(id)]).await.map_err(|e| e.to_string())
}

pub async fn follow_artist(client: &AuthCodeSpotify, artist_uri: &str) -> Result<(), String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before follow_artist failed, trying with existing token anyway: {e}");
    }
    let id = artist_id(artist_uri)?;
    client.library_add([LibraryId::Artist(id)]).await.map_err(|e| e.to_string())
}

pub async fn unfollow_artist(client: &AuthCodeSpotify, artist_uri: &str) -> Result<(), String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before unfollow_artist failed, trying with existing token anyway: {e}");
    }
    let id = artist_id(artist_uri)?;
    client.library_remove([LibraryId::Artist(id)]).await.map_err(|e| e.to_string())
}

pub async fn save_album(client: &AuthCodeSpotify, album_uri: &str) -> Result<(), String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before save_album failed, trying with existing token anyway: {e}");
    }
    let id = album_id(album_uri)?;
    client.library_add([LibraryId::Album(id)]).await.map_err(|e| e.to_string())
}

pub async fn unsave_album(client: &AuthCodeSpotify, album_uri: &str) -> Result<(), String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before unsave_album failed, trying with existing token anyway: {e}");
    }
    let id = album_id(album_uri)?;
    client.library_remove([LibraryId::Album(id)]).await.map_err(|e| e.to_string())
}
