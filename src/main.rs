mod config;
mod lyrics;
mod position;
mod api;
mod spike;
mod ui;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, ExecutableCommand};
use librespot_connect::{ConnectConfig, LoadRequest, Spirc};
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
use api::search::TrackResult;
use ui::{AppState, Focus, LyricsState, Nav, Screen, SearchState, SIDEBAR_ENTRIES};

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
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("librespot=debug"))
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

    // -- terminal UI setup (same pattern as ncspot-lyrics) --
    let _guard = TerminalGuard::new()?;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(stdout()))?;

    let (fetch_tx, fetch_rx) = spawn_fetch_thread();
    let cfg = config::load();
    let mut tracker: PositionTracker;
    let mut app = AppState {
        track_title: None,
        track_artist: None,
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
        search: SearchState::new(),
    };

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
                app.playing = None;
                app.lyrics = LyricsState::SessionEnded;
                terminal.draw(|f| ui::render(f, &app))?;
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue 'outer;
            }
        };
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

        terminal.draw(|f| ui::render(f, &app))?;

        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                // Tab is a distinct KeyCode, never a `Char(_)` -- safe to
                // intercept before anything else without ever eating a
                // literal keystroke a text field might want.
                if key.code == KeyCode::Tab {
                    app.nav.toggle_focus();
                } else if app.nav.focus == Focus::Sidebar {
                    match key.code {
                        KeyCode::Char('q') => break 'inner LoopExit::Quit,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break 'inner LoopExit::Quit
                        }
                        KeyCode::Char('f') if *app.nav.top() == Screen::NowPlaying => {
                            app.fullscreen = !app.fullscreen;
                            tmux_toggle_zoom();
                        }
                        KeyCode::Char('/') => {
                            app.nav.goto(Screen::Search);
                            app.nav.focus = Focus::Main;
                            app.search.query.clear();
                            app.search.results.clear();
                            app.search.error = None;
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
                        KeyCode::Left => {
                            let target =
                                (tracker.progress_ms(Instant::now()) as i64 - SEEK_STEP_MS).max(0);
                            let _ = spirc.set_position_ms(target as u32);
                        }
                        KeyCode::Right => {
                            let target = tracker.progress_ms(Instant::now()) as i64 + SEEK_STEP_MS;
                            let _ = spirc.set_position_ms(target as u32);
                        }
                        KeyCode::Up => {
                            app.sidebar_sel = app.sidebar_sel.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            app.sidebar_sel = (app.sidebar_sel + 1).min(SIDEBAR_ENTRIES.len() - 1);
                        }
                        KeyCode::Enter => {
                            let (_, screen) = SIDEBAR_ENTRIES[app.sidebar_sel];
                            app.nav.goto(screen);
                            app.nav.focus = Focus::Main;
                            if screen == Screen::Search {
                                app.search.query.clear();
                                app.search.results.clear();
                                app.search.error = None;
                            }
                        }
                        _ => {}
                    }
                } else {
                    // focus == Main
                    match *app.nav.top() {
                        Screen::Search => match key.code {
                            KeyCode::Esc => {
                                app.nav.escape();
                            }
                            KeyCode::Backspace => {
                                app.search.query.pop();
                                app.search.results.clear();
                                app.search.error = None;
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
                                app.search.query.push(c);
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
                            KeyCode::Char('f') => {
                                app.fullscreen = !app.fullscreen;
                                tmux_toggle_zoom();
                            }
                            KeyCode::Char('/') => {
                                app.nav.push(Screen::Search);
                                app.search.query.clear();
                                app.search.results.clear();
                                app.search.error = None;
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
                app.playing = None;
                app.lyrics = LyricsState::SessionEnded;
                terminal.draw(|f| ui::render(f, &app))?;
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }

    Ok(())
}
