//! Album art fetching: for the track that just started, and a prefetch of
//! the one queued after it.

use crate::lyrics::pipeline::{self, TrackMeta};
use crate::lyrics::spicy::SpicyClient;
use crate::{api, http};
use image::DynamicImage;
use librespot_connect::Spirc;
use rspotify::AuthCodeSpotify;
use std::sync::mpsc::Sender;

/// Downloads and decodes a cover. Blocking: call from `spawn_blocking`.
pub fn fetch_cover(url: &str) -> Option<DynamicImage> {
    let response = http::agent().get(url).call().ok()?;
    image::load_from_memory(&http::read_limited(response, http::MAX_IMAGE_BYTES).ok()?).ok()
}

/// Warms the art and lyrics caches for whatever librespot already has queued
/// next (`peek_next_track` is free: it reads state librespot maintains
/// anyway), so a skip that lands there finds both ready instead of starting
/// cold. Best-effort throughout: no client, no cover or no lyrics just means
/// less gets warmed, never an error surfaced to the user.
///
/// The cover goes to `cover_tx` keyed by URI, not by generation, because the
/// track is not playing yet. The main loop parks it until that track starts.
pub async fn prefetch_next_track(
    spirc: Spirc,
    client: Option<AuthCodeSpotify>,
    spicy: Option<SpicyClient>,
    cover_tx: Sender<(String, DynamicImage)>,
) {
    let Ok(Some(next_uri)) = spirc.peek_next_track().await else {
        return;
    };
    let Some(client) = client else { return };
    let meta = match api::track::get_next_track_meta(&client, &next_uri).await {
        Ok(meta) => meta,
        Err(e) => {
            log::info!("prefetch[{next_uri}]: get_next_track_meta failed: {e}");
            return;
        }
    };

    if let Some(cover_url) = meta.cover_url {
        let uri = next_uri.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(image) = fetch_cover(&cover_url) {
                let _ = cover_tx.send((uri, image));
            }
        });
    }

    pipeline::warm_cache(
        spicy,
        TrackMeta {
            track_id: next_uri,
            artist: meta.artist,
            title: meta.title,
            album: Some(meta.album),
            duration_ms: meta.duration_ms,
        },
    )
    .await;
}
