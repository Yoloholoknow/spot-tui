//! Spotify Web API search, for standalone play (Tier 1 of the roadmap):
//! no other Spotify client should be required to start something.
//!
//! Uses spot-tui's own registered Spotify app (Development Mode), not
//! ncspot's shared client_id -- ncspot's id turned out to be caught in a
//! broad, ongoing Spotify-side lockdown on third-party Web API access
//! (confirmed live: ncspot's own search is equally broken, not just
//! ours). A personal Dev Mode app's traffic doesn't resemble the
//! aggregate-abuse pattern that triggered that lockdown, so it plausibly
//! sidesteps it entirely -- though this can't be verified until it's
//! actually exercised for real.
//!
//! PKCE flow (via `librespot_oauth`, same crate/pattern ncspot itself
//! uses for its own login): only a client_id is needed, never a secret --
//! that's the whole point of PKCE for a native app that can't keep a
//! secret safe. The Spotify dashboard now requires an exact port in the
//! registered redirect URI (confirmed live -- the historic "register
//! without a port" exception no longer works), so unlike ncspot's
//! find-a-free-port-each-run approach, this uses one fixed port that must
//! match `REDIRECT_URI` below exactly, including in the dashboard.
//!
//! Because this is a different client_id than ncspot's, none of ncspot's
//! cached token is reusable -- refresh tokens are locked to the client_id
//! that issued them (confirmed earlier). First run needs a real one-time
//! browser login; the resulting token is cached separately from ncspot's.

use chrono::Utc;
use librespot_oauth::OAuthClientBuilder;
use rspotify::model::SearchType;
use rspotify::prelude::*;
use rspotify::{AuthCodeSpotify, ClientResult, Config, Credentials, OAuth, Token};

const CLIENT_ID: &str = "c77da1e492ed47689eec0c61f83e761d";
// Must match the redirect URI registered on the Spotify dashboard
// exactly, including the port -- loopback URIs without a port are no
// longer accepted there.
const REDIRECT_URI: &str = "http://127.0.0.1:8888/callback";
const SCOPES: &[&str] = &[
    "streaming",
    "user-read-email",
    "user-read-private",
    "user-library-read",
    "user-library-modify",
    "user-read-playback-state",
    "user-modify-playback-state",
    "playlist-read-private",
    "playlist-modify-public",
    "playlist-modify-private",
    "user-follow-read",
    "user-follow-modify",
    "user-top-read",
    "user-read-currently-playing",
    "user-read-recently-played",
];

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

fn token_cache_path() -> std::path::PathBuf {
    // Our own cache, separate from ncspot's -- different client_id means
    // a different, non-interchangeable token.
    let dir = directories::ProjectDirs::from("", "", "spot-tui")
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("spot-tui-cache"));
    dir.join("spotify_token.json")
}

fn is_expired(token: &Token) -> bool {
    token.expires_at.map(|exp| exp <= Utc::now()).unwrap_or(true)
}

fn oauth_token_to_rspotify(fresh: librespot_oauth::OAuthToken, prior_refresh: Option<&str>) -> Token {
    let remaining = fresh
        .expires_at
        .saturating_duration_since(std::time::Instant::now());
    let expires_in = chrono::Duration::from_std(remaining).unwrap_or_default();
    // Spotify rotates the refresh_token on every refresh (confirmed
    // live) -- fall back to the prior one only if the response somehow
    // omits a new one.
    let refresh_token = if fresh.refresh_token.is_empty() {
        prior_refresh.map(|s| s.to_string())
    } else {
        Some(fresh.refresh_token)
    };

    Token {
        access_token: fresh.access_token,
        expires_in,
        expires_at: Some(Utc::now() + expires_in),
        refresh_token,
        scopes: fresh.scopes.into_iter().collect(),
    }
}

/// Sync -- must only ever be called via `spawn_blocking`. librespot_oauth's
/// refresh_token() spins up its own blocking runtime internally; calling
/// it directly from inside our already-running tokio context panics
/// ("Cannot drop a runtime in a context where blocking is not allowed"),
/// confirmed live.
fn refresh_blocking(refresh_token: &str) -> Result<Token, String> {
    let client = OAuthClientBuilder::new(CLIENT_ID, REDIRECT_URI, SCOPES.to_vec())
        .build()
        .map_err(|e| e.to_string())?;
    let fresh = client.refresh_token(refresh_token).map_err(|e| e.to_string())?;
    Ok(oauth_token_to_rspotify(fresh, Some(refresh_token)))
}

/// Sync, blocking, and interactive: opens a browser for the user to log
/// in and approve scopes, then blocks listening on `REDIRECT_URI`'s exact
/// port for the callback. Only needed once -- after this, the cached
/// refresh_token means `refresh_blocking` handles everything silently.
fn login_blocking() -> Result<Token, String> {
    let client = OAuthClientBuilder::new(CLIENT_ID, REDIRECT_URI, SCOPES.to_vec())
        .open_in_browser()
        .build()
        .map_err(|e| e.to_string())?;
    let fresh = client.get_access_token().map_err(|e| e.to_string())?;
    Ok(oauth_token_to_rspotify(fresh, None))
}

fn write_token_cache(token: &Token) {
    let path = token_cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(token) {
        let _ = std::fs::write(path, json);
    }
}

/// Loads our own cached token, refreshing if expired, or running a real
/// interactive browser login if there's no cache yet (first run) or the
/// refresh_token itself has stopped working.
pub async fn load_or_refresh_token() -> Result<Token, String> {
    let path = token_cache_path();

    if let Ok(raw) = std::fs::read_to_string(&path) {
        if let Ok(cached) = serde_json::from_str::<Token>(&raw) {
            if !is_expired(&cached) {
                return Ok(cached);
            }
            if let Some(refresh_token) = cached.refresh_token.clone() {
                let refreshed = tokio::task::spawn_blocking(move || refresh_blocking(&refresh_token))
                    .await
                    .map_err(|e| e.to_string())?;
                if let Ok(fresh) = refreshed {
                    write_token_cache(&fresh);
                    return Ok(fresh);
                }
                log::warn!("cached refresh_token no longer works, falling back to interactive login");
            }
        }
    }

    let fresh = tokio::task::spawn_blocking(login_blocking)
        .await
        .map_err(|e| e.to_string())??;
    write_token_cache(&fresh);
    Ok(fresh)
}

pub async fn client_from_token(token: Token) -> AuthCodeSpotify {
    let creds = Credentials::new(CLIENT_ID, "");
    let config = Config {
        token_refreshing: false, // we handle refresh ourselves, above
        ..Config::default()
    };
    let spotify = AuthCodeSpotify::with_config(creds, OAuth::default(), config);
    // Populated right after construction, before any request can race
    // it.
    if let Ok(mut guard) = spotify.get_token().lock().await {
        *guard = Some(token);
    }
    spotify
}

/// Refreshes the client's held token in place if it's expired (or about
/// to be). Without this, a client built once at startup silently goes
/// stale after ~1 hour -- Spotify access tokens are short-lived -- and
/// every search after that fails with 401, confirmed live on a
/// multi-hour-old session. `token_refreshing: false` on the client means
/// rspotify never does this on its own; nothing else calls this path
/// either, so it must run before every use, not just once.
async fn ensure_fresh(client: &AuthCodeSpotify) -> Result<(), String> {
    let (needs_refresh, refresh_token) = {
        let token_arc = client.get_token();
        let guard = token_arc.lock().await.map_err(|_| "lock error".to_string())?;
        let token = guard.as_ref().ok_or("client has no token at all")?;
        let expiring_soon = token
            .expires_at
            .map(|exp| exp <= Utc::now() + chrono::Duration::seconds(60))
            .unwrap_or(true);
        (expiring_soon, token.refresh_token.clone())
    };

    if !needs_refresh {
        return Ok(());
    }

    let refresh_token = refresh_token.ok_or("token expiring but no refresh_token held")?;
    let fresh = tokio::task::spawn_blocking(move || refresh_blocking(&refresh_token))
        .await
        .map_err(|e| e.to_string())??;

    write_token_cache(&fresh);

    let token_arc = client.get_token();
    let mut guard = token_arc.lock().await.map_err(|_| "lock error".to_string())?;
    *guard = Some(fresh);
    Ok(())
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
