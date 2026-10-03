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
    /// Added for Phase 9 (Artist/Album Detail) -- `#[serde(default)]` so
    /// a search-cache entry written before this field existed still
    /// deserializes (as empty strings) instead of being treated as
    /// corrupt and discarded outright; the 1-hour cache TTL means a
    /// stale-shaped entry ages out on its own regardless.
    #[serde(default)]
    pub artist_uri: String,
    #[serde(default)]
    pub album_uri: String,
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
    crate::paths::cache_dir().join("search")
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
            artist_uri: "spotify:artist:a".to_string(),
            album_uri: "spotify:album:b".to_string(),
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

/// Spotify's own field-scope operators. A query already using one of
/// these is a power-user query with explicit intent -- auto-adding a
/// second `track:`-scoped variant on top would either double-wrap it
/// into nonsense (`track:"track:x"`) or override intent the user already
/// stated explicitly, so those queries are sent exactly as typed, single
/// call, same as before this tier.
const FIELD_SCOPE_PREFIXES: &[&str] = &["track:", "artist:", "album:", "year:"];

fn is_field_scoped(query: &str) -> bool {
    let lower = query.to_lowercase();
    FIELD_SCOPE_PREFIXES.iter().any(|p| lower.contains(p))
}

/// Field-scoped hits first (deduped by URI), then plain-phrase hits not
/// already present, capped at `limit`. Spotify's plain free-text search
/// (even restricted to `SearchType::Track`) matches the phrase against
/// track/artist/album text broadly, which is real recall but can rank a
/// track whose *album* happens to share the phrase above the track whose
/// *name* actually matches it -- reported as the original motivation for
/// this tier. `track:"<query>"` narrows to the track-name field
/// specifically, giving precision; merging keeps that precision up front
/// while still backfilling with the broader (higher-recall) plain
/// results so a query with no exact track-name hit still returns
/// something.
fn merge_results(field_scoped: Vec<TrackResult>, plain: Vec<TrackResult>, limit: usize) -> Vec<TrackResult> {
    let mut seen = std::collections::HashSet::new();
    let mut merged = Vec::with_capacity(limit);
    for t in field_scoped.into_iter().chain(plain) {
        if merged.len() >= limit {
            break;
        }
        if seen.insert(t.uri.clone()) {
            merged.push(t);
        }
    }
    merged
}

#[cfg(test)]
mod relevance_tests {
    use super::*;

    fn track(uri: &str) -> TrackResult {
        TrackResult {
            uri: uri.to_string(),
            title: uri.to_string(),
            artist: "A".to_string(),
            album: "B".to_string(),
            artist_uri: "spotify:artist:a".to_string(),
            album_uri: "spotify:album:b".to_string(),
        }
    }

    #[test]
    fn plain_query_is_not_field_scoped() {
        assert!(!is_field_scoped("bohemian rhapsody"));
    }

    #[test]
    fn a_query_already_using_a_field_operator_is_left_alone() {
        assert!(is_field_scoped("artist:Queen"));
        assert!(is_field_scoped("track:\"Bohemian Rhapsody\""));
        // Case-insensitive -- Spotify's own operators aren't case-sensitive either.
        assert!(is_field_scoped("ARTIST:Queen"));
    }

    #[test]
    fn merge_puts_field_scoped_hits_before_plain_hits() {
        let scoped = vec![track("a"), track("b")];
        let plain = vec![track("c"), track("d")];
        let merged = merge_results(scoped, plain, 10);
        assert_eq!(merged.iter().map(|t| t.uri.as_str()).collect::<Vec<_>>(), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn merge_dedupes_by_uri_keeping_the_field_scoped_copy() {
        let scoped = vec![track("a")];
        let plain = vec![track("a"), track("b")];
        let merged = merge_results(scoped, plain, 10);
        assert_eq!(merged.iter().map(|t| t.uri.as_str()).collect::<Vec<_>>(), vec!["a", "b"]);
    }

    #[test]
    fn merge_respects_the_limit() {
        let scoped = vec![track("a"), track("b")];
        let plain = vec![track("c"), track("d")];
        let merged = merge_results(scoped, plain, 3);
        assert_eq!(merged.iter().map(|t| t.uri.as_str()).collect::<Vec<_>>(), vec!["a", "b", "c"]);
    }
}

async fn run_query(client: &AuthCodeSpotify, query: &str, limit: u32) -> ClientResult<Vec<TrackResult>> {
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

    Ok(page
        .items
        .into_iter()
        .map(|t| {
            let first_artist = t.artists.first();
            TrackResult {
                uri: t.id.map(|id| id.uri()).unwrap_or_default(),
                title: t.name,
                artist: first_artist.map(|a| a.name.clone()).unwrap_or_default(),
                artist_uri: first_artist.and_then(|a| a.id.clone()).map(|id| id.uri()).unwrap_or_default(),
                album_uri: t.album.id.clone().map(|id| id.uri()).unwrap_or_default(),
                album: t.album.name,
            }
        })
        .collect())
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

    let results = if is_field_scoped(query) {
        run_query(client, query, limit).await?
    } else {
        let scoped_query = format!("track:\"{}\"", query.trim());
        let (scoped, plain) = tokio::join!(run_query(client, &scoped_query, limit), run_query(client, query, limit));
        // The plain query is the pre-existing, always-worked baseline --
        // its failure still propagates. The scoped query is this tier's
        // speculative addition on top; if it errors, degrade to
        // plain-only rather than breaking a search that would otherwise
        // have succeeded on its own.
        let scoped = scoped.unwrap_or_else(|e| {
            log::warn!("field-scoped search query failed, falling back to plain results only: {e}");
            Vec::new()
        });
        merge_results(scoped, plain?, limit.min(10) as usize)
    };

    write_search_cache(&cache_dir, query, &results, now);
    Ok(results)
}
