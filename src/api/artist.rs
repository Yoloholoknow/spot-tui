//! Artist Detail (Phase 9): the artist's own info plus a list of their
//! albums, read-only, play-from-here (opening an album). No top-tracks
//! section -- `artist_top_tracks` is marked deprecated in rspotify 0.16.1
//! ("Spotify has removed this endpoint"), so it's not something this
//! phase is choosing to skip, it's not available to build against at
//! all. No follow/unfollow toggle either -- that's a real, still-viable
//! mutation (`library_add`/`library_remove` with `LibraryId::Artist`,
//! same pattern Phase 5 already uses for playlists), just explicitly out
//! of scope for this phase's own stated "read-only" line in the plan.

use rspotify::clients::BaseClient;
use rspotify::model::{AlbumType, ArtistId};
use rspotify::prelude::Id;
use rspotify::AuthCodeSpotify;

use super::ensure_fresh;
use super::library::SavedAlbumSummary;

// Confirmed live via the real Spotify error body ("Invalid limit"): Dev
// Mode apps cap this endpoint below 50, same restriction (and same error
// message) `api::search` already hit and worked around. 10 is the one
// value already confirmed safe for *a* Dev-Mode-capped catalog endpoint
// in this app (search) -- not verified specifically for this endpoint,
// but a conservative starting point: too low costs an extra round trip
// per page, too high is a guaranteed 400.
const PAGE_LIMIT: u32 = 10;
const MAX_ITEMS: u32 = 2000;

#[derive(Debug, Clone, PartialEq)]
pub struct ArtistDetail {
    pub uri: String,
    pub name: String,
    pub genres: Vec<String>,
    pub albums: Vec<SavedAlbumSummary>,
}

pub async fn get_artist_detail(client: &AuthCodeSpotify, artist_uri: &str) -> Result<ArtistDetail, String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before get_artist_detail failed, trying with existing token anyway: {e}");
    }
    let id = ArtistId::from_id_or_uri(artist_uri).map_err(|e| e.to_string())?;
    let artist = match client.artist(id.as_ref()).await {
        Ok(a) => a,
        Err(e) => {
            let detail = super::describe_client_error(e).await;
            log::warn!("get_artist_detail: artist() failed for {artist_uri}: {detail}");
            return Err(detail);
        }
    };

    // Explicit include_groups, not [] -- rspotify's own doc comment claims
    // an empty list means "all types," but that assumption predates
    // Spotify's Feb-2026 API consolidation. Reported live as still 400ing
    // even with this change, so it wasn't the (or the whole) root cause --
    // kept anyway since it's not wrong, just insufficient on its own;
    // describe_client_error below is what actually surfaces Spotify's real
    // reason once reproduced again.
    let include_groups = [AlbumType::Album, AlbumType::Single, AlbumType::Compilation, AlbumType::AppearsOn];
    let mut albums = Vec::new();
    let mut offset: u32 = 0;
    loop {
        let page = match client.artist_albums_manual(id.as_ref(), include_groups, None, Some(PAGE_LIMIT), Some(offset)).await {
            Ok(p) => p,
            Err(e) => {
                let detail = super::describe_client_error(e).await;
                log::warn!("get_artist_detail: artist_albums_manual() failed for {artist_uri} at offset {offset}: {detail}");
                return Err(detail);
            }
        };
        let got = page.items.len() as u32;
        albums.extend(page.items.into_iter().filter_map(|a| {
            Some(SavedAlbumSummary {
                uri: a.id?.uri(),
                name: a.name,
                artist: a.artists.first().map(|ar| ar.name.clone()).unwrap_or_default(),
            })
        }));
        if got < PAGE_LIMIT || albums.len() as u32 >= MAX_ITEMS {
            break;
        }
        offset += PAGE_LIMIT;
    }

    // genres is deprecated upstream ("may not exist in the response") but
    // still has #[serde(default)], so it deserializes safely (as empty)
    // rather than failing the way the queue endpoint's missing
    // external_ids did -- shown when present, harmlessly absent otherwise.
    #[allow(deprecated)]
    let genres = artist.genres;
    Ok(ArtistDetail { uri: artist.id.uri(), name: artist.name, genres, albums })
}
