//! Album Detail (Phase 9): the album's own tracks, read-only,
//! play-from-here. No save/unsave-album toggle -- a real, viable
//! mutation (`library_add`/`library_remove` with `LibraryId::Album`,
//! same pattern Phase 5 already uses for playlists), just out of scope
//! for this phase's own stated "read-only" line in the plan.

use rspotify::clients::BaseClient;
use rspotify::model::AlbumId;
use rspotify::prelude::Id;
use rspotify::AuthCodeSpotify;

use super::ensure_fresh;
use super::search::TrackResult;

// Reduced preemptively, not from a live report on this specific endpoint
// -- `album_track_manual` is the same category of catalog-browse
// endpoint as `api::artist`'s `artist_albums_manual`, which was
// confirmed live to 400 with "Invalid limit" at 50 under Dev Mode. A
// full album almost always has more than 10 tracks, so shipping this at
// 50 would have failed on the very next real test. See `api::artist`'s
// own comment on why 10 specifically.
const PAGE_LIMIT: u32 = 10;
const MAX_ITEMS: u32 = 2000;

#[derive(Debug, Clone, PartialEq)]
pub struct AlbumDetail {
    pub uri: String,
    pub name: String,
    pub artist: String,
    pub artist_uri: String,
    pub tracks: Vec<TrackResult>,
}

pub async fn get_album_detail(client: &AuthCodeSpotify, album_uri: &str) -> Result<AlbumDetail, String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before get_album_detail failed, trying with existing token anyway: {e}");
    }
    let id = AlbumId::from_id_or_uri(album_uri).map_err(|e| e.to_string())?;
    let album = match client.album(id.as_ref(), None).await {
        Ok(a) => a,
        Err(e) => {
            let detail = super::describe_client_error(e).await;
            log::warn!("get_album_detail: album() failed for {album_uri}: {detail}");
            return Err(detail);
        }
    };
    let first_artist = album.artists.first();
    let artist = first_artist.map(|a| a.name.clone()).unwrap_or_default();
    let artist_uri = first_artist.and_then(|a| a.id.clone()).map(|aid| aid.uri()).unwrap_or_default();
    let album_uri_resolved = album.id.uri();
    let album_name = album.name;

    // Deliberately re-paginates from scratch via album_track_manual rather
    // than also using the first page FullAlbum already embeds -- simpler
    // than stitching two pagination sources together. At PAGE_LIMIT=10
    // (see its own comment) most real albums need 2+ pages, unlike when
    // this was written against a since-disproven limit of 50.
    let mut tracks = Vec::new();
    let mut offset: u32 = 0;
    loop {
        let page = match client.album_track_manual(id.as_ref(), None, Some(PAGE_LIMIT), Some(offset)).await {
            Ok(p) => p,
            Err(e) => {
                let detail = super::describe_client_error(e).await;
                log::warn!("get_album_detail: album_track_manual() failed for {album_uri} at offset {offset}: {detail}");
                return Err(detail);
            }
        };
        let got = page.items.len() as u32;
        tracks.extend(page.items.into_iter().filter_map(|t| {
            let track_first_artist = t.artists.first();
            Some(TrackResult {
                uri: t.id?.uri(),
                title: t.name,
                artist: track_first_artist.map(|a| a.name.clone()).unwrap_or_default(),
                artist_uri: track_first_artist
                    .and_then(|a| a.id.clone())
                    .map(|aid| aid.uri())
                    .unwrap_or_default(),
                album: album_name.clone(),
                album_uri: album_uri_resolved.clone(),
            })
        }));
        if got < PAGE_LIMIT || tracks.len() as u32 >= MAX_ITEMS {
            break;
        }
        offset += PAGE_LIMIT;
    }

    Ok(AlbumDetail { uri: album_uri_resolved, name: album_name, artist, artist_uri, tracks })
}
