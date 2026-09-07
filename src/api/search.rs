//! Standalone search+play (Tier 1 of the roadmap): no other Spotify
//! client should be required to start something. Client/token bootstrap
//! lives in the parent `api` module; this is the search-specific surface.

use rspotify::model::SearchType;
use rspotify::prelude::*;
use rspotify::{AuthCodeSpotify, ClientResult};

use super::ensure_fresh;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrackResult {
    pub uri: String,
    pub title: String,
    pub artist: String,
    pub album: String,
}

/// Tier 4: search/metadata response caching. Directly reduces Web API
/// call volume, which is the axis Spotify actually rate-limits on
/// (confirmed this session -- the 429s and the 400 were both on
/// api.spotify.com, never on audio streaming). Same TTL'd-JSON-on-disk
/// pattern `lyrics.rs` already uses for lrclib.
const SEARCH_CACHE_TTL_SECS: u64 = 60 * 60;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SearchCacheEntry {
    fetched_at_unix: u64,
    results: Vec<TrackResult>,
}

fn search_cache_dir() -> std::path::PathBuf {
    directories::ProjectDirs::from("", "", "spot-tui")
        .map(|d| d.cache_dir().join("search"))
        .unwrap_or_else(|| std::env::temp_dir().join("spot-tui-search-cache"))
}

/// Normalizes case/whitespace so trivially-different typings of the same
/// query ("Test", " test ") hit the same cache entry.
fn search_cache_path(cache_dir: &std::path::Path, query: &str) -> std::path::PathBuf {
    use sha1::{Digest, Sha1};
    let normalized = query.trim().to_lowercase();
    let hash = format!("{:x}", Sha1::digest(normalized.as_bytes()));
    cache_dir.join(format!("{hash}.json"))
}

fn read_search_cache(cache_dir: &std::path::Path, query: &str, now_unix: u64) -> Option<Vec<TrackResult>> {
    let path = search_cache_path(cache_dir, query);
    let data = std::fs::read_to_string(path).ok()?;
    let entry: SearchCacheEntry = serde_json::from_str(&data).ok()?;
    if now_unix.saturating_sub(entry.fetched_at_unix) > SEARCH_CACHE_TTL_SECS {
        return None; // stale, allow refetch
    }
    Some(entry.results)
}

fn write_search_cache(cache_dir: &std::path::Path, query: &str, results: &[TrackResult], now_unix: u64) {
    let _ = std::fs::create_dir_all(cache_dir);
    let entry = SearchCacheEntry {
        fetched_at_unix: now_unix,
        results: results.to_vec(),
    };
    if let Ok(json) = serde_json::to_string(&entry) {
        let _ = std::fs::write(search_cache_path(cache_dir, query), json);
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("spot-tui-search-cache-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn track(title: &str) -> TrackResult {
        TrackResult {
            uri: format!("spotify:track:{title}"),
            title: title.to_string(),
            artist: "A".to_string(),
            album: "B".to_string(),
        }
    }

    #[test]
    fn returns_none_when_nothing_cached() {
        let dir = scratch_dir("empty");
        assert_eq!(read_search_cache(&dir, "anything", 1_000_000), None);
    }

    #[test]
    fn round_trips_write_then_read() {
        let dir = scratch_dir("roundtrip");
        let results = vec![track("a"), track("b")];
        write_search_cache(&dir, "my query", &results, 1_000_000);
        assert_eq!(read_search_cache(&dir, "my query", 1_000_010), Some(results));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn expires_after_ttl() {
        let dir = scratch_dir("ttl");
        let results = vec![track("a")];
        write_search_cache(&dir, "q", &results, 1_000_000);
        assert_eq!(
            read_search_cache(&dir, "q", 1_000_000 + SEARCH_CACHE_TTL_SECS - 1),
            Some(results)
        );
        assert_eq!(
            read_search_cache(&dir, "q", 1_000_000 + SEARCH_CACHE_TTL_SECS + 1),
            None
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cache_key_normalizes_case_and_whitespace() {
        let dir = scratch_dir("normalize");
        assert_eq!(
            search_cache_path(&dir, "Test Query"),
            search_cache_path(&dir, "  test query  ")
        );
    }
}

pub async fn search_tracks(
    client: &AuthCodeSpotify,
    query: &str,
    limit: u32,
) -> ClientResult<Vec<TrackResult>> {
    let cache_dir = search_cache_dir();
    let now = now_unix();
    if let Some(cached) = read_search_cache(&cache_dir, query, now) {
        return Ok(cached);
    }

    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before search failed, trying with existing token anyway: {e}");
    }

    // Dev Mode apps cap this at 10 (down from 50 as of Spotify's Feb 2026
    // migration); confirmed live -- anything higher is a 400 "Invalid
    // limit". Clamped here too, not just at the call site, so this can't
    // silently regress if another caller passes a bigger number later.
    let result = client
        .search(query, SearchType::Track, None, None, Some(limit.min(10)), None)
        .await?;

    let rspotify::model::SearchResult::Tracks(page) = result else {
        return Ok(vec![]);
    };

    let results: Vec<TrackResult> = page
        .items
        .into_iter()
        .map(|t| TrackResult {
            uri: t.id.map(|id| id.uri()).unwrap_or_default(),
            title: t.name,
            artist: t.artists.first().map(|a| a.name.clone()).unwrap_or_default(),
            album: t.album.name,
        })
        .collect();

    write_search_cache(&cache_dir, query, &results, now);
    Ok(results)
}
