// Spotify Web API client bootstrap: PKCE OAuth and token cache/refresh, shared by
// every `api::*` submodule.
//
// Uses spot-tui's own Spotify app (Development Mode), not ncspot's client id:
// ncspot's was caught in a Spotify-side lockdown on third-party Web API access
// (its own search was equally broken). Refresh tokens are locked to the client id
// that issued them, so ncspot's cached token is unusable here; the first run does
// a one-time browser login and caches the token separately.
//
// PKCE (via `librespot_oauth`, as ncspot does) needs only a client id, never a
// secret. The dashboard requires an exact port in the redirect URI, so this uses
// one fixed port that must match `REDIRECT_URI` there.

pub mod album;
pub mod artist;
pub mod devices;
pub mod library;
pub mod playlists;
pub mod queue;
pub mod search;
pub mod track;

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
    // Separate from ncspot's: a different client_id means a different,
    // non-interchangeable token.
    crate::paths::cache_dir().join("spotify_token.json")
}

fn is_expired(token: &Token) -> bool {
    token
        .expires_at
        .map(|exp| exp <= Utc::now())
        .unwrap_or(true)
}

fn oauth_token_to_rspotify(
    fresh: librespot_oauth::OAuthToken,
    prior_refresh: Option<&str>,
) -> Token {
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

/// Blocking: call only via `spawn_blocking`. librespot_oauth's `refresh_token()`
/// builds its own runtime, and calling it inside a running tokio context panics
/// ("Cannot drop a runtime in a context where blocking is not allowed").
fn refresh_blocking(refresh_token: &str) -> Result<Token, String> {
    let client = OAuthClientBuilder::new(CLIENT_ID, REDIRECT_URI, SCOPES.to_vec())
        .build()
        .map_err(|e| e.to_string())?;
    let fresh = client
        .refresh_token(refresh_token)
        .map_err(|e| e.to_string())?;
    Ok(oauth_token_to_rspotify(fresh, Some(refresh_token)))
}

/// Blocking and interactive: opens a browser for login, then listens on
/// `REDIRECT_URI`'s port for the callback. Needed once; afterwards the cached
/// refresh token is used silently.
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

    if let Ok(raw) = std::fs::read_to_string(&path)
        && let Ok(cached) = serde_json::from_str::<Token>(&raw)
    {
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

/// Refreshes the client's token in place if it is expired or about to be.
/// Spotify access tokens last about an hour, and `token_refreshing: false` stops
/// rspotify refreshing on its own, so this must run before every use or calls
/// start failing with 401.
async fn ensure_fresh(client: &AuthCodeSpotify) -> Result<(), String> {
    let (needs_refresh, refresh_token) = {
        let token_arc = client.get_token();
        let guard = token_arc
            .lock()
            .await
            .map_err(|_| "lock error".to_string())?;
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
    let mut guard = token_arc
        .lock()
        .await
        .map_err(|_| "lock error".to_string())?;
    *guard = Some(fresh);
    Ok(())
}

/// Extracts the response body from an HTTP-status failure. `ClientError`'s own
/// text ("status code 400 Bad Request") names the failure but not the reason.
/// The raw text is logged rather than parsed as `ApiError`, so there is always
/// something concrete to read even when parsing would have failed.
pub async fn describe_client_error(err: rspotify::ClientError) -> String {
    match err {
        rspotify::ClientError::Http(http_err) => match *http_err {
            rspotify::http::HttpError::StatusCode(response) => {
                let status = response.status();
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|e| format!("<failed to read body: {e}>"));
                format!("HTTP {status}: {body}")
            }
            other => other.to_string(),
        },
        other => other.to_string(),
    }
}
