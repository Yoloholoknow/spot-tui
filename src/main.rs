mod config;
mod lyrics;
mod pins;
mod position;
mod api;
mod spike;
mod ui;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, ExecutableCommand};
use librespot_connect::{ConnectConfig, LoadRequest, LoadRequestOptions, PlayingTrack, Spirc};
use librespot_core::cache::Cache;
use librespot_core::config::{DeviceType, SessionConfig};
use librespot_core::session::Session;
use librespot_metadata::audio::UniqueFields;
use librespot_playback::audio_backend;
use librespot_playback::config::{AudioFormat, PlayerConfig};
use librespot_playback::mixer::{self, MixerConfig};
use librespot_playback::player::{Player, PlayerEvent};
use lyrics::{current_line_index, CachedLyrics, LyricLine, LyricsClient};
use position::PositionTracker;
use std::io::stdout;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use rspotify::AuthCodeSpotify;
use api::library::{FollowedArtist, PlaylistSummary, SavedAlbumSummary};
use api::search::TrackResult;
use ui::{
    filtered_sorted, pinned_first, AppState, ConfirmAction, Fetch, Focus, LibraryState, ListFilter, LyricsState,
    Nav, PendingConfirm, PlaylistDetailState, PlaylistPicker, Screen, SearchState, TextPrompt, TextPromptAction,
    LIBRARY_ENTRIES,
};

enum LibraryFetchResult {
    LikedSongs(Result<Vec<TrackResult>, String>),
    SavedAlbums(Result<Vec<SavedAlbumSummary>, String>),
    FollowedArtists(Result<Vec<FollowedArtist>, String>),
    Playlists(Result<Vec<PlaylistSummary>, String>),
    PlaylistTracks {
        playlist_uri: String,
        result: Result<Vec<TrackResult>, String>,
    },
}

/// Results of Phase 5's mutating calls, following the exact same
/// spawn-then-`try_recv` convention as `LibraryFetchResult` (drained in
/// the same per-frame loop). Every variant that targets a specific
/// playlist carries its URI so a stale response can be checked against
/// whatever's actually showing before being applied.
enum CrudResult {
    PlaylistCreated(Result<PlaylistSummary, String>),
    PlaylistRenamed { playlist_uri: String, new_name: String, result: Result<(), String> },
    PlaylistDeleted { playlist_uri: String, result: Result<(), String> },
    TrackAdded { playlist_uri: String, result: Result<(), String> },
    TrackRemoved { playlist_uri: String, result: Result<(), String> },
    TrackReordered { playlist_uri: String, result: Result<(), String> },
}

/// Owned result of resolving a Sidebar row into an action -- computed in
/// its own statement so the borrow of `app` inside `ui::sidebar_rows(&app)`
/// ends there, before the action actually mutates `app`. A `match`'s
/// scrutinee temporary lives for the whole arm body it's matched into,
/// not just until a binding's last use, so doing the borrow and the
/// mutation in the same match arm does not compile.
enum SidebarAction {
    Goto(Screen),
    OpenPlaylist(PlaylistSummary),
}

const TICK: Duration = Duration::from_millis(100);
const DEBOUNCE: Duration = Duration::from_millis(250);
const SEEK_STEP_MS: i64 = 5000;

#[derive(Clone)]
struct TrackMeta {
    track_id: String,
    artist: String,
    title: String,
    album: Option<String>,
    duration_ms: u32,
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).expect("HOME not set")
}

fn log_file_path() -> PathBuf {
    // macOS convention for app logs, distinct from the cache dir.
    dirs_home().join("Library/Logs/spot-tui/spot-tui.log")
}

fn cache_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "spot-tui")
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("spot-tui-cache"))
}

fn spawn_fetch_thread() -> (mpsc::Sender<(u64, TrackMeta)>, mpsc::Receiver<(u64, CachedLyrics)>) {
    let (req_tx, req_rx) = mpsc::channel::<(u64, TrackMeta)>();
    let (res_tx, res_rx) = mpsc::channel();

    std::thread::spawn(move || {
        let client = LyricsClient::new(cache_dir());
        for (generation, meta) in req_rx {
            let result = client.fetch(
                &meta.track_id,
                &meta.artist,
                &meta.title,
                meta.album.as_deref(),
                meta.duration_ms,
            );
            if res_tx.send((generation, result)).is_err() {
                return;
            }
        }
    });

    (req_tx, res_rx)
}

struct TerminalGuard;

impl TerminalGuard {
    fn new() -> std::io::Result<Self> {
        enable_raw_mode()?;
        execute!(stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = stdout().execute(LeaveAlternateScreen);
    }
}

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = stdout().execute(LeaveAlternateScreen);
        default_hook(info);
    }));
}

fn tmux_toggle_zoom() {
    if std::env::var("TMUX").is_ok() {
        let _ = std::process::Command::new("tmux")
            .args(["resize-pane", "-Z"])
            .status();
    }
}

/// While move-mode is active, `pd.selected` is a raw array index (see
/// `render_playlist_detail`'s doc comment on why pinned-first bubbling is
/// skipped during a move). This finds where `real_index` lands in the
/// normal pinned-first display, so callers can convert back to a display
/// position -- used both entering move-mode (display index -> real
/// index) and exiting it (real index -> display index).
fn display_index_for_real_index(pd: &PlaylistDetailState, pinned_tracks: &std::collections::HashSet<String>, real_index: usize) -> usize {
    let label = |t: &TrackResult| format!("{} \u{2014} {}", t.artist, t.title);
    if let Fetch::Ready(items) = &pd.tracks {
        let display = pinned_first(filtered_sorted(items, &pd.filter, &label), pinned_tracks, |t| t.uri.as_str());
        display.iter().position(|&(i, _)| i == real_index).unwrap_or(real_index)
    } else {
        real_index
    }
}

/// Move-mode just exited (confirm or cancel) -- converts `pd.selected`
/// from a raw array index back to its position in the normal
/// pinned-first display that resumes now, so the highlight stays on the
/// same track rather than landing on an unrelated row whenever a pin is
/// active. Shared by both exit paths (`Enter` confirms, `Esc` cancels).
fn resume_normal_display_index(app: &mut AppState) {
    let new_selected = app
        .playlist_detail
        .as_ref()
        .map(|pd| display_index_for_real_index(pd, &app.pinned_tracks, pd.selected));
    if let (Some(pd), Some(new_selected)) = (&mut app.playlist_detail, new_selected) {
        pd.selected = new_selected;
    }
}

/// Re-fetches one playlist's track list, setting `Fetch::Loading` first.
/// Shared by `open_playlist_detail` (opening fresh) and Phase 5's
/// add/remove-track success handlers (refreshing after a mutation) --
/// the second concrete case that justifies pulling this out of
/// `open_playlist_detail` rather than duplicating its spawn body.
fn refetch_playlist_tracks(
    app: &mut AppState,
    library_tx: &mpsc::Sender<LibraryFetchResult>,
    spotify_client: &Option<AuthCodeSpotify>,
    playlist_uri: String,
) {
    if let Some(pd) = &mut app.playlist_detail {
        if pd.playlist.uri == playlist_uri {
            pd.tracks = Fetch::Loading;
        }
    }
    match spotify_client.clone() {
        Some(client) => {
            let tx = library_tx.clone();
            let uri_for_task = playlist_uri.clone();
            tokio::spawn(async move {
                let result = api::library::playlist_tracks(&client, &uri_for_task).await;
                let _ = tx.send(LibraryFetchResult::PlaylistTracks { playlist_uri: uri_for_task, result });
            });
        }
        None => {
            if let Some(pd) = &mut app.playlist_detail {
                if pd.playlist.uri == playlist_uri {
                    pd.tracks = Fetch::Failed("Spotify client not ready yet".into());
                }
            }
        }
    }
}

/// Re-fetches Your Playlists (and, transitively, the Sidebar's live
/// playlist rows -- both read `app.library.playlists`). There is no
/// generic "whenever `Fetch` is `NotStarted`, refetch" watcher anywhere
/// in this codebase -- the two existing triggers (`client_checked`'s
/// eager fetch, and Library home's push-into-Your-Playlists guard) each
/// fire at most once, at a specific moment, not on every frame. Phase 5's
/// mutations need a third, explicit trigger: call this directly after a
/// create/rename/delete succeeds.
fn refetch_playlists(
    app: &mut AppState,
    library_tx: &mpsc::Sender<LibraryFetchResult>,
    spotify_client: &Option<AuthCodeSpotify>,
) {
    app.library.playlists = Fetch::Loading;
    if let Some(client) = spotify_client.clone() {
        let tx = library_tx.clone();
        tokio::spawn(async move {
            let result = api::library::your_playlists(&client).await.map_err(|e| e.to_string());
            let _ = tx.send(LibraryFetchResult::Playlists(result));
        });
    } else {
        app.library.playlists = Fetch::Failed("Spotify client not ready yet".into());
    }
}

/// Opens Playlist Detail on `playlist` and kicks off its track fetch.
/// Shared by two real call sites now (Sidebar's own playlist rows, and
/// the Your Playlists screen's Enter) -- the second concrete case that
/// justifies pulling this out rather than duplicating it.
fn open_playlist_detail(
    app: &mut AppState,
    library_tx: &mpsc::Sender<LibraryFetchResult>,
    spotify_client: &Option<AuthCodeSpotify>,
    playlist: PlaylistSummary,
) {
    app.nav.push(Screen::PlaylistDetail);
    let playlist_uri = playlist.uri.clone();
    app.playlist_detail = Some(PlaylistDetailState {
        playlist,
        tracks: Fetch::Loading,
        selected: 0,
        filter: ListFilter::default(),
        move_mode: None,
    });
    refetch_playlist_tracks(app, library_tx, spotify_client, playlist_uri);
}

/// A physical Shift+<letter>: matches the literal uppercase char (how
/// most terminals report it) as well as lowercase-plus-SHIFT-modifier
/// (how some terminals/configurations report it instead). Originally
/// written just for Shift+P -- reported live as "pinning does not do
/// anything," most likely this exact platform encoding gap rather than
/// the pin logic itself being wrong -- generalized here as the second
/// concrete case (Shift+D, deleting the open playlist from within
/// Playlist Detail, needs the identical robustness).
fn is_shift_char(code: KeyCode, modifiers: KeyModifiers, upper: char, lower: char) -> bool {
    code == KeyCode::Char(upper) || (code == KeyCode::Char(lower) && modifiers.contains(KeyModifiers::SHIFT))
}

fn is_pin_key(code: KeyCode, modifiers: KeyModifiers) -> bool {
    is_shift_char(code, modifiers, 'P', 'p')
}

/// Fires the mutation behind a confirmed `ConfirmAction`. Split out of
/// `handle_confirm_key` so the "no client yet" bail-out is written once.
fn fire_confirm_action(
    app: &mut AppState,
    action: ConfirmAction,
    crud_tx: &mpsc::Sender<CrudResult>,
    spotify_client: &Option<AuthCodeSpotify>,
) {
    let Some(client) = spotify_client.clone() else {
        app.status = Some(("Spotify client not ready yet".to_string(), true));
        return;
    };
    match action {
        ConfirmAction::DeletePlaylist(playlist) => {
            app.status = Some((format!("deleting \"{}\"\u{2026}", playlist.name), false));
            let tx = crud_tx.clone();
            let playlist_uri = playlist.uri.clone();
            tokio::spawn(async move {
                let result = api::playlists::delete_playlist(&client, &playlist_uri).await;
                let _ = tx.send(CrudResult::PlaylistDeleted { playlist_uri, result });
            });
        }
        ConfirmAction::RemoveTrack { playlist_uri, track_uri } => {
            app.status = Some(("removing track\u{2026}".to_string(), false));
            let tx = crud_tx.clone();
            let playlist_uri_for_result = playlist_uri.clone();
            tokio::spawn(async move {
                let result = api::playlists::remove_track(&client, &playlist_uri, &track_uri).await;
                let _ = tx.send(CrudResult::TrackRemoved { playlist_uri: playlist_uri_for_result, result });
            });
        }
    }
}

/// `y` confirms and fires the mutation; `n`/`Esc` cancels; every other
/// key is swallowed without touching or dismissing the dialog -- an
/// arbitrary keypress shouldn't accidentally confirm or cancel a
/// destructive action.
fn handle_confirm_key(
    app: &mut AppState,
    code: KeyCode,
    crud_tx: &mpsc::Sender<CrudResult>,
    spotify_client: &Option<AuthCodeSpotify>,
) {
    match code {
        // Enter as a synonym for `y` -- reported live as wanted, matches
        // `Enter`'s standing role elsewhere in the app as the one
        // universal "confirm/activate" key.
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
            if let Some(confirm) = app.pending_confirm.take() {
                fire_confirm_action(app, confirm.action, crud_tx, spotify_client);
            }
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            app.pending_confirm = None;
        }
        _ => {}
    }
}

/// `Enter` submits (fires create/rename); `Esc` cancels outright -- unlike
/// `ListFilter`'s filter box, there's no "keep it applied" middle state
/// for a name that was never submitted. Every other key edits the field,
/// same cursor conventions as Search/`ListFilter`.
fn handle_text_prompt_key(
    app: &mut AppState,
    code: KeyCode,
    crud_tx: &mpsc::Sender<CrudResult>,
    spotify_client: &Option<AuthCodeSpotify>,
) {
    match code {
        KeyCode::Esc => {
            app.text_prompt = None;
        }
        KeyCode::Enter => {
            let Some(prompt) = app.text_prompt.take() else { return };
            let name = prompt.query.trim().to_string();
            if name.is_empty() {
                app.status = Some(("name can't be empty".to_string(), true));
                return;
            }
            let Some(client) = spotify_client.clone() else {
                app.status = Some(("Spotify client not ready yet".to_string(), true));
                return;
            };
            let tx = crud_tx.clone();
            match prompt.action {
                TextPromptAction::CreatePlaylist => {
                    app.status = Some(("creating playlist\u{2026}".to_string(), false));
                    tokio::spawn(async move {
                        let result = api::playlists::create_playlist(&client, &name).await;
                        let _ = tx.send(CrudResult::PlaylistCreated(result));
                    });
                }
                TextPromptAction::RenamePlaylist(playlist) => {
                    app.status = Some(("renaming playlist\u{2026}".to_string(), false));
                    let playlist_uri = playlist.uri;
                    tokio::spawn(async move {
                        let result = api::playlists::rename_playlist(&client, &playlist_uri, &name).await;
                        let _ = tx.send(CrudResult::PlaylistRenamed { playlist_uri, new_name: name, result });
                    });
                }
            }
        }
        KeyCode::Backspace => {
            if let Some(prompt) = &mut app.text_prompt {
                prompt.backspace_at_cursor();
            }
        }
        KeyCode::Left => {
            if let Some(prompt) = &mut app.text_prompt {
                prompt.cursor_left();
            }
        }
        KeyCode::Right => {
            if let Some(prompt) = &mut app.text_prompt {
                prompt.cursor_right();
            }
        }
        KeyCode::Char(c) => {
            if let Some(prompt) = &mut app.text_prompt {
                prompt.insert_at_cursor(c);
            }
        }
        _ => {}
    }
}

/// `Up`/`Down` move the picker's own selection; `Enter` adds the captured
/// track to whichever playlist is selected and closes; `Esc` cancels.
fn handle_picker_key(
    app: &mut AppState,
    code: KeyCode,
    crud_tx: &mpsc::Sender<CrudResult>,
    spotify_client: &Option<AuthCodeSpotify>,
) {
    match code {
        KeyCode::Esc => {
            app.playlist_picker = None;
        }
        KeyCode::Up => {
            if let Some(picker) = &mut app.playlist_picker {
                picker.selected = picker.selected.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            let count = match &app.library.playlists {
                Fetch::Ready(items) => items.len(),
                _ => 0,
            };
            if let Some(picker) = &mut app.playlist_picker {
                if count > 0 {
                    picker.selected = (picker.selected + 1).min(count - 1);
                }
            }
        }
        KeyCode::Enter => {
            let label = |p: &PlaylistSummary| p.name.clone();
            let picked_playlist_uri = match &app.library.playlists {
                Fetch::Ready(items) => {
                    let ordered =
                        pinned_first(filtered_sorted(items, &ListFilter::default(), &label), &app.pinned_playlists, |p| {
                            p.uri.as_str()
                        });
                    app.playlist_picker
                        .as_ref()
                        .and_then(|picker| ordered.get(picker.selected).map(|&(_, p)| p.uri.clone()))
                }
                _ => None,
            };
            let Some(picker) = app.playlist_picker.take() else { return };
            let Some(playlist_uri) = picked_playlist_uri else { return };
            let Some(client) = spotify_client.clone() else {
                app.status = Some(("Spotify client not ready yet".to_string(), true));
                return;
            };
            app.status = Some(("adding to playlist\u{2026}".to_string(), false));
            let tx = crud_tx.clone();
            let playlist_uri_for_result = playlist_uri.clone();
            tokio::spawn(async move {
                let result = api::playlists::add_track(&client, &playlist_uri, &picker.track_uri).await;
                let _ = tx.send(CrudResult::TrackAdded { playlist_uri: playlist_uri_for_result, result });
            });
        }
        _ => {}
    }
}

enum LoopExit {
    Quit,
    Disconnected,
}

/// Full librespot/Connect bootstrap, extracted so it can be retried:
/// Tier 4 resilience -- a session drop (laptop sleep, wifi blip) used to
/// leave the app permanently in `SessionEnded` with nothing to recover
/// it. Returns `Err` instead of panicking on any failure, since a failed
/// reconnect attempt should trigger backoff-and-retry, not crash.
async fn connect_spirc() -> Result<
    (
        Spirc,
        tokio::task::JoinHandle<()>,
        librespot_playback::player::PlayerEventChannel,
    ),
    String,
> {
    // credentials_path stays pointed at ncspot's cache -- that's the
    // actual auth-reuse point. volume_path/audio_path get our own
    // directory: librespot treats volume_path as a directory and writes
    // a file literally named `volume` inside it, which collided with
    // ncspot's own pre-existing `volume/` subdirectory at that path
    // (confirmed live: "Cannot save volume to cache: Is a directory").
    let ncspot_librespot_dir = dirs_home().join(".cache/ncspot/librespot");
    let own_librespot_cache_dir = cache_dir().join("librespot");
    let cache = Cache::new(
        Some(&ncspot_librespot_dir),
        Some(&own_librespot_cache_dir),
        Some(&own_librespot_cache_dir),
        None,
    )
    .map_err(|e| e.to_string())?;
    let credentials = cache
        .credentials()
        .ok_or("no cached credentials found in ncspot's cache")?;

    let session = Session::new(SessionConfig::default(), Some(cache));
    let mixer_fn = mixer::find(None).ok_or("no default mixer available")?;
    let mixer = mixer_fn(MixerConfig::default()).map_err(|e| e.to_string())?;
    let backend = audio_backend::find(None).ok_or("no default audio backend")?;
    let soft_volume = mixer.get_soft_volume();
    let player = Player::new(PlayerConfig::default(), session.clone(), soft_volume, move || {
        backend(None, AudioFormat::default())
    });
    let player_events = player.get_player_event_channel();

    let connect_config = ConnectConfig {
        name: "spot-tui".to_string(),
        device_type: DeviceType::Computer,
        // ConnectConfig::default() sets this to u16::MAX / 2 (50%), which
        // feeds the mixer's actual soft-volume attenuation -- a real
        // loudness drop on every fresh launch, not just a displayed
        // number. Start at max instead; loudness is controlled via the
        // system/global volume, matching how it's normally used.
        initial_volume: u16::MAX,
        ..ConnectConfig::default()
    };
    let (spirc, spirc_task) = Spirc::new(connect_config, session, credentials, player, mixer)
        .await
        .map_err(|e| e.to_string())?;
    let spirc_handle = tokio::spawn(spirc_task);

    Ok((spirc, spirc_handle, player_events))
}

fn to_lyrics_state(cached: CachedLyrics) -> LyricsState {
    match cached {
        CachedLyrics::Synced { lines } => LyricsState::Synced(
            lines
                .into_iter()
                .map(|(secs, text)| LyricLine {
                    timestamp: Duration::from_secs_f64(secs),
                    text,
                })
                .collect(),
        ),
        CachedLyrics::Plain { text } => LyricsState::Plain(text),
        CachedLyrics::Instrumental => LyricsState::Instrumental,
        CachedLyrics::NotFound => LyricsState::NotFound,
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // Phase 0 design-scope spike: verify spirc.transfer/device-transfer and
    // the playlist-reorder endpoint before any UI gets built around them.
    // No TUI involved -- runs and exits.
    if std::env::args().any(|a| a == "--spike-phase0") {
        let token = api::load_or_refresh_token()
            .await
            .map_err(std::io::Error::other)?;
        let client = api::client_from_token(token).await;
        if let Err(e) = spike::run_phase0(&client).await {
            eprintln!("spike failed: {e}");
        }
        return Ok(());
    }

    install_panic_hook();
    // env_logger defaults to stderr -- wrong assumption made earlier that
    // this "doesn't collide with the TUI since it only writes to stdout":
    // a real terminal interleaves both streams onto the same screen
    // regardless of which fd wrote what, confirmed live (raw log lines
    // spilling across the TUI when run as a bare command, no redirect).
    // Logging to a file instead avoids needing the user to know to
    // redirect stderr manually -- a real command should just work.
    let log_path = log_file_path();
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(log_file) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
        // "librespot=debug" alone (the original filter) sets everything
        // NOT explicitly named to "off" -- every log::info!/warn! call in
        // this crate's own code (main.rs, api/*.rs) has been silently
        // dropped this whole session as a result, independent of whatever
        // it was trying to report. "info" as the global default keeps
        // librespot's own verbosity at debug while actually letting the
        // app's own logging through.
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,librespot=debug"))
            .target(env_logger::Target::Pipe(Box::new(log_file)))
            .init();
    }

    // -- Web API client for search (Tier 1): bootstrapped eagerly so it's
    // ready by the time the user presses `/`, not fetched on first use.
    // A failure here (e.g. no refresh_token cached, or the shared
    // ncspot client_id rate-limited) disables search only -- playback
    // and lyrics don't depend on this at all.
    let (client_tx, client_rx) = mpsc::channel::<Option<AuthCodeSpotify>>();
    tokio::spawn(async move {
        match api::load_or_refresh_token().await {
            Ok(token) => {
                let _ = client_tx.send(Some(api::client_from_token(token).await));
            }
            Err(e) => {
                log::warn!("search unavailable: failed to load/refresh Spotify token: {e}");
                let _ = client_tx.send(None);
            }
        }
    });
    let mut spotify_client: Option<AuthCodeSpotify> = None;
    let mut client_checked = false;
    let (search_tx, search_rx) = mpsc::channel::<Result<Vec<TrackResult>, String>>();
    let (library_tx, library_rx) = mpsc::channel::<LibraryFetchResult>();
    let (crud_tx, crud_rx) = mpsc::channel::<CrudResult>();

    // -- terminal UI setup (same pattern as ncspot-lyrics) --
    let _guard = TerminalGuard::new()?;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(stdout()))?;

    let (fetch_tx, fetch_rx) = spawn_fetch_thread();
    let cfg = config::load();
    let mut tracker: PositionTracker;
    let mut app = AppState {
        track_title: None,
        track_artist: None,
        track_album: None,
        lyrics: LyricsState::Idle,
        current_line: None,
        fullscreen: false,
        context_lines: cfg.context_lines,
        playing: None,
        position: Duration::ZERO,
        duration: Duration::ZERO,
        volume: u16::MAX, // matches the initial_volume set on connect_config above
        nav: Nav::new(),
        sidebar_sel: 0,
        library: LibraryState::new(),
        playlist_detail: None,
        pinned_playlists: pins::load("playlists"),
        pinned_tracks: pins::load("tracks"),
        search: SearchState::new(),
        pending_confirm: None,
        text_prompt: None,
        playlist_picker: None,
        status: None,
    };

    // Scroll offsets, one per list, persisted across frames and threaded
    // separately from `app` -- see `ui::ScrollState`'s own doc comment
    // for why this isn't just more fields on `AppState`.
    let mut scroll = ui::ScrollState::default();

    let mut generation: u64 = 0;
    let mut pending_fetch: Option<(u64, TrackMeta, Instant)> = None;
    let mut synced_lines: Vec<LyricLine> = Vec::new();

    // Tier 4 resilience: reconnect with capped exponential backoff instead
    // of leaving the app permanently dead after a session drop. Mirrors
    // the same backoff pattern ncspot-lyrics's socket reader already
    // proved out.
    let mut backoff = Duration::from_millis(500);
    const MAX_BACKOFF: Duration = Duration::from_secs(10);

    'outer: loop {
        let (spirc, spirc_handle, mut player_events) = match connect_spirc().await {
            Ok(v) => {
                backoff = Duration::from_millis(500);
                v
            }
            Err(e) => {
                log::warn!("connect failed, retrying in {backoff:?}: {e}");
                app.track_title = None;
                app.track_artist = None;
                app.track_album = None;
                app.playing = None;
                app.lyrics = LyricsState::SessionEnded;
                terminal.draw(|f| ui::render(f, &app, &mut scroll))?;
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue 'outer;
            }
        };

        // Reclaim whatever was last active, matching the official app's
        // "continue where you left off" on launch -- Spirc::transfer with
        // the same device on both ends of the underlying spclient call
        // (see spike.rs's finding) is a no-op if we're already active,
        // so this is safe to call on every (re)connect, not just the
        // first. Untested until run live: this is the first time this
        // codebase has called Spirc::transfer at all, as opposed to
        // Spirc::activate (which explicitly does NOT resume playback).
        match spirc.transfer(None) {
            Ok(()) => log::info!("spirc.transfer(None) sent -- attempting to reclaim last active session"),
            Err(e) => log::warn!("spirc.transfer(None) failed: {e}"),
        }

        // Fresh session: don't keep showing a frozen position/track from
        // whatever the last one was.
        app.lyrics = LyricsState::Idle;
        tracker = PositionTracker::new();

    let exit: LoopExit = 'inner: loop {
        if spirc_handle.is_finished() {
            log::warn!("Spirc task ended -- Connect session dropped, reconnecting");
            break 'inner LoopExit::Disconnected;
        }

        while let Ok(event) = player_events.try_recv() {
            let now = Instant::now();

            if let PlayerEvent::VolumeChanged { volume } = &event {
                app.volume = *volume;
            }

            if let PlayerEvent::TrackChanged { audio_item } = &event {
                let (artist, album) = match &audio_item.unique_fields {
                    UniqueFields::Track { artists, album, .. } => (
                        artists.0.first().map(|a| a.name.clone()).unwrap_or_default(),
                        Some(album.clone()),
                    ),
                    _ => (String::new(), None),
                };
                app.track_title = Some(audio_item.name.clone());
                app.track_artist = if artist.is_empty() { None } else { Some(artist.clone()) };
                app.track_album = album.clone();
                app.duration = Duration::from_millis(audio_item.duration_ms as u64);
                app.lyrics = LyricsState::Loading;
                generation += 1;
                pending_fetch = Some((
                    generation,
                    TrackMeta {
                        track_id: audio_item.track_id.to_string(),
                        artist,
                        title: audio_item.name.clone(),
                        album,
                        duration_ms: audio_item.duration_ms,
                    },
                    Instant::now() + DEBOUNCE,
                ));
            }

            tracker.on_event(&event, now);
        }

        if let Some((fetch_gen, meta, deadline)) = pending_fetch.clone() {
            if Instant::now() >= deadline {
                let _ = fetch_tx.send((fetch_gen, meta));
                pending_fetch = None;
            }
        }

        if !client_checked {
            if let Ok(result) = client_rx.try_recv() {
                app.search.client_ready = result.is_some();
                spotify_client = result;
                client_checked = true;

                // Fetch playlists as soon as the client's ready, not
                // lazily on first Library visit -- the Sidebar shows
                // them directly (see ui::sidebar_rows) and shouldn't sit
                // empty until the user happens to drill into Library
                // first. Guarded the same way the lazy trigger elsewhere
                // is, so whichever fires first wins and the other is a
                // harmless no-op.
                if let (Some(client), true) =
                    (spotify_client.clone(), matches!(app.library.playlists, Fetch::NotStarted))
                {
                    app.library.playlists = Fetch::Loading;
                    let tx = library_tx.clone();
                    tokio::spawn(async move {
                        let result = api::library::your_playlists(&client).await.map_err(|e| e.to_string());
                        let _ = tx.send(LibraryFetchResult::Playlists(result));
                    });
                }
                // Same reasoning, now that Liked Songs is also a direct
                // Sidebar entry: fetch eagerly rather than leaving it
                // stuck on "loading..." forever, since the only other
                // trigger is Library home's own Enter handler, which the
                // Sidebar's direct entry bypasses entirely.
                if let (Some(client), true) =
                    (spotify_client.clone(), matches!(app.library.liked_songs, Fetch::NotStarted))
                {
                    app.library.liked_songs = Fetch::Loading;
                    let tx = library_tx.clone();
                    tokio::spawn(async move {
                        let result = api::library::liked_songs(&client).await.map_err(|e| e.to_string());
                        let _ = tx.send(LibraryFetchResult::LikedSongs(result));
                    });
                }
            }
        }

        while let Ok(result) = search_rx.try_recv() {
            app.search.searching = false;
            match result {
                Ok(results) => {
                    app.search.results = results;
                    app.search.selected = 0;
                    app.search.error = None;
                }
                Err(e) => {
                    app.search.error = Some(e);
                }
            }
        }

        while let Ok(result) = library_rx.try_recv() {
            match result {
                LibraryFetchResult::LikedSongs(r) => {
                    app.library.liked_songs = r.map_or_else(Fetch::Failed, Fetch::Ready);
                }
                LibraryFetchResult::SavedAlbums(r) => {
                    app.library.saved_albums = r.map_or_else(Fetch::Failed, Fetch::Ready);
                }
                LibraryFetchResult::FollowedArtists(r) => {
                    app.library.followed_artists = r.map_or_else(Fetch::Failed, Fetch::Ready);
                }
                LibraryFetchResult::Playlists(r) => {
                    app.library.playlists = r.map_or_else(Fetch::Failed, Fetch::Ready);
                }
                LibraryFetchResult::PlaylistTracks { playlist_uri, result } => {
                    // Guard against a stale fetch for a playlist the user
                    // has since backed out of overwriting whichever one
                    // is actually showing now.
                    if let Some(pd) = &mut app.playlist_detail {
                        if pd.playlist.uri == playlist_uri {
                            pd.tracks = result.map_or_else(Fetch::Failed, Fetch::Ready);
                        }
                    }
                }
            }
        }

        while let Ok(result) = crud_rx.try_recv() {
            match result {
                CrudResult::PlaylistCreated(Ok(_)) => {
                    app.status = Some(("playlist created".to_string(), false));
                    refetch_playlists(&mut app, &library_tx, &spotify_client);
                }
                CrudResult::PlaylistCreated(Err(e)) => {
                    app.status = Some((format!("create playlist failed: {e}"), true));
                }
                CrudResult::PlaylistRenamed { playlist_uri, new_name, result: Ok(()) } => {
                    app.status = Some(("playlist renamed".to_string(), false));
                    // Your Playlists/Sidebar both read `library.playlists`,
                    // refetched below -- but Playlist Detail's header reads
                    // its own separate `pd.playlist.name` copy, which a
                    // refetch of the *list* never touches. Patch it
                    // directly so an already-open detail view doesn't keep
                    // showing the old name until backed out and reopened.
                    if let Some(pd) = &mut app.playlist_detail {
                        if pd.playlist.uri == playlist_uri {
                            pd.playlist.name = new_name;
                        }
                    }
                    refetch_playlists(&mut app, &library_tx, &spotify_client);
                }
                CrudResult::PlaylistRenamed { result: Err(e), .. } => {
                    app.status = Some((format!("rename failed: {e}"), true));
                }
                CrudResult::PlaylistDeleted { playlist_uri, result: Ok(()) } => {
                    app.status = Some(("playlist deleted".to_string(), false));
                    // The detail screen for a playlist that no longer
                    // exists has nothing left to show -- back out to Your
                    // Playlists rather than leave stale tracks on screen.
                    if app.playlist_detail.as_ref().is_some_and(|pd| pd.playlist.uri == playlist_uri) {
                        app.playlist_detail = None;
                        app.nav.goto(Screen::YourPlaylists);
                    }
                    refetch_playlists(&mut app, &library_tx, &spotify_client);
                }
                CrudResult::PlaylistDeleted { result: Err(e), .. } => {
                    app.status = Some((format!("delete failed: {e}"), true));
                }
                CrudResult::TrackAdded { playlist_uri, result: Ok(()) } => {
                    app.status = Some(("added to playlist".to_string(), false));
                    // Same staleness guard as TrackRemoved -- rare (the
                    // add-to-playlist target is usually a *different*
                    // playlist than whichever one's open), but if they
                    // happen to coincide, its track list shouldn't sit
                    // stale until backed out and reopened.
                    if app.playlist_detail.as_ref().is_some_and(|pd| pd.playlist.uri == playlist_uri) {
                        refetch_playlist_tracks(&mut app, &library_tx, &spotify_client, playlist_uri);
                    }
                }
                CrudResult::TrackAdded { result: Err(e), .. } => {
                    app.status = Some((format!("add to playlist failed: {e}"), true));
                }
                CrudResult::TrackRemoved { playlist_uri, result: Ok(()) } => {
                    app.status = Some(("removed from playlist".to_string(), false));
                    if app.playlist_detail.as_ref().is_some_and(|pd| pd.playlist.uri == playlist_uri) {
                        refetch_playlist_tracks(&mut app, &library_tx, &spotify_client, playlist_uri);
                    }
                }
                CrudResult::TrackRemoved { result: Err(e), .. } => {
                    app.status = Some((format!("remove track failed: {e}"), true));
                }
                CrudResult::TrackReordered { playlist_uri, result: Ok(()) } => {
                    app.status = Some(("reordered".to_string(), false));
                    // Refetches to reconcile with the server's own
                    // snapshot_id even though the local Vec was already
                    // optimistically reordered during move-mode -- cheap
                    // insurance against drift, matching every other
                    // mutation's refetch-on-success convention.
                    if app.playlist_detail.as_ref().is_some_and(|pd| pd.playlist.uri == playlist_uri) {
                        refetch_playlist_tracks(&mut app, &library_tx, &spotify_client, playlist_uri);
                    }
                }
                CrudResult::TrackReordered { playlist_uri, result: Err(e) } => {
                    app.status = Some((format!("reorder failed: {e}"), true));
                    // Unlike every other mutation here, the local list was
                    // already optimistically reordered *before* this
                    // result arrived (that's the whole point of move-mode
                    // not hitting the network per keystroke) -- on
                    // failure that local order is now wrong and has to be
                    // refetched away, not just left showing a move that
                    // never actually happened server-side.
                    if app.playlist_detail.as_ref().is_some_and(|pd| pd.playlist.uri == playlist_uri) {
                        refetch_playlist_tracks(&mut app, &library_tx, &spotify_client, playlist_uri);
                    }
                }
            }
        }

        while let Ok((fetch_gen, result)) = fetch_rx.try_recv() {
            if fetch_gen == generation {
                app.lyrics = to_lyrics_state(result);
                if let LyricsState::Synced(lines) = &app.lyrics {
                    synced_lines = lines.clone();
                }
            }
        }

        let now = Instant::now();
        app.position = Duration::from_millis(tracker.progress_ms(now) as u64);
        app.playing = if tracker.current_track_id().is_some() {
            Some(tracker.is_playing())
        } else {
            None
        };
        app.current_line = if matches!(app.lyrics, LyricsState::Synced(_)) {
            current_line_index(&synced_lines, app.position)
        } else {
            None
        };

        terminal.draw(|f| ui::render(f, &app, &mut scroll))?;

        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                // A mutation's result message (success or failure) shows
                // until the next keypress, same lifetime a status line
                // conventionally gets.
                app.status = None;

                // Phase 5's transient overlays (name prompt, yes/no
                // confirm, playlist picker) fully own the keypress when
                // active -- checked before even `Tab`, so none of them
                // leak through to the screen underneath. See the doc
                // comment above `ui::TextPrompt` for why these are
                // sibling `Option`s here rather than `Screen` variants.
                if app.pending_confirm.is_some() {
                    handle_confirm_key(&mut app, key.code, &crud_tx, &spotify_client);
                } else if app.text_prompt.is_some() {
                    handle_text_prompt_key(&mut app, key.code, &crud_tx, &spotify_client);
                } else if app.playlist_picker.is_some() {
                    handle_picker_key(&mut app, key.code, &crud_tx, &spotify_client);
                // Tab is a distinct KeyCode, never a `Char(_)` -- safe to
                // intercept before anything else without ever eating a
                // literal keystroke a text field might want.
                } else if key.code == KeyCode::Tab {
                    app.nav.toggle_focus();
                } else if app.nav.focus == Focus::Sidebar {
                    match key.code {
                        KeyCode::Char('q') => break 'inner LoopExit::Quit,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break 'inner LoopExit::Quit
                        }
                        KeyCode::Char('?') => {
                            app.nav.push(Screen::Help);
                            // Every other Sidebar activation (Enter/Right)
                            // hands focus to Main -- this one didn't,
                            // leaving focus on Sidebar while Help showed
                            // in Main. Sidebar-focus has no Esc binding
                            // of its own, so Esc then did nothing at all.
                            app.nav.focus = Focus::Main;
                        }
                        KeyCode::Char('f') if *app.nav.top() == Screen::NowPlaying => {
                            app.fullscreen = !app.fullscreen;
                            tmux_toggle_zoom();
                        }
                        KeyCode::Char('/') => {
                            app.nav.goto(Screen::Search);
                            app.nav.focus = Focus::Main;
                            app.search.query.clear();
                            app.search.cursor = 0;
                            app.search.results.clear();
                            app.search.error = None;
                        }
                        KeyCode::Char('l') => {
                            app.nav.goto(Screen::Library);
                            app.nav.focus = Focus::Main;
                        }
                        KeyCode::Char(c) if is_pin_key(KeyCode::Char(c), key.modifiers) => {
                            let playlist_uri = match ui::sidebar_rows(&app).get(app.sidebar_sel) {
                                Some(ui::SidebarRow::Playlist(p)) => Some(p.uri.clone()),
                                _ => None,
                            };
                            if let Some(uri) = playlist_uri {
                                pins::toggle_in_place(&mut app.pinned_playlists, &uri);
                                pins::save("playlists", &app.pinned_playlists);
                            }
                        }
                        KeyCode::Char(' ') => {
                            let _ = spirc.play_pause();
                        }
                        KeyCode::Char('n') => {
                            let _ = spirc.next();
                        }
                        KeyCode::Char('p') => {
                            let _ = spirc.prev();
                        }
                        KeyCode::Char('+') => {
                            let _ = spirc.volume_up();
                        }
                        KeyCode::Char('-') => {
                            let _ = spirc.volume_down();
                        }
                        // Seek narrowed to just Now Playing (Main-focus) --
                        // it's the one screen seeking is actually about.
                        // Left/Right here instead mirror yazi/ranger-style
                        // pane navigation: Right activates the selected
                        // Sidebar row (same as Enter), matching "drill
                        // into the right pane." Left has nothing further
                        // left to go to from the Sidebar, so stays unbound.
                        KeyCode::Up => {
                            app.sidebar_sel = app.sidebar_sel.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            let row_count = ui::sidebar_rows(&app).len();
                            app.sidebar_sel = (app.sidebar_sel + 1).min(row_count.saturating_sub(1));
                        }
                        KeyCode::Enter | KeyCode::Right => {
                            let action = match ui::sidebar_rows(&app).get(app.sidebar_sel) {
                                Some(ui::SidebarRow::Menu(_, screen)) => Some(SidebarAction::Goto(*screen)),
                                Some(ui::SidebarRow::Playlist(p)) => {
                                    Some(SidebarAction::OpenPlaylist((*p).clone()))
                                }
                                None => None,
                            };
                            match action {
                                Some(SidebarAction::Goto(screen)) => {
                                    app.nav.goto(screen);
                                    app.nav.focus = Focus::Main;
                                    if screen == Screen::Search {
                                        app.search.query.clear();
                                        app.search.cursor = 0;
                                        app.search.results.clear();
                                        app.search.error = None;
                                    }
                                }
                                Some(SidebarAction::OpenPlaylist(playlist)) => {
                                    open_playlist_detail(&mut app, &library_tx, &spotify_client, playlist);
                                    app.nav.focus = Focus::Main;
                                }
                                None => {}
                            }
                        }
                        _ => {}
                    }
                } else {
                    // focus == Main
                    match *app.nav.top() {
                        Screen::Search => match key.code {
                            // Not Left -- this is a text-input screen; a
                            // directional key needed for moving the
                            // cursor within the query can't also mean
                            // "leave," or editing becomes a minefield.
                            // Reported live: arrow-key cursor movement
                            // didn't work at all here (always had to
                            // backspace-and-retype for a mid-query fix).
                            KeyCode::Esc => {
                                app.nav.escape();
                            }
                            KeyCode::Backspace => {
                                app.search.backspace_at_cursor();
                                app.search.results.clear();
                                app.search.error = None;
                            }
                            KeyCode::Left => {
                                app.search.cursor_left();
                            }
                            KeyCode::Right => {
                                app.search.cursor_right();
                            }
                            KeyCode::Up => {
                                app.search.selected = app.search.selected.saturating_sub(1);
                            }
                            KeyCode::Down => {
                                if !app.search.results.is_empty() {
                                    app.search.selected =
                                        (app.search.selected + 1).min(app.search.results.len() - 1);
                                }
                            }
                            // Not Right -- Enter here can play a track and jump to Now Playing,
                            // a real "leave here" side effect. Right means "go deeper," not
                            // "commit and teleport" (reported live as feeling unnatural).
                            KeyCode::Enter => {
                                if app.search.results.is_empty() {
                                    if let Some(client) = spotify_client.clone() {
                                        if !app.search.query.trim().is_empty() {
                                            let query = app.search.query.clone();
                                            let tx = search_tx.clone();
                                            app.search.searching = true;
                                            app.search.error = None;
                                            tokio::spawn(async move {
                                                // Dev Mode apps cap search at 10 results (down
                                                // from 50 as of Spotify's Feb 2026 migration) --
                                                // confirmed live, anything higher is a 400
                                                // "Invalid limit".
                                                let result = api::search::search_tracks(&client, &query, 10)
                                                    .await
                                                    .map_err(|e| e.to_string());
                                                let _ = tx.send(result);
                                            });
                                        }
                                    }
                                } else if let Some(track) =
                                    app.search.results.get(app.search.selected).cloned()
                                {
                                    // activate() must precede load(): Spirc
                                    // ignores Load while not the active
                                    // device (confirmed live, logged plainly).
                                    let _ = spirc.activate();
                                    let _ = spirc.load(LoadRequest::from_context_uri(
                                        track.uri,
                                        Default::default(),
                                    ));
                                    let _ = spirc.play();
                                    app.nav.goto(Screen::NowPlaying);
                                }
                            }
                            KeyCode::Char(c) => {
                                app.search.insert_at_cursor(c);
                                app.search.results.clear();
                                app.search.error = None;
                            }
                            _ => {}
                        },
                        Screen::NowPlaying => match key.code {
                            KeyCode::Esc => {
                                app.nav.escape();
                            }
                            KeyCode::Char('q') => break 'inner LoopExit::Quit,
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                break 'inner LoopExit::Quit
                            }
                            KeyCode::Char('?') => {
                                app.nav.push(Screen::Help);
                            }
                            KeyCode::Char('f') => {
                                app.fullscreen = !app.fullscreen;
                                tmux_toggle_zoom();
                            }
                            KeyCode::Char('/') => {
                                app.nav.push(Screen::Search);
                                app.search.query.clear();
                                app.search.cursor = 0;
                                app.search.results.clear();
                                app.search.error = None;
                            }
                            // `goto`, not `push` -- a universal "jump to Library" shortcut
                            // should always land on exactly [NowPlaying, Library], never pile
                            // up stack depth if pressed again from somewhere already nested
                            // under Library (LikedSongs, Playlist Detail, etc).
                            KeyCode::Char('l') => {
                                app.nav.goto(Screen::Library);
                            }
                            KeyCode::Char('c') => {
                                app.text_prompt =
                                    Some(TextPrompt::new("New playlist name", "", TextPromptAction::CreatePlaylist));
                            }
                            // Adds whatever's currently playing -- the one screen where
                            // "the selected track" means the track itself, not a list row.
                            KeyCode::Char('a') => {
                                if let Some(track_id) = tracker.current_track_id() {
                                    app.playlist_picker =
                                        Some(PlaylistPicker { track_uri: track_id.to_string(), selected: 0 });
                                }
                            }
                            KeyCode::Char(' ') => {
                                let _ = spirc.play_pause();
                            }
                            KeyCode::Char('n') => {
                                let _ = spirc.next();
                            }
                            KeyCode::Char('p') => {
                                let _ = spirc.prev();
                            }
                            KeyCode::Char('+') | KeyCode::Up => {
                                let _ = spirc.volume_up();
                            }
                            KeyCode::Char('-') | KeyCode::Down => {
                                let _ = spirc.volume_down();
                            }
                            KeyCode::Left => {
                                let target = (tracker.progress_ms(Instant::now()) as i64
                                    - SEEK_STEP_MS)
                                    .max(0);
                                let _ = spirc.set_position_ms(target as u32);
                            }
                            KeyCode::Right => {
                                let target =
                                    tracker.progress_ms(Instant::now()) as i64 + SEEK_STEP_MS;
                                let _ = spirc.set_position_ms(target as u32);
                            }
                            _ => {}
                        },
                        Screen::Library => match key.code {
                            KeyCode::Char('q') => break 'inner LoopExit::Quit,
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                break 'inner LoopExit::Quit
                            }
                            KeyCode::Char('?') => {
                                app.nav.push(Screen::Help);
                            }
                            KeyCode::Char(' ') => {
                                let _ = spirc.play_pause();
                            }
                            KeyCode::Char('n') => {
                                let _ = spirc.next();
                            }
                            KeyCode::Char('p') => {
                                let _ = spirc.prev();
                            }
                            KeyCode::Char('+') => {
                                let _ = spirc.volume_up();
                            }
                            KeyCode::Char('-') => {
                                let _ = spirc.volume_down();
                            }
                            KeyCode::Char('l') => {
                                app.nav.goto(Screen::Library);
                            }
                            KeyCode::Char('c') => {
                                app.text_prompt =
                                    Some(TextPrompt::new("New playlist name", "", TextPromptAction::CreatePlaylist));
                            }
                            KeyCode::Esc | KeyCode::Left => {
                                app.nav.escape();
                            }
                            KeyCode::Up => {
                                app.library.home_selected = app.library.home_selected.saturating_sub(1);
                            }
                            KeyCode::Down => {
                                app.library.home_selected =
                                    (app.library.home_selected + 1).min(LIBRARY_ENTRIES.len() - 1);
                            }
                            KeyCode::Enter | KeyCode::Right => {
                                let (_, screen) = LIBRARY_ENTRIES[app.library.home_selected];
                                app.nav.push(screen);
                                match screen {
                                    Screen::LikedSongs
                                        if matches!(app.library.liked_songs, Fetch::NotStarted) =>
                                    {
                                        app.library.liked_songs = Fetch::Loading;
                                        match spotify_client.clone() {
                                            Some(client) => {
                                                let tx = library_tx.clone();
                                                tokio::spawn(async move {
                                                    let result = api::library::liked_songs(&client)
                                                        .await
                                                        .map_err(|e| e.to_string());
                                                    let _ = tx.send(LibraryFetchResult::LikedSongs(result));
                                                });
                                            }
                                            None => {
                                                app.library.liked_songs =
                                                    Fetch::Failed("Spotify client not ready yet".into());
                                            }
                                        }
                                    }
                                    Screen::SavedAlbums
                                        if matches!(app.library.saved_albums, Fetch::NotStarted) =>
                                    {
                                        app.library.saved_albums = Fetch::Loading;
                                        match spotify_client.clone() {
                                            Some(client) => {
                                                let tx = library_tx.clone();
                                                tokio::spawn(async move {
                                                    let result = api::library::saved_albums(&client)
                                                        .await
                                                        .map_err(|e| e.to_string());
                                                    let _ = tx.send(LibraryFetchResult::SavedAlbums(result));
                                                });
                                            }
                                            None => {
                                                app.library.saved_albums =
                                                    Fetch::Failed("Spotify client not ready yet".into());
                                            }
                                        }
                                    }
                                    Screen::FollowedArtists
                                        if matches!(app.library.followed_artists, Fetch::NotStarted) =>
                                    {
                                        app.library.followed_artists = Fetch::Loading;
                                        match spotify_client.clone() {
                                            Some(client) => {
                                                let tx = library_tx.clone();
                                                tokio::spawn(async move {
                                                    let result = api::library::followed_artists(&client)
                                                        .await
                                                        .map_err(|e| e.to_string());
                                                    let _ =
                                                        tx.send(LibraryFetchResult::FollowedArtists(result));
                                                });
                                            }
                                            None => {
                                                app.library.followed_artists =
                                                    Fetch::Failed("Spotify client not ready yet".into());
                                            }
                                        }
                                    }
                                    Screen::YourPlaylists
                                        if matches!(app.library.playlists, Fetch::NotStarted) =>
                                    {
                                        app.library.playlists = Fetch::Loading;
                                        match spotify_client.clone() {
                                            Some(client) => {
                                                let tx = library_tx.clone();
                                                tokio::spawn(async move {
                                                    let result = api::library::your_playlists(&client)
                                                        .await
                                                        .map_err(|e| e.to_string());
                                                    let _ = tx.send(LibraryFetchResult::Playlists(result));
                                                });
                                            }
                                            None => {
                                                app.library.playlists =
                                                    Fetch::Failed("Spotify client not ready yet".into());
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        },
                        Screen::LikedSongs => {
                            let label = |t: &TrackResult| format!("{} \u{2014} {}", t.artist, t.title);
                            match key.code {
                                // Filter editing intercepts everything typing-related --
                                // Left/Right move the cursor mid-string (same as Search),
                                // Up/Down fall through unguarded below so they still move
                                // the highlighted track live while typing, reported live
                                // as wanted.
                                KeyCode::Char(c) if app.library.liked_songs_filter.editing => {
                                    app.library.liked_songs_filter.insert_at_cursor(c);
                                    app.library.liked_songs_selected = 0;
                                }
                                KeyCode::Backspace if app.library.liked_songs_filter.editing => {
                                    app.library.liked_songs_filter.backspace_at_cursor();
                                    app.library.liked_songs_selected = 0;
                                }
                                KeyCode::Enter if app.library.liked_songs_filter.editing => {
                                    app.library.liked_songs_filter.editing = false;
                                }
                                KeyCode::Esc if app.library.liked_songs_filter.editing => {
                                    app.library.liked_songs_filter.cancel_editing();
                                    app.library.liked_songs_selected = 0;
                                }
                                // Left/Right move the cursor mid-string while typing (same as
                                // Search) -- two other designs (pane-nav passthrough, no-op)
                                // were tried and rejected live as feeling broken.
                                KeyCode::Left if app.library.liked_songs_filter.editing => {
                                    app.library.liked_songs_filter.cursor_left();
                                }
                                KeyCode::Right if app.library.liked_songs_filter.editing => {
                                    app.library.liked_songs_filter.cursor_right();
                                }
                                KeyCode::Char('q') => break 'inner LoopExit::Quit,
                                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                    break 'inner LoopExit::Quit
                                }
                                KeyCode::Char('?') => {
                                    app.nav.push(Screen::Help);
                                }
                                KeyCode::Char(' ') => {
                                    let _ = spirc.play_pause();
                                }
                                KeyCode::Char('n') => {
                                    let _ = spirc.next();
                                }
                                KeyCode::Char('p') => {
                                    let _ = spirc.prev();
                                }
                                KeyCode::Char('+') => {
                                    let _ = spirc.volume_up();
                                }
                                KeyCode::Char('-') => {
                                    let _ = spirc.volume_down();
                                }
                                KeyCode::Char('l') => {
                                    app.nav.goto(Screen::Library);
                                }
                                KeyCode::Char('c') => {
                                    app.text_prompt = Some(TextPrompt::new(
                                        "New playlist name",
                                        "",
                                        TextPromptAction::CreatePlaylist,
                                    ));
                                }
                                // A filter that's applied but no longer being typed still
                                // has to be dismissed with its own Esc first -- otherwise
                                // Esc immediately leaves the screen with the filter
                                // silently still narrowing the list next time it's opened.
                                KeyCode::Esc if !app.library.liked_songs_filter.query.is_empty() => {
                                    app.library.liked_songs_filter.query.clear();
                                    app.library.liked_songs_filter.cursor = 0;
                                    app.library.liked_songs_selected = 0;
                                }
                                KeyCode::Esc | KeyCode::Left => {
                                    app.nav.escape();
                                }
                                KeyCode::Char('/') => {
                                    app.library.liked_songs_filter.start_editing();
                                }
                                KeyCode::Char('o') => {
                                    app.library.liked_songs_filter.sort_alpha =
                                        !app.library.liked_songs_filter.sort_alpha;
                                }
                                KeyCode::Char('a') => {
                                    if let Fetch::Ready(items) = &app.library.liked_songs {
                                        let display = filtered_sorted(items, &app.library.liked_songs_filter, &label);
                                        if let Some(track) =
                                            display.get(app.library.liked_songs_selected).map(|&(_, t)| t.clone())
                                        {
                                            app.playlist_picker =
                                                Some(PlaylistPicker { track_uri: track.uri, selected: 0 });
                                        }
                                    }
                                }
                                KeyCode::Up => {
                                    app.library.liked_songs_selected =
                                        app.library.liked_songs_selected.saturating_sub(1);
                                }
                                KeyCode::Down => {
                                    if let Fetch::Ready(items) = &app.library.liked_songs {
                                        let display = filtered_sorted(items, &app.library.liked_songs_filter, &label);
                                        if !display.is_empty() {
                                            app.library.liked_songs_selected =
                                                (app.library.liked_songs_selected + 1).min(display.len() - 1);
                                        }
                                    }
                                }
                                // Not Right -- Enter here can play a track and jump to Now Playing,
                                // a real "leave here" side effect. Right means "go deeper," not
                                // "commit and teleport" (reported live as feeling unnatural).
                                KeyCode::Enter => {
                                    if let Fetch::Ready(items) = &app.library.liked_songs {
                                        let display = filtered_sorted(items, &app.library.liked_songs_filter, &label);
                                        if let Some(track) =
                                            display.get(app.library.liked_songs_selected).map(|&(_, t)| t.clone())
                                        {
                                            let _ = spirc.activate();
                                            let _ = spirc.load(LoadRequest::from_context_uri(
                                                track.uri,
                                                Default::default(),
                                            ));
                                            let _ = spirc.play();
                                            app.nav.goto(Screen::NowPlaying);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        Screen::SavedAlbums => {
                            let label = |a: &SavedAlbumSummary| format!("{} \u{2014} {}", a.name, a.artist);
                            match key.code {
                                KeyCode::Char(c) if app.library.saved_albums_filter.editing => {
                                    app.library.saved_albums_filter.insert_at_cursor(c);
                                    app.library.saved_albums_selected = 0;
                                }
                                KeyCode::Backspace if app.library.saved_albums_filter.editing => {
                                    app.library.saved_albums_filter.backspace_at_cursor();
                                    app.library.saved_albums_selected = 0;
                                }
                                KeyCode::Enter if app.library.saved_albums_filter.editing => {
                                    app.library.saved_albums_filter.editing = false;
                                }
                                KeyCode::Esc if app.library.saved_albums_filter.editing => {
                                    app.library.saved_albums_filter.cancel_editing();
                                    app.library.saved_albums_selected = 0;
                                }
                                KeyCode::Left if app.library.saved_albums_filter.editing => {
                                    app.library.saved_albums_filter.cursor_left();
                                }
                                KeyCode::Right if app.library.saved_albums_filter.editing => {
                                    app.library.saved_albums_filter.cursor_right();
                                }
                                KeyCode::Char('q') => break 'inner LoopExit::Quit,
                                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                    break 'inner LoopExit::Quit
                                }
                                KeyCode::Char('?') => {
                                    app.nav.push(Screen::Help);
                                }
                                KeyCode::Char(' ') => {
                                    let _ = spirc.play_pause();
                                }
                                KeyCode::Char('n') => {
                                    let _ = spirc.next();
                                }
                                KeyCode::Char('p') => {
                                    let _ = spirc.prev();
                                }
                                KeyCode::Char('+') => {
                                    let _ = spirc.volume_up();
                                }
                                KeyCode::Char('-') => {
                                    let _ = spirc.volume_down();
                                }
                                KeyCode::Char('l') => {
                                    app.nav.goto(Screen::Library);
                                }
                                KeyCode::Char('c') => {
                                    app.text_prompt = Some(TextPrompt::new(
                                        "New playlist name",
                                        "",
                                        TextPromptAction::CreatePlaylist,
                                    ));
                                }
                                KeyCode::Esc if !app.library.saved_albums_filter.query.is_empty() => {
                                    app.library.saved_albums_filter.query.clear();
                                    app.library.saved_albums_filter.cursor = 0;
                                    app.library.saved_albums_selected = 0;
                                }
                                KeyCode::Esc | KeyCode::Left => {
                                    app.nav.escape();
                                }
                                KeyCode::Char('/') => {
                                    app.library.saved_albums_filter.start_editing();
                                }
                                KeyCode::Char('o') => {
                                    app.library.saved_albums_filter.sort_alpha =
                                        !app.library.saved_albums_filter.sort_alpha;
                                }
                                KeyCode::Up => {
                                    app.library.saved_albums_selected =
                                        app.library.saved_albums_selected.saturating_sub(1);
                                }
                                KeyCode::Down => {
                                    if let Fetch::Ready(items) = &app.library.saved_albums {
                                        let display = filtered_sorted(items, &app.library.saved_albums_filter, &label);
                                        if !display.is_empty() {
                                            app.library.saved_albums_selected =
                                                (app.library.saved_albums_selected + 1).min(display.len() - 1);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        Screen::FollowedArtists => {
                            let label = |a: &FollowedArtist| a.name.clone();
                            match key.code {
                                KeyCode::Char(c) if app.library.followed_artists_filter.editing => {
                                    app.library.followed_artists_filter.insert_at_cursor(c);
                                    app.library.followed_artists_selected = 0;
                                }
                                KeyCode::Backspace if app.library.followed_artists_filter.editing => {
                                    app.library.followed_artists_filter.backspace_at_cursor();
                                    app.library.followed_artists_selected = 0;
                                }
                                KeyCode::Enter if app.library.followed_artists_filter.editing => {
                                    app.library.followed_artists_filter.editing = false;
                                }
                                KeyCode::Esc if app.library.followed_artists_filter.editing => {
                                    app.library.followed_artists_filter.cancel_editing();
                                    app.library.followed_artists_selected = 0;
                                }
                                KeyCode::Left if app.library.followed_artists_filter.editing => {
                                    app.library.followed_artists_filter.cursor_left();
                                }
                                KeyCode::Right if app.library.followed_artists_filter.editing => {
                                    app.library.followed_artists_filter.cursor_right();
                                }
                                KeyCode::Char('q') => break 'inner LoopExit::Quit,
                                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                    break 'inner LoopExit::Quit
                                }
                                KeyCode::Char('?') => {
                                    app.nav.push(Screen::Help);
                                }
                                KeyCode::Char(' ') => {
                                    let _ = spirc.play_pause();
                                }
                                KeyCode::Char('n') => {
                                    let _ = spirc.next();
                                }
                                KeyCode::Char('p') => {
                                    let _ = spirc.prev();
                                }
                                KeyCode::Char('+') => {
                                    let _ = spirc.volume_up();
                                }
                                KeyCode::Char('-') => {
                                    let _ = spirc.volume_down();
                                }
                                KeyCode::Char('l') => {
                                    app.nav.goto(Screen::Library);
                                }
                                KeyCode::Char('c') => {
                                    app.text_prompt = Some(TextPrompt::new(
                                        "New playlist name",
                                        "",
                                        TextPromptAction::CreatePlaylist,
                                    ));
                                }
                                KeyCode::Esc if !app.library.followed_artists_filter.query.is_empty() => {
                                    app.library.followed_artists_filter.query.clear();
                                    app.library.followed_artists_filter.cursor = 0;
                                    app.library.followed_artists_selected = 0;
                                }
                                KeyCode::Esc | KeyCode::Left => {
                                    app.nav.escape();
                                }
                                KeyCode::Char('/') => {
                                    app.library.followed_artists_filter.start_editing();
                                }
                                KeyCode::Char('o') => {
                                    app.library.followed_artists_filter.sort_alpha =
                                        !app.library.followed_artists_filter.sort_alpha;
                                }
                                KeyCode::Up => {
                                    app.library.followed_artists_selected =
                                        app.library.followed_artists_selected.saturating_sub(1);
                                }
                                KeyCode::Down => {
                                    if let Fetch::Ready(items) = &app.library.followed_artists {
                                        let display = filtered_sorted(items, &app.library.followed_artists_filter, &label);
                                        if !display.is_empty() {
                                            app.library.followed_artists_selected =
                                                (app.library.followed_artists_selected + 1).min(display.len() - 1);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        Screen::YourPlaylists => {
                            let label =
                                |p: &PlaylistSummary| format!("{} ({} tracks)", p.name, p.track_count);
                            match key.code {
                                KeyCode::Char(c) if app.library.playlists_filter.editing => {
                                    app.library.playlists_filter.insert_at_cursor(c);
                                    app.library.playlists_selected = 0;
                                }
                                KeyCode::Backspace if app.library.playlists_filter.editing => {
                                    app.library.playlists_filter.backspace_at_cursor();
                                    app.library.playlists_selected = 0;
                                }
                                KeyCode::Enter if app.library.playlists_filter.editing => {
                                    app.library.playlists_filter.editing = false;
                                }
                                KeyCode::Esc if app.library.playlists_filter.editing => {
                                    app.library.playlists_filter.cancel_editing();
                                    app.library.playlists_selected = 0;
                                }
                                KeyCode::Left if app.library.playlists_filter.editing => {
                                    app.library.playlists_filter.cursor_left();
                                }
                                KeyCode::Right if app.library.playlists_filter.editing => {
                                    app.library.playlists_filter.cursor_right();
                                }
                                KeyCode::Char('q') => break 'inner LoopExit::Quit,
                                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                    break 'inner LoopExit::Quit
                                }
                                KeyCode::Char('?') => {
                                    app.nav.push(Screen::Help);
                                }
                                KeyCode::Char(' ') => {
                                    let _ = spirc.play_pause();
                                }
                                KeyCode::Char('n') => {
                                    let _ = spirc.next();
                                }
                                KeyCode::Char('p') => {
                                    let _ = spirc.prev();
                                }
                                KeyCode::Char('+') => {
                                    let _ = spirc.volume_up();
                                }
                                KeyCode::Char('-') => {
                                    let _ = spirc.volume_down();
                                }
                                KeyCode::Char('l') => {
                                    app.nav.goto(Screen::Library);
                                }
                                KeyCode::Esc if !app.library.playlists_filter.query.is_empty() => {
                                    app.library.playlists_filter.query.clear();
                                    app.library.playlists_filter.cursor = 0;
                                    app.library.playlists_selected = 0;
                                }
                                KeyCode::Esc | KeyCode::Left => {
                                    app.nav.escape();
                                }
                                KeyCode::Char('/') => {
                                    app.library.playlists_filter.start_editing();
                                }
                                KeyCode::Char('o') => {
                                    app.library.playlists_filter.sort_alpha =
                                        !app.library.playlists_filter.sort_alpha;
                                }
                                KeyCode::Char('c') => {
                                    app.text_prompt =
                                        Some(TextPrompt::new("New playlist name", "", TextPromptAction::CreatePlaylist));
                                }
                                KeyCode::Char('r') => {
                                    if let Fetch::Ready(items) = &app.library.playlists {
                                        let display = pinned_first(
                                            filtered_sorted(items, &app.library.playlists_filter, &label),
                                            &app.pinned_playlists,
                                            |p| p.uri.as_str(),
                                        );
                                        if let Some(playlist) =
                                            display.get(app.library.playlists_selected).map(|&(_, p)| p.clone())
                                        {
                                            app.text_prompt = Some(TextPrompt::new(
                                                "Rename playlist",
                                                playlist.name.clone(),
                                                TextPromptAction::RenamePlaylist(playlist),
                                            ));
                                        }
                                    }
                                }
                                // Always confirms first, no exceptions -- deleting a playlist
                                // is hard to reverse.
                                KeyCode::Char('d') => {
                                    if let Fetch::Ready(items) = &app.library.playlists {
                                        let display = pinned_first(
                                            filtered_sorted(items, &app.library.playlists_filter, &label),
                                            &app.pinned_playlists,
                                            |p| p.uri.as_str(),
                                        );
                                        if let Some(playlist) =
                                            display.get(app.library.playlists_selected).map(|&(_, p)| p.clone())
                                        {
                                            app.pending_confirm = Some(PendingConfirm {
                                                message: format!("Delete playlist \"{}\"? y/n", playlist.name),
                                                action: ConfirmAction::DeletePlaylist(playlist),
                                            });
                                        }
                                    }
                                }
                                KeyCode::Char(c) if is_pin_key(KeyCode::Char(c), key.modifiers) => {
                                    if let Fetch::Ready(items) = &app.library.playlists {
                                        let display = pinned_first(
                                            filtered_sorted(items, &app.library.playlists_filter, &label),
                                            &app.pinned_playlists,
                                            |p| p.uri.as_str(),
                                        );
                                        if let Some(uri) =
                                            display.get(app.library.playlists_selected).map(|&(_, p)| p.uri.clone())
                                        {
                                            pins::toggle_in_place(&mut app.pinned_playlists, &uri);
                                            pins::save("playlists", &app.pinned_playlists);
                                        }
                                    }
                                }
                                KeyCode::Up => {
                                    app.library.playlists_selected =
                                        app.library.playlists_selected.saturating_sub(1);
                                }
                                KeyCode::Down => {
                                    if let Fetch::Ready(items) = &app.library.playlists {
                                        let display = pinned_first(
                                            filtered_sorted(items, &app.library.playlists_filter, &label),
                                            &app.pinned_playlists,
                                            |p| p.uri.as_str(),
                                        );
                                        if !display.is_empty() {
                                            app.library.playlists_selected =
                                                (app.library.playlists_selected + 1).min(display.len() - 1);
                                        }
                                    }
                                }
                                KeyCode::Enter | KeyCode::Right => {
                                    let playlist = match &app.library.playlists {
                                        Fetch::Ready(items) => {
                                            let display = pinned_first(
                                                filtered_sorted(items, &app.library.playlists_filter, &label),
                                                &app.pinned_playlists,
                                                |p| p.uri.as_str(),
                                            );
                                            display
                                                .get(app.library.playlists_selected)
                                                .map(|&(_, p)| p.clone())
                                        }
                                        _ => None,
                                    };
                                    if let Some(playlist) = playlist {
                                        open_playlist_detail(&mut app, &library_tx, &spotify_client, playlist);
                                    }
                                }
                                _ => {}
                            }
                        }
                        Screen::PlaylistDetail => {
                            let label = |t: &TrackResult| format!("{} \u{2014} {}", t.artist, t.title);
                            let filter_editing =
                                app.playlist_detail.as_ref().is_some_and(|pd| pd.filter.editing);
                            let move_mode_active =
                                app.playlist_detail.as_ref().is_some_and(|pd| pd.move_mode.is_some());
                            match key.code {
                                KeyCode::Char(c) if filter_editing => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.filter.insert_at_cursor(c);
                                        pd.selected = 0;
                                    }
                                }
                                KeyCode::Backspace if filter_editing => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.filter.backspace_at_cursor();
                                        pd.selected = 0;
                                    }
                                }
                                KeyCode::Enter if filter_editing => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.filter.editing = false;
                                    }
                                }
                                KeyCode::Esc if filter_editing => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.filter.cancel_editing();
                                        pd.selected = 0;
                                    }
                                }
                                KeyCode::Left if filter_editing => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.filter.cursor_left();
                                    }
                                }
                                KeyCode::Right if filter_editing => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.filter.cursor_right();
                                    }
                                }
                                // Move-mode: `Up`/`Down` relocate the track locally (no
                                // network call per keystroke), `Enter` confirms with a
                                // single `reorder_track` call reflecting the net
                                // displacement, `Esc` walks it back to where it started.
                                // Every other key is swallowed while active -- same
                                // reasoning as `filter_editing`'s own catch-all: editing
                                // playlist content mid-reorder (remove/add/rename/pin)
                                // would race the pending local move.
                                KeyCode::Up if move_mode_active => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        if let Fetch::Ready(items) = &mut pd.tracks {
                                            pd.selected = ui::move_item_up(items, pd.selected);
                                        }
                                    }
                                }
                                KeyCode::Down if move_mode_active => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        if let Fetch::Ready(items) = &mut pd.tracks {
                                            pd.selected = ui::move_item_down(items, pd.selected);
                                        }
                                    }
                                }
                                KeyCode::Enter if move_mode_active => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        if let Some(start) = pd.move_mode.take() {
                                            let end = pd.selected;
                                            if start != end {
                                                if let Some(client) = spotify_client.clone() {
                                                    app.status = Some(("reordering\u{2026}".to_string(), false));
                                                    let tx = crud_tx.clone();
                                                    let playlist_uri = pd.playlist.uri.clone();
                                                    let playlist_uri_for_result = playlist_uri.clone();
                                                    tokio::spawn(async move {
                                                        let result = api::playlists::reorder_track(
                                                            &client,
                                                            &playlist_uri,
                                                            start,
                                                            end,
                                                        )
                                                        .await;
                                                        let _ = tx.send(CrudResult::TrackReordered {
                                                            playlist_uri: playlist_uri_for_result,
                                                            result,
                                                        });
                                                    });
                                                } else {
                                                    app.status =
                                                        Some(("Spotify client not ready yet".to_string(), true));
                                                }
                                            }
                                        }
                                    }
                                    resume_normal_display_index(&mut app);
                                }
                                KeyCode::Esc if move_mode_active => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        if let Some(start) = pd.move_mode.take() {
                                            if let Fetch::Ready(items) = &mut pd.tracks {
                                                pd.selected = ui::move_item_to(items, pd.selected, start);
                                            }
                                        }
                                    }
                                    resume_normal_display_index(&mut app);
                                }
                                KeyCode::Char(_) if move_mode_active => {}
                                KeyCode::Char('m') => {
                                    // Pins don't block this -- move-mode simply stops
                                    // applying the pinned-first bubbling to the display
                                    // while active (see render_playlist_detail), so display
                                    // position always equals real array position
                                    // regardless of what's pinned. Filter/sort still have
                                    // to be off for the same reason: either one changes
                                    // display order in a way this doesn't (yet) undo.
                                    let ineligible_reason = match &app.playlist_detail {
                                        Some(pd) if pd.filter.editing || !pd.filter.query.is_empty() || pd.filter.sort_alpha => {
                                            Some("clear the filter and turn off sort to reorder")
                                        }
                                        Some(pd) => match &pd.tracks {
                                            Fetch::Ready(_) => None,
                                            _ => Some("still loading"),
                                        },
                                        None => Some("no playlist open"),
                                    };
                                    match ineligible_reason {
                                        None => {
                                            // `pd.selected` right now is a position in the
                                            // normal pinned-first DISPLAY, not necessarily
                                            // the real array -- has to be converted before
                                            // move-mode starts treating it as a raw index,
                                            // or entering move-mode with a pin active would
                                            // silently start moving the wrong track.
                                            let real_index = if let Some(pd) = &app.playlist_detail {
                                                if let Fetch::Ready(items) = &pd.tracks {
                                                    let display = pinned_first(
                                                        filtered_sorted(items, &pd.filter, &label),
                                                        &app.pinned_tracks,
                                                        |t| t.uri.as_str(),
                                                    );
                                                    display.get(pd.selected).map(|&(i, _)| i)
                                                } else {
                                                    None
                                                }
                                            } else {
                                                None
                                            };
                                            if let Some(real_index) = real_index {
                                                if let Some(pd) = &mut app.playlist_detail {
                                                    pd.selected = real_index;
                                                    pd.move_mode = Some(real_index);
                                                }
                                            }
                                        }
                                        Some(reason) => {
                                            app.status = Some((reason.to_string(), true));
                                        }
                                    }
                                }
                                KeyCode::Char('q') => break 'inner LoopExit::Quit,
                                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                    break 'inner LoopExit::Quit
                                }
                                KeyCode::Char('?') => {
                                    app.nav.push(Screen::Help);
                                }
                                KeyCode::Char(' ') => {
                                    let _ = spirc.play_pause();
                                }
                                KeyCode::Char('n') => {
                                    let _ = spirc.next();
                                }
                                KeyCode::Char('p') => {
                                    let _ = spirc.prev();
                                }
                                KeyCode::Char('+') => {
                                    let _ = spirc.volume_up();
                                }
                                KeyCode::Char('-') => {
                                    let _ = spirc.volume_down();
                                }
                                KeyCode::Char('l') => {
                                    app.nav.goto(Screen::Library);
                                }
                                // A different playlist than the one open here -- `r` already
                                // owns renaming *this* one.
                                KeyCode::Char('c') => {
                                    app.text_prompt = Some(TextPrompt::new(
                                        "New playlist name",
                                        "",
                                        TextPromptAction::CreatePlaylist,
                                    ));
                                }
                                KeyCode::Esc
                                    if app
                                        .playlist_detail
                                        .as_ref()
                                        .is_some_and(|pd| !pd.filter.query.is_empty()) =>
                                {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.filter.query.clear();
                                        pd.filter.cursor = 0;
                                        pd.selected = 0;
                                    }
                                }
                                KeyCode::Esc | KeyCode::Left => {
                                    app.nav.escape();
                                }
                                KeyCode::Char('/') => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.filter.start_editing();
                                    }
                                }
                                KeyCode::Char('o') => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.filter.sort_alpha = !pd.filter.sort_alpha;
                                    }
                                }
                                // Renames the open playlist itself, not the selected track.
                                KeyCode::Char('r') => {
                                    if let Some(pd) = &app.playlist_detail {
                                        app.text_prompt = Some(TextPrompt::new(
                                            "Rename playlist",
                                            pd.playlist.name.clone(),
                                            TextPromptAction::RenamePlaylist(pd.playlist.clone()),
                                        ));
                                    }
                                }
                                // Always confirms first, no exceptions -- removing a track
                                // is hard to reverse.
                                KeyCode::Char('d') => {
                                    if let Some(pd) = &app.playlist_detail {
                                        if let Fetch::Ready(items) = &pd.tracks {
                                            let display = pinned_first(
                                                filtered_sorted(items, &pd.filter, &label),
                                                &app.pinned_tracks,
                                                |t| t.uri.as_str(),
                                            );
                                            if let Some((_, track)) = display.get(pd.selected) {
                                                let track = (*track).clone();
                                                let playlist_uri = pd.playlist.uri.clone();
                                                app.pending_confirm = Some(PendingConfirm {
                                                    message: format!(
                                                        "Remove \"{} \u{2014} {}\" from this playlist? y/n",
                                                        track.artist, track.title
                                                    ),
                                                    action: ConfirmAction::RemoveTrack {
                                                        playlist_uri,
                                                        track_uri: track.uri,
                                                    },
                                                });
                                            }
                                        }
                                    }
                                }
                                // Shift+D deletes the *open playlist itself* -- plain `d`
                                // already means "remove the selected track" here, so
                                // deleting the playlist needs a different key. Reported
                                // live: pressing `d` inside a playlist only ever removed a
                                // track, with no way to delete the playlist from within it.
                                KeyCode::Char(c) if is_shift_char(KeyCode::Char(c), key.modifiers, 'D', 'd') => {
                                    if let Some(pd) = &app.playlist_detail {
                                        app.pending_confirm = Some(PendingConfirm {
                                            message: format!("Delete playlist \"{}\"? y/n", pd.playlist.name),
                                            action: ConfirmAction::DeletePlaylist(pd.playlist.clone()),
                                        });
                                    }
                                }
                                KeyCode::Char('a') => {
                                    if let Some(pd) = &app.playlist_detail {
                                        if let Fetch::Ready(items) = &pd.tracks {
                                            let display = pinned_first(
                                                filtered_sorted(items, &pd.filter, &label),
                                                &app.pinned_tracks,
                                                |t| t.uri.as_str(),
                                            );
                                            if let Some((_, track)) = display.get(pd.selected) {
                                                app.playlist_picker =
                                                    Some(PlaylistPicker { track_uri: track.uri.clone(), selected: 0 });
                                            }
                                        }
                                    }
                                }
                                KeyCode::Char(c) if is_pin_key(KeyCode::Char(c), key.modifiers) => {
                                    if let Some(pd) = &app.playlist_detail {
                                        if let Fetch::Ready(items) = &pd.tracks {
                                            let display = pinned_first(
                                                filtered_sorted(items, &pd.filter, &label),
                                                &app.pinned_tracks,
                                                |t| t.uri.as_str(),
                                            );
                                            if let Some(uri) =
                                                display.get(pd.selected).map(|&(_, t)| t.uri.clone())
                                            {
                                                pins::toggle_in_place(&mut app.pinned_tracks, &uri);
                                                pins::save("tracks", &app.pinned_tracks);
                                            }
                                        }
                                    }
                                }
                                KeyCode::Up => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        pd.selected = pd.selected.saturating_sub(1);
                                    }
                                }
                                KeyCode::Down => {
                                    if let Some(pd) = &mut app.playlist_detail {
                                        if let Fetch::Ready(items) = &pd.tracks {
                                            let display = pinned_first(
                                                filtered_sorted(items, &pd.filter, &label),
                                                &app.pinned_tracks,
                                                |t| t.uri.as_str(),
                                            );
                                            if !display.is_empty() {
                                                pd.selected = (pd.selected + 1).min(display.len() - 1);
                                            }
                                        }
                                    }
                                }
                                // Not Right -- Enter here can play a track and jump to Now Playing,
                                // a real "leave here" side effect. Right means "go deeper," not
                                // "commit and teleport" (reported live as feeling unnatural).
                                KeyCode::Enter => {
                                    if let Some(pd) = &app.playlist_detail {
                                        if let Fetch::Ready(items) = &pd.tracks {
                                            let display = pinned_first(
                                                filtered_sorted(items, &pd.filter, &label),
                                                &app.pinned_tracks,
                                                |t| t.uri.as_str(),
                                            );
                                            if let Some(&(original_index, _)) = display.get(pd.selected) {
                                                // Loads the whole playlist as
                                                // context starting at the
                                                // selected track, not just
                                                // that one track in isolation
                                                // -- n/p then walk the real
                                                // playlist, matching "play
                                                // playlist as context" in the
                                                // plan. original_index (not
                                                // pd.selected) since the real
                                                // context is indexed against
                                                // the unfiltered playlist --
                                                // filtered_sorted carries the
                                                // original position precisely
                                                // so this stays correct even
                                                // with a filter/sort active.
                                                let opts = LoadRequestOptions {
                                                    playing_track: Some(PlayingTrack::Index(original_index as u32)),
                                                    ..Default::default()
                                                };
                                                let _ = spirc.activate();
                                                let _ = spirc.load(LoadRequest::from_context_uri(
                                                    pd.playlist.uri.clone(),
                                                    opts,
                                                ));
                                                let _ = spirc.play();
                                                app.nav.goto(Screen::NowPlaying);
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        Screen::Help => match key.code {
                            KeyCode::Char('q') => break 'inner LoopExit::Quit,
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                break 'inner LoopExit::Quit
                            }
                            KeyCode::Esc | KeyCode::Left => {
                                app.nav.escape();
                            }
                            KeyCode::Char(' ') => {
                                let _ = spirc.play_pause();
                            }
                            KeyCode::Char('n') => {
                                let _ = spirc.next();
                            }
                            KeyCode::Char('p') => {
                                let _ = spirc.prev();
                            }
                            KeyCode::Char('+') => {
                                let _ = spirc.volume_up();
                            }
                            KeyCode::Char('-') => {
                                let _ = spirc.volume_down();
                            }
                            KeyCode::Char('l') => {
                                app.nav.goto(Screen::Library);
                            }
                            KeyCode::Char('c') => {
                                app.text_prompt =
                                    Some(TextPrompt::new("New playlist name", "", TextPromptAction::CreatePlaylist));
                            }
                            _ => {}
                        },
                    }
                }
            }
        }
    };

        let _ = spirc.shutdown();

        match exit {
            LoopExit::Quit => break 'outer,
            LoopExit::Disconnected => {
                app.track_title = None;
                app.track_artist = None;
                app.track_album = None;
                app.playing = None;
                app.lyrics = LyricsState::SessionEnded;
                terminal.draw(|f| ui::render(f, &app, &mut scroll))?;
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }

    Ok(())
}
