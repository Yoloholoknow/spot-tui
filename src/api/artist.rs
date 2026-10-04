// Artist Detail: the artist's info and albums. No top-tracks section:
// `artist_top_tracks` is deprecated in rspotify 0.16 ("Spotify has removed this
// endpoint"). Following is `library::follow_artist`.

use rspotify::clients::BaseClient;
use rspotify::model::{AlbumType, ArtistId};
use rspotify::prelude::Id;
use rspotify::AuthCodeSpotify;

use super::ensure_fresh;
use super::library::SavedAlbumSummary;

// Dev Mode apps cap this endpoint below 50 ("Invalid limit"), as with search. 10
// is the value known to work for a Dev-Mode catalogue endpoint; not verified for
// this one specifically, but too low costs a round trip and too high is a 400.
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

    // Explicit `include_groups`: rspotify documents an empty list as "all types", but
    // that predates Spotify's Feb 2026 API consolidation. This alone did not cure the
    // 400s; `describe_client_error` surfaces the real reason.
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

    // `genres` is deprecated upstream and may be absent. `#[serde(default)]` makes it
    // deserialize as empty instead of failing.
    #[allow(deprecated)]
    let genres = artist.genres;
    Ok(ArtistDetail { uri: artist.id.uri(), name: artist.name, genres, albums })
}
