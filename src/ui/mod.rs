//! ratatui rendering: sidebar + main pane + playbar layout, and the
//! fullscreen Now Playing layout.

mod art;
mod help;
mod lyrics_view;
mod now_playing;
mod overlays;
mod playbar;
mod screens;
mod theme;

use self::art::*;
use self::help::*;
use self::lyrics_view::*;
use self::now_playing::*;
use self::overlays::*;
use self::playbar::*;
use self::screens::*;
use self::theme::*;
pub use self::lyrics_view::word_sweep_active;

use crate::api::search::TrackResult;
use crate::state::*;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Padding, Paragraph, Wrap};
use ratatui::Frame;
use std::time::Duration;

/// Persisted scroll offsets, one per list, threaded through `render`
/// alongside `&AppState` rather than living inside it. Rebuilding a fresh
/// `ListState` every frame (offset always 0) was the original, buggy
/// approach: ratatui's own "keep selection visible" clamp then re-pins
/// the highlight to the bottom edge of the viewport on *every* render
/// once you've scrolled past one screenful, regardless of which
/// direction you're actually moving -- reported live as "stays at the
/// bottom even when scrolling up." Persisting `ListState.offset` across
/// frames lets ratatui's real algorithm work as designed: the viewport
/// only moves once the selection would leave it, not on every keystroke.
/// A separate top-level struct (not new fields nested inside `AppState`)
/// because rendering needs to mutate exactly one of these at a time while
/// reading many *other* fields of `AppState` immutably in the same call --
/// nesting them inside `AppState` itself would fight the borrow checker
/// over a mutable borrow of one field colliding with an immutable borrow
/// of its parent struct.
#[derive(Default)]
pub struct ScrollState {
    pub sidebar: ListState,
    pub search: ListState,
    pub liked_songs: ListState,
    pub saved_albums: ListState,
    pub followed_artists: ListState,
    pub playlists: ListState,
    pub playlist_detail: ListState,
    pub playlist_picker: ListState,
    pub quick_jump: ListState,
    pub queue: ListState,
    pub devices: ListState,
    pub artist_detail: ListState,
    pub album_detail: ListState,
    /// Help's scroll offset in rendered rows -- a plain `u16` for
    /// `Paragraph::scroll`, not a `ListState`: Help is a reference the
    /// user scans, not a list they navigate item by item (the mockup's
    /// own reasoning for giving it a two-column layout with no
    /// per-row selection at all). Clamped inside `render_help` against
    /// the real rendered height, so the key handler in `main.rs` can
    /// increment/decrement blindly without knowing the content size.
    pub help: u16,
}

/// Real album art via a terminal graphics protocol, threaded through
/// `render` the same way `ScrollState` is and for the same reason: it's
/// mutable render-side cache, not application state, and nesting it
/// inside `AppState` would fight the borrow checker the same way
/// `ScrollState`'s own doc comment already explains.
///
/// `picker` is populated once at startup (`main.rs`) if the terminal's
/// capability query (possibly overridden -- see that call site) reports
/// a real graphics protocol; `None` means "no real protocol available or
/// detection failed," in which case `render_art` always uses the hashed
/// placeholder and never touches the fields below at all.
///
/// `cover_image` holds the currently-playing track's *decoded* cover
/// (cheap to keep, no network/decode cost to reuse) plus the track uri
/// it belongs to, set once per track (`main.rs`, off the render path).
/// `sized_covers` is a small cache of already resize-encoded
/// `StatefulProtocol`s, one per distinct `(track uri, width, height)`
/// this app has actually rendered at -- built lazily in `render_art`,
/// not eagerly. This two-level design (decode once, encode once per
/// size) exists because a single shared `StatefulProtocol` re-encodes,
/// and on Kitty fully *re-transmits*, the whole image every time its
/// render `Rect`'s cell size changes (confirmed by reading
/// `ratatui-image`'s own Kitty protocol source) -- since the compact
/// hero and the fullscreen layouts use different art sizes by design,
/// a single shared protocol meant every `f` toggle forced a full
/// re-transmit, reported live as visible lag and display corruption
/// under rapid toggling. Caching one encoded protocol per size actually
/// seen means toggling between a stable, already-visited set of sizes
/// (the normal case) never re-triggers that cost after the first visit
/// to each size.
#[derive(Default)]
pub struct ImageState {
    pub picker: Option<ratatui_image::picker::Picker>,
    pub cover_image: Option<(String, image::DynamicImage)>,
    pub sized_covers: Vec<(String, u16, u16, ratatui_image::protocol::StatefulProtocol)>,
    /// One-shot, whole-process-lifetime retransmit: armed the first time
    /// this run builds any sized cover at all (in practice, the boot
    /// track's cover), fired once `STARTUP_RETRANSMIT_DELAY` later by
    /// clearing the entire cache so the very next render misses and
    /// rebuilds+retransmits fresh -- exactly what manually skipping a
    /// track and back already does to "fix" a blank cover, just
    /// automatic. A near-identical mechanism was tried once before this
    /// session at a 700ms delay and reverted: that gap was still short
    /// enough to land while Ghostty's own kitty image-compositing state
    /// from the *first* transmission was still settling, and a second
    /// full transmission landing in that window corrupted every
    /// subsequent cover for the rest of the session (the same trigger
    /// Phase 18 already found once, for a different cause). Reattempted
    /// here at a real multi-second delay specifically because that's
    /// the property that makes a *manual* skip-then-back safe -- by the
    /// time a human notices and acts, real seconds have passed, not
    /// milliseconds. The exact minimum safe gap isn't independently
    /// confirmed; this is a live experiment against real hardware, not
    /// a proven fix -- if blank art recurs, the delay needs widening
    /// further; if pixelation/corruption recurs instead, the gap is
    /// still too short and this needs reverting again.
    pub startup_retransmit_at: Option<std::time::Instant>,
    pub startup_retransmit_done: bool,
    /// The "soft load next track" prefetch's decoded cover, keyed by the
    /// uri it belongs to -- separate from `cover_image` (the *currently
    /// showing* slot) so a background prefetch can never clobber or race
    /// with what's on screen right now. `main.rs` moves this into
    /// `cover_image` once the real track-changed event actually arrives
    /// for the matching uri, instead of starting a fresh fetch.
    pub prewarmed_cover: Option<(String, image::DynamicImage)>,
}

/// How long to wait, after this process's very first cover-art
/// transmission, before automatically clearing the cache and
/// retransmitting once -- see `ImageState::startup_retransmit_at`'s own
/// doc comment for why this specific value is a judgment call, not a
/// derived or confirmed-safe number.
pub const STARTUP_RETRANSMIT_DELAY: std::time::Duration = std::time::Duration::from_millis(2000);

/// How many distinct `(track, size)` encoded protocols `ImageState`
/// keeps at once -- comfortably more than the handful of distinct art
/// sizes one session realistically produces (compact, fullscreen
/// two-pane, fullscreen narrow-stacked), so eviction is rare in
/// practice, not a tight budget being constantly hit.
pub const SIZED_COVER_CACHE_CAP: usize = 4;

pub const STARTUP_SPINNER: [char; 10] = ['\u{280B}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283C}', '\u{2834}', '\u{2826}', '\u{2827}', '\u{2807}', '\u{280F}'];

/// The very first thing drawn on a cold start, while `connect_spirc()` is
/// still resolving in the background -- previously this window was a
/// blank alternate-screen with zero feedback (the whole render loop was
/// blocked behind `connect_spirc().await`, which takes several real
/// seconds: AP resolution, auth, first track load). Reported live as a
/// separate, related bug: the very first album-art render after a cold
/// start would show completely blank (skipping to another track and back
/// fixed it), most likely a real terminal-side race -- Ghostty's own
/// kitty-graphics subsystem not yet ready for the first image placement
/// immediately after entering the alternate screen. Showing this
/// animation for a guaranteed minimum duration (`main.rs`'s
/// `STARTUP_MIN_VISIBLE`) turns an unexplained blank wait into a
/// deliberate, visible one, and gives that subsystem a real window to
/// finish initializing before the first real frame (with real album art)
/// ever gets drawn.
pub fn render_startup(frame: &mut Frame, tick: usize) {
    let area = frame.area();
    let spinner = STARTUP_SPINNER[tick % STARTUP_SPINNER.len()];
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new("spot-tui").alignment(Alignment::Center).style(Style::default().add_modifier(Modifier::BOLD).fg(ACCENT)),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(format!("{spinner} connecting to spotify\u{2026}"))
            .alignment(Alignment::Center)
            .style(Style::default().fg(DIM)),
        rows[2],
    );
}

pub fn render(frame: &mut Frame, app: &AppState, scroll: &mut ScrollState, images: &mut ImageState) {
    if app.fullscreen && *app.nav.top() == Screen::NowPlaying {
        render_fullscreen(frame, app, images);
        render_overlays(frame, app, &mut scroll.playlist_picker, &mut scroll.quick_jump);
        return;
    }

    let area = frame.area();
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(2), Constraint::Length(1)])
        .split(area);
    let (body_area, playbar_area, status_area) = (outer[0], outer[1], outer[2]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(22), Constraint::Min(1)])
        .split(body_area);
    let (sidebar_area, main_area) = (body[0], body[1]);

    render_sidebar(frame, app, &mut scroll.sidebar, sidebar_area);

    let main_block = Block::default()
        .borders(Borders::TOP)
        .border_style(focus_border_style(app.nav.focus == Focus::Main));
    let main_area = main_block.inner(main_area);
    frame.render_widget(main_block, body[1]);

    match app.nav.top() {
        Screen::Search => render_search(frame, app, &mut scroll.search, main_area),
        Screen::NowPlaying => render_compact(frame, app, images, main_area),
        Screen::Library => render_library_home(frame, app, main_area),
        Screen::LikedSongs => render_list_screen(
            frame,
            main_area,
            ListView {
                title: "Liked Songs",
                fetch: &app.library.liked_songs,
                filter: &app.library.liked_songs_filter,
                selected: app.library.liked_songs_selected,
            },
            &mut scroll.liked_songs,
            |t: &TrackResult| format!("{} \u{2014} {}", t.artist, t.title),
        ),
        Screen::SavedAlbums => render_list_screen(
            frame,
            main_area,
            ListView {
                title: "Saved Albums",
                fetch: &app.library.saved_albums,
                filter: &app.library.saved_albums_filter,
                selected: app.library.saved_albums_selected,
            },
            &mut scroll.saved_albums,
            |a: &crate::api::library::SavedAlbumSummary| format!("{} \u{2014} {}", a.name, a.artist),
        ),
        Screen::FollowedArtists => render_list_screen(
            frame,
            main_area,
            ListView {
                title: "Followed Artists",
                fetch: &app.library.followed_artists,
                filter: &app.library.followed_artists_filter,
                selected: app.library.followed_artists_selected,
            },
            &mut scroll.followed_artists,
            |a: &crate::api::library::FollowedArtist| a.name.clone(),
        ),
        Screen::YourPlaylists => render_your_playlists(frame, app, &mut scroll.playlists, main_area),
        Screen::PlaylistDetail => render_playlist_detail(frame, app, &mut scroll.playlist_detail, main_area),
        Screen::Help => render_help(frame, main_area, &mut scroll.help),
        Screen::Queue => render_queue(frame, app, &mut scroll.queue, main_area),
        Screen::Devices => render_devices(frame, app, &mut scroll.devices, main_area),
        Screen::ArtistDetail => render_artist_detail(frame, app, &mut scroll.artist_detail, main_area),
        Screen::AlbumDetail => render_album_detail(frame, app, &mut scroll.album_detail, main_area),
    }
    render_playbar(frame, app, playbar_area);
    render_status(frame, app, status_area);
    render_overlays(frame, app, &mut scroll.playlist_picker, &mut scroll.quick_jump);
}

/// Draws whichever overlay is active (at most one in practice -- see the
/// field order comment on `AppState`) centered on top of whatever's
/// already been drawn this frame, fullscreen included. Called last
/// specifically so it paints over everything else.
pub fn render_overlays(
    frame: &mut Frame,
    app: &AppState,
    picker_list_state: &mut ListState,
    quick_jump_list_state: &mut ListState,
) {
    if let Some(confirm) = &app.pending_confirm {
        render_confirm_overlay(frame, confirm);
    } else if let Some(prompt) = &app.text_prompt {
        render_text_prompt_overlay(frame, prompt);
    } else if let Some(picker) = &app.playlist_picker {
        render_playlist_picker_overlay(frame, app, picker, picker_list_state);
    } else if let Some(qj) = &app.quick_jump {
        render_quick_jump_overlay(frame, app, qj, quick_jump_list_state);
    }
}

