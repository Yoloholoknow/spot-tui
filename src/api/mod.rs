//! Spotify Web API client bootstrap: PKCE OAuth, token cache/refresh.
//! Shared across every `api::*` submodule (`search` today; `library`,
//! `playlists`, `queue`, `devices` land alongside the phases that need
//! them -- see the design-scope plan's Architecture changes).
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

pub mod search;

use chrono::Utc;
use librespot_oauth::OAuthClientBuilder;
use rspotify::clients::BaseClient;
use rspotify::{AuthCodeSpotify, Config, Credentials, OAuth, Token};

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
