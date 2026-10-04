//! Fetching lyrics for a track from the sources in priority order.
//!
//! Each source answers "no usable synced result" with `None`, which falls
//! through to the next:
//!
//! 1. Spicy Lyrics' developer API (only with a configured key): word-level
//!    sync, credited under the lyrics.
//! 2. Spotify's own first-party lyrics, over the librespot session.
//! 3. YouTube Music, a different licensing catalogue.
//! 4. lrclib, the community database. Runs on a dedicated thread because its
//!    client is blocking, and is the only source that can answer plain,
//!    instrumental or not-found.
//!
//! Results come back on a channel tagged with the request's generation, so
//! the caller can drop an answer for a track that has since been skipped.

use super::spicy::{SpicyClient, SpicyLyrics};
use super::{cached_synced, spicy_cache_key, spotify_lyrics, store_synced, ytmusic, CachedLyrics, LyricsClient};
use crate::paths::cache_dir;
use librespot_core::session::Session;
use librespot_core::{SpotifyId, SpotifyUri};
use std::sync::mpsc::{self, Receiver, Sender};

/// What the sources need to identify a track.
#[derive(Clone)]
pub struct TrackMeta {
    pub track_id: String,
    pub artist: String,
    pub title: String,
    pub album: Option<String>,
    pub duration_ms: u32,
}

pub struct LyricsPipeline {
    lrclib_tx: Sender<(u64, TrackMeta)>,
    result_tx: Sender<(u64, CachedLyrics)>,
    spicy: Option<SpicyClient>,
}

impl LyricsPipeline {
    /// Starts the lrclib thread. Returns the receiver for every source's
    /// results.
    pub fn new(spicy: Option<SpicyClient>) -> (Self, Receiver<(u64, CachedLyrics)>) {
        let (lrclib_tx, lrclib_rx) = mpsc::channel::<(u64, TrackMeta)>();
        let (result_tx, result_rx) = mpsc::channel();

        let thread_tx = result_tx.clone();
        std::thread::spawn(move || {
            let client = LyricsClient::new(cache_dir());
            for (generation, meta) in lrclib_rx {
                let album = meta.album.as_deref();
                let result = client.fetch(&meta.track_id, &meta.artist, &meta.title, album, meta.duration_ms);
                if thread_tx.send((generation, result)).is_err() {
                    return;
                }
            }
        });

        (Self { lrclib_tx, result_tx, spicy }, result_rx)
    }

    /// Starts the source chain for `meta`. The answer arrives on the
    /// receiver returned by `new`, tagged `generation`.
    pub fn request(&self, session: &Session, generation: u64, meta: TrackMeta) {
        let Some(track_id) = parse_track_id(&meta.track_id) else {
            let _ = self.lrclib_tx.send((generation, meta));
            return;
        };
        let session = session.clone();
        let spicy = self.spicy.clone();
        let result_tx = self.result_tx.clone();
        let lrclib_tx = self.lrclib_tx.clone();
        tokio::spawn(async move {
            if let (Some(client), Ok(base62)) = (&spicy, track_id.to_base62())
                && let Some(found) = spicy_lookup(client, &meta.track_id, &base62).await
            {
                let _ = result_tx.send((generation, found));
                return;
            }
            if let Some(found) = spotify_lyrics(&session, track_id).await {
                let _ = result_tx.send((generation, found));
                return;
            }
            let duration_secs = meta.duration_ms as f64 / 1000.0;
            if let Some(found) = ytmusic::ytmusic_lyrics(&meta.artist, &meta.title, duration_secs).await {
                let _ = result_tx.send((generation, found));
                return;
            }
            let _ = lrclib_tx.send((generation, meta));
        });
    }
}

fn parse_track_id(uri: &str) -> Option<SpotifyId> {
    SpotifyUri::from_uri(uri).ok().and_then(|uri| SpotifyId::try_from(&uri).ok())
}

/// Spicy Lyrics for one track: a cached synced result first (so a replay
/// never spends rate-limit quota), then the API, storing whatever it returns.
/// A stale lrclib `Plain`/`NotFound` entry is deliberately not a hit (see
/// `cached_synced`), and a synced result overwrites it.
async fn spicy_lookup(client: &SpicyClient, track_uri: &str, base62_id: &str) -> Option<CachedLyrics> {
    let dir = cache_dir();
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    // Its own cache key: entries from before word timing existed sit under
    // the plain uri and are never read here, so they refresh on next play.
    let key = spicy_cache_key(track_uri);
    if let Some(hit) = cached_synced(&dir, &key, now_unix) {
        return Some(hit);
    }
    let SpicyLyrics { lines, words, credit } = client.lyrics(base62_id).await?;
    if let Err(e) = store_synced(&dir, &key, lines.clone(), words.clone(), Some(credit.clone()), now_unix) {
        log::warn!("spicy_lyrics[{base62_id}]: couldn't cache the result: {e}");
    }
    Some(CachedLyrics::Synced { lines, words, credit: Some(credit) })
}

/// Warms the on-disk lyrics caches for a track that is about to play, so a
/// skip that lands on it finds them ready. Only the two sources that cache
/// to disk (Spicy, lrclib) are worth prefetching; Spotify and YouTube Music
/// re-fetch on every play and would just waste a request.
pub async fn warm_cache(spicy: Option<SpicyClient>, meta: TrackMeta) {
    if let (Some(client), Some(id)) = (&spicy, parse_track_id(&meta.track_id))
        && let Ok(base62) = id.to_base62()
        && spicy_lookup(client, &meta.track_id, &base62).await.is_some()
    {
        return;
    }
    tokio::task::spawn_blocking(move || {
        let album = meta.album.as_deref().filter(|a| !a.is_empty());
        LyricsClient::new(cache_dir()).fetch(&meta.track_id, &meta.artist, &meta.title, album, meta.duration_ms);
    });
}
