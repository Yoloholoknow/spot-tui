// Album Detail: the album's own tracks. Read-only; saving the album is
// `library::save_album`.

use rspotify::clients::BaseClient;
use rspotify::model::AlbumId;
use rspotify::prelude::Id;
use rspotify::AuthCodeSpotify;

use super::ensure_fresh;
use super::search::TrackResult;

// Dev Mode apps cap catalogue endpoints below 50 ("Invalid limit" 400). 10 is the
// value known to work for search and is used here too; too low costs a round trip
// per page, too high is a guaranteed 400.
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

    // Paginates from scratch via `album_track_manual` instead of stitching onto the
    // first page `FullAlbum` embeds. At 10 per page most albums need 2+ pages.
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
