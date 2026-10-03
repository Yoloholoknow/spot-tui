//! Next-track prefetch (soft-load): fetching enough metadata for a track
//! that hasn't started playing yet -- `librespot`'s own `PlayerEvent`
//! only carries this for the track that's *actually* loaded, so a track
//! merely sitting next in the queue needs one Web API call to learn its
//! artist/title/album/cover ahead of time.

use rspotify::clients::BaseClient;
use rspotify::model::TrackId;
use rspotify::AuthCodeSpotify;

use super::ensure_fresh;

#[derive(Debug, Clone, PartialEq)]
pub struct NextTrackMeta {
    pub artist: String,
    pub title: String,
    pub album: String,
    pub duration_ms: u32,
    /// Largest available cover image, if any -- same "largest-first"
    /// convention `librespot`'s own `covers` field already follows, so
    /// callers don't need two different sorting rules.
    pub cover_url: Option<String>,
}

pub async fn get_next_track_meta(client: &AuthCodeSpotify, track_uri: &str) -> Result<NextTrackMeta, String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before get_next_track_meta failed, trying with existing token anyway: {e}");
    }
    let id = TrackId::from_id_or_uri(track_uri).map_err(|e| e.to_string())?;
    let track = match client.track(id.as_ref(), None).await {
        Ok(t) => t,
        Err(e) => {
            let detail = super::describe_client_error(e).await;
            log::warn!("get_next_track_meta: track() failed for {track_uri}: {detail}");
            return Err(detail);
        }
    };
    let artist = track.artists.first().map(|a| a.name.clone()).unwrap_or_default();
    let cover_url = track
        .album
        .images
        .iter()
        .max_by_key(|img| img.width.unwrap_or(0) * img.height.unwrap_or(0))
        .map(|img| img.url.clone());
    Ok(NextTrackMeta {
        artist,
        title: track.name,
        album: track.album.name,
        duration_ms: track.duration.num_milliseconds().max(0) as u32,
        cover_url,
    })
}
