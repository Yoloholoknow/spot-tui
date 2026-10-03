//! A single track's own artist/album ids (Phase 28: `v`/`Shift+V` on Now
//! Playing). Nothing else in this app needed a bare track lookup by uri --
//! every other screen that opens an artist/album from a track already has
//! the ids in hand from a list (`TrackResult::artist_uri`/`album_uri`,
//! Phase 9). The currently-playing track has no such list entry: librespot's
//! own `AudioItem` carries artist/album *names* only (confirmed by reading
//! `librespot-metadata`'s `UniqueFields::Track`), so this is a real, if
//! small, network call rather than a free lookup.

use rspotify::clients::BaseClient;
use rspotify::model::TrackId;
use rspotify::prelude::Id;
use rspotify::AuthCodeSpotify;

use super::ensure_fresh;

#[derive(Debug, Clone, PartialEq)]
pub struct TrackIds {
    pub artist_uri: String,
    pub album_uri: String,
}

/// Like `describe_client_error` results elsewhere in this app, verified
/// live, not by unit test -- no pure logic here to isolate.
pub async fn get_track_ids(client: &AuthCodeSpotify, track_uri: &str) -> Result<TrackIds, String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before get_track_ids failed, trying with existing token anyway: {e}");
    }
    let id = TrackId::from_id_or_uri(track_uri).map_err(|e| e.to_string())?;
    let track = match client.track(id, None).await {
        Ok(t) => t,
        Err(e) => {
            let detail = super::describe_client_error(e).await;
            log::warn!("get_track_ids: track() failed for {track_uri}: {detail}");
            return Err(detail);
        }
    };
    let artist_uri = track.artists.first().and_then(|a| a.id.clone()).map(|id| id.uri()).unwrap_or_default();
    let album_uri = track.album.id.map(|id| id.uri()).unwrap_or_default();
    Ok(TrackIds { artist_uri, album_uri })
}
