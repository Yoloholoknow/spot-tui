//! Signing in and out. Spotify needs two separate logins, each with its own
//! browser prompt:
//!
//! - **Playback**, through librespot's built-in client ID. Only that client's
//!   login is accepted for the token login (`login5`) Connect depends on, and a
//!   developer-mode app's is refused. Librespot stores reusable credentials.
//! - **Web API** (library, playlists, queue), through the user's own Spotify app.
//!   librespot's shared client is rate-limited to the point of returning 429.
//!
//! "Signed in" means both stored logins exist, and a sign-in only repeats the
//! step whose login is missing.

use crate::{api, paths};
use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use librespot_core::config::SessionConfig;
use librespot_core::session::Session;
use librespot_oauth::OAuthClientBuilder;
use std::path::{Path, PathBuf};

/// librespot's own file name for the stored playback credentials.
fn credentials_file() -> PathBuf {
    paths::librespot_cache().join("credentials.json")
}

/// The stored logins, which are exactly what signing out deletes.
fn login_files() -> [PathBuf; 2] {
    [credentials_file(), api::token_cache_path()]
}

pub fn is_signed_in() -> bool {
    all_exist(&login_files())
}

fn all_exist(files: &[PathBuf]) -> bool {
    files.iter().all(|f| f.is_file())
}

/// The librespot cache holding the stored credentials (plus its volume and
/// audio cache, which signing out leaves alone).
pub fn session_cache() -> Result<Cache, String> {
    let dir = paths::librespot_cache();
    paths::ensure_private_dir(&dir).map_err(|e| e.to_string())?;
    Cache::new(Some(&dir), Some(&dir), Some(&dir), None).map_err(|e| e.to_string())
}

/// Where librespot's own client ID has its login callback registered.
const PLAYBACK_REDIRECT_URI: &str = "http://127.0.0.1:8898/login";

/// Interactive: one browser prompt per missing login. The Web API token is
/// saved last, so a sign-in that fails halfway never counts as signed in.
///
/// Prints the sign-in links to stdout: call it with the terminal restored.
pub async fn sign_in() -> Result<(), String> {
    let needs_playback = !credentials_file().is_file();
    let needs_web_api = !api::token_cache_path().is_file();
    let steps = usize::from(needs_playback) + usize::from(needs_web_api);
    println!(
        "\nSigning in to Spotify takes {steps} browser step{}. If nothing opens, copy each link\n\
         into a browser. Press Ctrl+C to cancel.\n",
        if steps == 1 { "" } else { "s" }
    );
    if needs_playback {
        println!("Playback login:");
        sign_in_playback().await?;
    }
    if needs_web_api {
        println!("\nLibrary login (your own Spotify app):");
        let token = tokio::task::spawn_blocking(api::login_blocking)
            .await
            .map_err(|e| e.to_string())??;
        api::write_token_cache(&token);
    }
    Ok(())
}

/// Logs in to Spotify's servers once so librespot stores reusable credentials,
/// then proves they pass the token login Connect needs, so a bad login fails
/// here and not as an endless reconnect later.
async fn sign_in_playback() -> Result<(), String> {
    let access_token = tokio::task::spawn_blocking(|| {
        let client_id = SessionConfig::default().client_id;
        OAuthClientBuilder::new(&client_id, PLAYBACK_REDIRECT_URI, vec!["streaming"])
            .open_in_browser()
            .build()
            .and_then(|client| client.get_access_token())
            .map(|token| token.access_token)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;

    let session = Session::new(SessionConfig::default(), Some(session_cache()?));
    let result = async {
        session
            .connect(Credentials::with_access_token(access_token), true)
            .await
            .map_err(|e| format!("Spotify rejected the login: {e}"))?;
        session
            .login5()
            .auth_token()
            .await
            .map_err(|e| format!("Spotify rejected the stored login: {e}"))
    }
    .await;
    session.shutdown();
    match result {
        Ok(_) => {
            // librespot creates its credentials file with default permissions.
            if let Ok(creds) = std::fs::read(credentials_file()) {
                let _ = paths::write_private(&credentials_file(), &creds);
            }
            Ok(())
        }
        Err(e) => {
            remove_files(&[credentials_file()]);
            Err(e)
        }
    }
}

/// Deletes the stored logins. The next run asks to sign in again.
pub fn sign_out() {
    remove_files(&login_files());
}

fn remove_files(files: &[PathBuf]) {
    for file in files {
        if let Err(e) = std::fs::remove_file(file)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("could not remove {}: {e}", display(file));
        }
    }
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("spot-tui-test-auth-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn signed_in_needs_both_logins() {
        let d = dir("both");
        let files = [d.join("credentials.json"), d.join("token.json")];
        assert!(!all_exist(&files));
        std::fs::write(&files[0], "x").unwrap();
        assert!(!all_exist(&files), "playback login alone is not signed in");
        std::fs::write(&files[1], "x").unwrap();
        assert!(all_exist(&files));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn signing_out_removes_the_logins_and_keeps_everything_else() {
        let d = dir("out");
        let files = [d.join("credentials.json"), d.join("token.json")];
        let keep = d.join("volume");
        for f in files.iter().chain([&keep]) {
            std::fs::write(f, "x").unwrap();
        }
        remove_files(&files);
        assert!(!files[0].exists() && !files[1].exists());
        assert!(keep.exists(), "volume and audio cache survive a sign out");
        remove_files(&files); // already gone: not an error
        let _ = std::fs::remove_dir_all(&d);
    }
}
