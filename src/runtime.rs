//! The main loop: connects the player (retrying with backoff when the
//! session drops), then each frame drains player events and background
//! results, draws, and handles at most one terminal event.

use crate::config::{self, Config};
use crate::covers::{fetch_cover, prefetch_next_track};
use crate::input::{self, KeyCtx};
use crate::lyrics::pipeline::{LyricsPipeline, TrackMeta};
use crate::lyrics::romanizer::Romanizer;
use crate::lyrics::spicy::SpicyClient;
use crate::lyrics::{current_line_index, CachedLyrics, LyricLine};
use crate::player::{self, Connection, ConnectError};
use crate::position::PositionTracker;
use crate::services::{ServiceReceivers, Services};
use crate::state::{AppState, LyricsState, RepeatMode, Screen};
use crate::terminal::{detect_graphics_picker, install_panic_hook, TerminalGuard};
use crate::{api, pins, ui};
use crossterm::event::{self, Event};
use image::DynamicImage;
use librespot_metadata::audio::UniqueFields;
use librespot_playback::player::PlayerEvent;
use ratatui::backend::CrosstermBackend;
use rspotify::AuthCodeSpotify;
use std::io::stdout;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

type Terminal = ratatui::Terminal<CrosstermBackend<std::io::Stdout>>;

/// Redraw interval.
const TICK: Duration = Duration::from_millis(100);
/// Redraw interval while a word-by-word lyric sweep is moving (20 fps); at
/// the normal 10 fps the sweep would visibly step.
const WORD_TICK: Duration = Duration::from_millis(50);
/// How long after a track change to wait before fetching its lyrics, so
/// skipping through several tracks quickly fetches only the last.
const LYRICS_DEBOUNCE: Duration = Duration::from_millis(250);
/// The queue changes on its own (a track ends, another device skips), so it
/// is refetched this often while it is on screen.
const QUEUE_POLL_INTERVAL: Duration = Duration::from_secs(5);

const RECONNECT_INITIAL_BACKOFF: Duration = Duration::from_millis(500);
const RECONNECT_MAX_BACKOFF: Duration = Duration::from_secs(10);

/// Doubles the wait between connect attempts, capped at `RECONNECT_MAX_BACKOFF`.
fn next_backoff(current: Duration) -> Duration {
    (current * 2).min(RECONNECT_MAX_BACKOFF)
}

// Startup: the splash stays up for at least `STARTUP_MIN_VISIBLE`, which
// gives Ghostty's kitty-graphics subsystem time to initialise before the
// first frame with real album art (drawn too early, the first cover renders
// blank). After the first connect it then waits for the resumed track to
// settle: librespot's resume handshake can emit several `TrackChanged`s in a
// row (a resumed track, a failed autoplay resolve, then the real one), and
// waiting until they stop arriving skips past the intermediate ones.
const STARTUP_MIN_VISIBLE: Duration = Duration::from_millis(600);
const STARTUP_TICK: Duration = Duration::from_millis(80);
const STARTUP_TRACK_SETTLE: Duration = Duration::from_millis(400);
/// Escape hatch for when nothing ever settles (nothing playing at all).
const STARTUP_TRACK_MAX_WAIT: Duration = Duration::from_secs(4);

enum LoopExit {
    Quit,
    Disconnected,
}

/// The pending, debounced lyrics request for the track that just started.
struct PendingLyrics {
    generation: u64,
    meta: TrackMeta,
    due: Instant,
}

/// Channels for album art: the current track's, tagged with its generation,
/// and the prefetched next track's, tagged with its URI.
struct CoverChannels {
    tx: Sender<(u64, DynamicImage)>,
    rx: Receiver<(u64, DynamicImage)>,
    prefetch_tx: Sender<(String, DynamicImage)>,
    prefetch_rx: Receiver<(String, DynamicImage)>,
}

struct Runtime {
    terminal: Terminal,
    cfg: Config,
    app: AppState,
    images: ui::ImageState,
    scroll: ui::ScrollState,
    tracker: PositionTracker,

    svc: Services,
    service_rx: ServiceReceivers,
    client_rx: Receiver<Option<AuthCodeSpotify>>,
    client_checked: bool,

    spicy: Option<SpicyClient>,
    lyrics: LyricsPipeline,
    lyrics_rx: Receiver<(u64, CachedLyrics)>,
    romanizer: Romanizer,
    covers: CoverChannels,

    /// Incremented on every track change and attached to every async result
    /// about the current track (lyrics, cover, romanization), so an answer for
    /// a track already skipped past is dropped rather than painted over the
    /// current one.
    generation: u64,
    pending_lyrics: Option<PendingLyrics>,
    /// The current track's synced lyric lines, for finding the current line.
    synced_lines: Vec<LyricLine>,
    queue_last_fetched: Option<Instant>,
}

pub async fn run() -> std::io::Result<()> {
    install_panic_hook();

    // The Web API client loads in the background: a failure here disables
    // search and library browsing only, never playback or lyrics.
    let (client_tx, client_rx) = mpsc::channel::<Option<AuthCodeSpotify>>();
    tokio::spawn(async move {
        let client = match api::load_or_refresh_token().await {
            Ok(token) => Some(api::client_from_token(token).await),
            Err(e) => {
                log::warn!("Web API unavailable: failed to load/refresh Spotify token: {e}");
                None
            }
        };
        let _ = client_tx.send(client);
    });

    let _guard = TerminalGuard::new()?;
    let terminal = ratatui::Terminal::new(CrosstermBackend::new(stdout()))?;
    let picker = detect_graphics_picker();

    let cfg = config::load();
    let spicy = cfg.spicy_lyrics_key().map(SpicyClient::new);
    log::info!(
        "spicy_lyrics: {}",
        if spicy.is_some() { "key configured, used first" } else { "no key configured, skipped" }
    );
    let (lyrics, lyrics_rx) = LyricsPipeline::new(spicy.clone());
    let (svc, service_rx) = Services::new();
    let (cover_tx, cover_rx) = mpsc::channel();
    let (prefetch_tx, prefetch_rx) = mpsc::channel();

    let mut rt = Runtime {
        terminal,
        app: AppState::new(cfg.romanize_lyrics, pins::load("playlists"), pins::load("tracks"), player::INITIAL_VOLUME),
        cfg,
        images: ui::ImageState { picker, ..Default::default() },
        scroll: ui::ScrollState::default(),
        tracker: PositionTracker::new(),
        svc,
        service_rx,
        client_rx,
        client_checked: false,
        spicy,
        lyrics,
        lyrics_rx,
        romanizer: Romanizer::new(),
        covers: CoverChannels { tx: cover_tx, rx: cover_rx, prefetch_tx, prefetch_rx },
        generation: 0,
        pending_lyrics: None,
        synced_lines: Vec::new(),
        queue_last_fetched: None,
    };
    rt.run().await
}

impl Runtime {
    /// Connect, run until the session drops or the user quits, reconnect
    /// with capped exponential backoff.
    async fn run(&mut self) -> std::io::Result<()> {
        let mut backoff = RECONNECT_INITIAL_BACKOFF;
        // Only the process's first connect attempt gets the splash, and only
        // its first successful connect waits for the track to settle;
        // reconnects stay fast.
        let mut first_attempt = true;
        let mut awaiting_first_track = true;
        loop {
            let connected = if std::mem::take(&mut first_attempt) {
                self.connect_with_splash().await?
            } else {
                player::connect().await
            };
            let mut conn = match connected {
                Ok(conn) => conn,
                Err(e) => {
                    log::warn!("connect failed, retrying in {backoff:?}: {e}");
                    self.show_disconnected(e == ConnectError::NoCredentials)?;
                    tokio::time::sleep(backoff).await;
                    backoff = next_backoff(backoff);
                    continue;
                }
            };
            backoff = RECONNECT_INITIAL_BACKOFF;

            // Reclaim whatever was last active ("continue where you left
            // off"). `transfer` with this device on both ends is a no-op if
            // it is already active, so it is safe on every (re)connect.
            match conn.spirc.transfer(None) {
                Ok(()) => log::info!("spirc.transfer(None) sent, reclaiming the last active session"),
                Err(e) => log::warn!("spirc.transfer(None) failed: {e}"),
            }
            // A fresh session must not keep showing the old one's state.
            self.app.lyrics = LyricsState::Idle;
            self.tracker = PositionTracker::new();

            if std::mem::take(&mut awaiting_first_track) {
                self.wait_for_first_track(&mut conn).await?;
            }

            let exit = self.run_session(&mut conn)?;
            let _ = conn.spirc.shutdown();
            match exit {
                LoopExit::Quit => return Ok(()),
                LoopExit::Disconnected => {
                    self.show_disconnected(false)?;
                    tokio::time::sleep(backoff).await;
                    backoff = next_backoff(backoff);
                }
            }
        }
    }

    /// Animates the splash while the first connect resolves (several real
    /// seconds: AP resolution, auth, first track load).
    async fn connect_with_splash(&mut self) -> std::io::Result<Result<Connection, ConnectError>> {
        let start = Instant::now();
        let mut handle = tokio::spawn(player::connect());
        let mut tick: usize = 0;
        let result = loop {
            tokio::select! {
                res = &mut handle => break res.expect("connect task panicked"),
                _ = tokio::time::sleep(STARTUP_TICK) => {
                    tick = tick.wrapping_add(1);
                    self.terminal.draw(|f| ui::render_startup(f, tick))?;
                }
            }
        };
        while start.elapsed() < STARTUP_MIN_VISIBLE {
            tick = tick.wrapping_add(1);
            self.terminal.draw(|f| ui::render_startup(f, tick))?;
            tokio::time::sleep(STARTUP_TICK).await;
        }
        Ok(result)
    }

    /// Keeps the splash up, applying player events exactly as the main loop
    /// would, until no further `TrackChanged` has arrived for
    /// `STARTUP_TRACK_SETTLE`. Otherwise Now Playing would briefly show
    /// "nothing playing", then flash an intermediate track before the real one.
    async fn wait_for_first_track(&mut self, conn: &mut Connection) -> std::io::Result<()> {
        let wait_start = Instant::now();
        let mut last_track_change: Option<Instant> = None;
        let mut tick: usize = 0;
        loop {
            if conn.task.is_finished() {
                break;
            }
            if self.drain_player_events(conn) {
                last_track_change = Some(Instant::now());
            }
            let settled = last_track_change.is_some_and(|t| t.elapsed() >= STARTUP_TRACK_SETTLE);
            if settled || wait_start.elapsed() >= STARTUP_TRACK_MAX_WAIT {
                break;
            }
            tick = tick.wrapping_add(1);
            self.terminal.draw(|f| ui::render_startup(f, tick))?;
            tokio::time::sleep(STARTUP_TICK).await;
        }
        Ok(())
    }

    fn show_disconnected(&mut self, no_login: bool) -> std::io::Result<()> {
        self.app.track_title = None;
        self.app.track_artist = None;
        self.app.track_album = None;
        self.app.playing = None;
        self.app.lyrics = if no_login { LyricsState::NoLogin } else { LyricsState::SessionEnded };
        self.draw()
    }

    fn draw(&mut self) -> std::io::Result<()> {
        let Self { terminal, app, scroll, images, .. } = self;
        terminal.draw(|f| ui::render(f, app, scroll, images))?;
        Ok(())
    }

    /// One connected session: every frame, background work, draw, then at
    /// most one terminal event.
    fn run_session(&mut self, conn: &mut Connection) -> std::io::Result<LoopExit> {
        loop {
            if conn.task.is_finished() {
                log::warn!("Spirc task ended, the Connect session dropped; reconnecting");
                return Ok(LoopExit::Disconnected);
            }

            self.drain_player_events(conn);
            self.start_due_lyrics_fetch(conn);
            self.poll_client_ready();
            self.poll_queue();
            self.drain_results();
            self.update_playback_fields();
            self.draw()?;

            let tick = if ui::word_sweep_active(&self.app.lyrics, self.app.current_line, self.app.playing) {
                WORD_TICK
            } else {
                TICK
            };
            if !event::poll(tick)? {
                continue;
            }
            match event::read()? {
                // Switching tmux windows away and back drops a placed
                // kitty-graphics image, and tmux gives the app no other
                // signal. Clearing the cache forces the next render to
                // re-encode and retransmit it.
                Event::FocusGained => self.images.sized_covers.clear(),
                Event::Key(key) => {
                    let mut ctx = KeyCtx {
                        app: &mut self.app,
                        svc: &self.svc,
                        spirc: &conn.spirc,
                        tracker: &self.tracker,
                        scroll: &mut self.scroll,
                        romanizer: &mut self.romanizer,
                        generation: self.generation,
                        confirm_quit: self.cfg.confirm_quit,
                        quit: false,
                    };
                    input::handle_key(&mut ctx, key);
                    if ctx.quit {
                        return Ok(LoopExit::Quit);
                    }
                }
                _ => {}
            }
        }
    }

    /// Applies every queued `PlayerEvent`. Returns whether a `TrackChanged`
    /// was among them.
    fn drain_player_events(&mut self, conn: &mut Connection) -> bool {
        let mut track_changed = false;
        while let Ok(event) = conn.events.try_recv() {
            match &event {
                PlayerEvent::VolumeChanged { volume } => self.app.volume = *volume,
                PlayerEvent::ShuffleChanged { shuffle } => self.app.shuffle = *shuffle,
                PlayerEvent::RepeatChanged { context, track } => {
                    self.app.repeat = RepeatMode::from_flags(*context, *track)
                }
                PlayerEvent::TrackChanged { audio_item } => {
                    self.on_track_changed(conn, audio_item);
                    track_changed = true;
                }
                _ => {}
            }
            self.tracker.on_event(&event, Instant::now());
        }
        // No player event carries smart shuffle, so read it from librespot's
        // connect state on every pass. Shuffle off always wins.
        self.app.smart_shuffle = self.app.shuffle && librespot_connect::smart_shuffle_active();
        track_changed
    }

    fn on_track_changed(&mut self, conn: &Connection, item: &librespot_metadata::audio::AudioItem) {
        let (artist, album) = match &item.unique_fields {
            UniqueFields::Track { artists, album, .. } => {
                (artists.0.first().map(|a| a.name.clone()).unwrap_or_default(), Some(album.clone()))
            }
            _ => (String::new(), None),
        };
        let uri = item.track_id.to_string();

        let app = &mut self.app;
        app.track_title = Some(item.name.clone());
        app.track_artist = (!artist.is_empty()).then(|| artist.clone());
        app.track_album = album.clone();
        app.current_track_uri = Some(uri.clone());
        app.duration = Duration::from_millis(item.duration_ms as u64);
        app.lyrics = LyricsState::Loading;
        app.lyrics_credit = None;
        app.romanized_lines = None;
        self.generation += 1;

        // Use the prefetched cover if it is for exactly this track; `take`
        // clears the slot either way so a stale one never lingers.
        let already_warm = match self.images.prewarmed_cover.take() {
            Some((prefetched_uri, image)) if prefetched_uri == uri => {
                self.images.cover_image = Some((prefetched_uri, image));
                self.images.sized_covers.clear();
                true
            }
            _ => false,
        };
        // Covers only matter when a real graphics protocol is in use.
        if !already_warm
            && self.images.picker.is_some()
            && let Some(url) = item.covers.first().map(|c| c.url.clone())
        {
            let (tx, generation) = (self.covers.tx.clone(), self.generation);
            tokio::task::spawn_blocking(move || {
                if let Some(image) = fetch_cover(&url) {
                    let _ = tx.send((generation, image));
                }
            });
        }

        tokio::spawn(prefetch_next_track(
            conn.spirc.clone(),
            self.svc.client.clone(),
            self.spicy.clone(),
            self.covers.prefetch_tx.clone(),
        ));

        self.pending_lyrics = Some(PendingLyrics {
            generation: self.generation,
            meta: TrackMeta {
                track_id: uri,
                artist,
                title: item.name.clone(),
                album,
                duration_ms: item.duration_ms,
            },
            due: Instant::now() + LYRICS_DEBOUNCE,
        });
    }

    fn start_due_lyrics_fetch(&mut self, conn: &Connection) {
        if self.pending_lyrics.as_ref().is_some_and(|p| Instant::now() >= p.due)
            && let Some(pending) = self.pending_lyrics.take()
        {
            self.lyrics.request(&conn.session, pending.generation, pending.meta);
        }
    }

    /// Once the Web API client has loaded, fetches what the sidebar shows so
    /// it is not empty until the first Library visit.
    fn poll_client_ready(&mut self) {
        if self.client_checked {
            return;
        }
        let Ok(client) = self.client_rx.try_recv() else { return };
        self.client_checked = true;
        self.app.search.client_ready = client.is_some();
        self.svc.client = client;
        if self.svc.client.is_some() {
            self.svc.ensure_loaded(&mut self.app, Screen::YourPlaylists);
            self.svc.ensure_loaded(&mut self.app, Screen::LikedSongs);
        }
    }

    fn poll_queue(&mut self) {
        if *self.app.nav.top() != Screen::Queue {
            // Leaving the screen resets the timer, so returning fetches
            // immediately instead of waiting out the old interval.
            self.queue_last_fetched = None;
            return;
        }
        if self.queue_last_fetched.is_none_or(|t| t.elapsed() >= QUEUE_POLL_INTERVAL) && self.svc.client.is_some() {
            self.queue_last_fetched = Some(Instant::now());
            self.svc.refetch_queue();
        }
    }

    /// Applies every finished background job to the state.
    fn drain_results(&mut self) {
        while let Ok(result) = self.service_rx.search.try_recv() {
            let search = &mut self.app.search;
            search.searching = false;
            match result {
                Ok(results) => {
                    search.results = results;
                    search.selected = 0;
                    search.error = None;
                }
                Err(e) => search.error = Some(e),
            }
        }
        while let Ok(result) = self.service_rx.library.try_recv() {
            self.svc.apply_library_result(&mut self.app, result);
        }
        while let Ok(result) = self.service_rx.crud.try_recv() {
            self.svc.apply_crud_result(&mut self.app, result);
        }

        while let Ok((generation, cached)) = self.lyrics_rx.try_recv() {
            if generation != self.generation {
                continue;
            }
            self.app.lyrics_credit = match &cached {
                CachedLyrics::Synced { credit, .. } => credit.clone(),
                _ => None,
            };
            self.app.lyrics = LyricsState::from(cached);
            self.app.romanized_lines = None;
            if let LyricsState::Synced(lines) = &self.app.lyrics {
                self.synced_lines = lines.clone();
            }
            self.romanizer.request(&self.app, self.generation);
        }
        while let Ok((generation, lines)) = self.romanizer.rx.try_recv() {
            if generation == self.generation {
                self.app.romanized_lines = Some(lines);
            }
        }

        while let Ok((generation, image)) = self.covers.rx.try_recv() {
            if generation == self.generation
                && let Some(uri) = &self.app.current_track_uri
            {
                // Only the decoded image is stored; the protocol-encoded
                // version per render size is built lazily by the renderer.
                self.images.cover_image = Some((uri.clone(), image));
                self.images.sized_covers.clear();
            }
        }
        // The prefetched cover just waits here until its track starts (see
        // `on_track_changed`); a newer prefetch simply replaces it.
        while let Ok((uri, image)) = self.covers.prefetch_rx.try_recv() {
            self.images.prewarmed_cover = Some((uri, image));
        }
    }

    fn update_playback_fields(&mut self) {
        let now = Instant::now();
        self.app.position = Duration::from_millis(self.tracker.progress_ms(now) as u64);
        self.app.playing = self.tracker.current_track_id().map(|_| self.tracker.is_playing());
        self.app.current_line = if matches!(self.app.lyrics, LyricsState::Synced(_)) {
            current_line_index(&self.synced_lines, self.app.position)
        } else {
            None
        };
    }
}

#[cfg(test)]
mod backoff_tests {
    use super::*;

    #[test]
    fn the_wait_doubles_from_the_initial_backoff() {
        assert_eq!(next_backoff(RECONNECT_INITIAL_BACKOFF), Duration::from_secs(1));
        assert_eq!(next_backoff(Duration::from_secs(1)), Duration::from_secs(2));
    }

    #[test]
    fn the_wait_never_exceeds_the_cap() {
        assert_eq!(next_backoff(Duration::from_secs(6)), RECONNECT_MAX_BACKOFF);
        assert_eq!(next_backoff(RECONNECT_MAX_BACKOFF), RECONNECT_MAX_BACKOFF);
    }

    #[test]
    fn the_cap_is_reached_in_a_handful_of_attempts() {
        let mut wait = RECONNECT_INITIAL_BACKOFF;
        let attempts = (0..20).take_while(|_| {
            let done = wait == RECONNECT_MAX_BACKOFF;
            wait = next_backoff(wait);
            !done
        });
        assert_eq!(attempts.count(), 5);
    }
}
