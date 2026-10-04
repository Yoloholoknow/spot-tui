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
pub use self::lyrics_view::word_sweep_active;
use self::lyrics_view::*;
use self::now_playing::*;
use self::overlays::*;
use self::playbar::*;
use self::screens::*;
use self::theme::*;

use crate::api::search::TrackResult;
use crate::state::*;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Gauge, List, ListItem, ListState, Padding, Paragraph, Wrap,
};
use std::time::Duration;

/// Persisted scroll offsets, one per list. Kept across frames because a fresh
/// `ListState` each frame (offset 0) makes ratatui re-pin the highlight to the
/// bottom edge on every render once the list is longer than the screen;
/// persisting the offset lets the viewport move only when the selection leaves
/// it. A separate struct from `AppState` so rendering can mutate one of these
/// while reading other `AppState` fields, which one struct would not allow.
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
    /// Help's scroll offset in rendered rows, a plain `u16` for `Paragraph::scroll`:
    /// Help is read, not navigated item by item. Clamped inside `render_help` against
    /// the real height, so key handlers can change it blindly.
    pub help: u16,
}

/// Album art state, threaded through `render` like `ScrollState` (it is mutable
/// render-side cache, not application state).
///
/// `picker` is set once at startup; `None` means no graphics protocol is
/// available, and `render_art` then only draws the placeholder.
///
/// `cover_image` is the playing track's decoded cover with its URI.
/// `sized_covers` caches the protocol-encoded image per `(uri, width, height)`,
/// built lazily by `render_art`. A single shared protocol would re-encode and,
/// on Kitty, fully retransmit the image whenever its render size changed, and the
/// compact and fullscreen layouts use different sizes, so every `f` toggle lagged
/// and could corrupt the display. One entry per size seen avoids that after the
/// first visit to each.
#[derive(Default)]
pub struct ImageState {
    pub picker: Option<ratatui_image::picker::Picker>,
    pub cover_image: Option<(String, image::DynamicImage)>,
    pub sized_covers: Vec<(String, u16, u16, ratatui_image::protocol::StatefulProtocol)>,
    /// One-shot retransmit: armed when the first sized cover is built, fired
    /// `STARTUP_RETRANSMIT_DELAY` later by clearing the cache so the next render
    /// re-encodes and retransmits. This does automatically what skipping a track and
    /// back does by hand to fix a blank first cover. The delay is a judgment call, not
    /// a proven-safe value: 700 ms was tried and corrupted every later cover in
    /// Ghostty, which was still settling the first transmission. If blank art
    /// returns, widen it; if corruption returns, it is still too short.
    pub startup_retransmit_at: Option<std::time::Instant>,
    pub startup_retransmit_done: bool,
    /// The prefetched next track's decoded cover, keyed by its URI. Separate from
    /// `cover_image` so a background prefetch can never replace what is on screen.
    /// Moved into `cover_image` when that track actually starts.
    pub prewarmed_cover: Option<(String, image::DynamicImage)>,
}

/// Delay between the first cover transmission and the one-shot retransmit; see
/// `ImageState::startup_retransmit_at`.
pub const STARTUP_RETRANSMIT_DELAY: std::time::Duration = std::time::Duration::from_millis(2000);

/// How many `(track, size)` encoded protocols `ImageState` keeps: more than the
/// handful of sizes a session produces (compact, fullscreen, stacked), so
/// eviction is rare.
pub const SIZED_COVER_CACHE_CAP: usize = 4;

pub const STARTUP_SPINNER: [char; 10] = [
    '\u{280B}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283C}', '\u{2834}', '\u{2826}', '\u{2827}',
    '\u{2807}', '\u{280F}',
];

/// Drawn while the first connect resolves (several seconds: AP resolution, auth,
/// first track load). It also holds off the first frame with real album art:
/// drawn too early after entering the alternate screen, Ghostty's kitty-graphics
/// subsystem is not ready and the first cover renders blank.
pub fn render_startup(frame: &mut Frame, tick: usize) {
    let area = frame.area();
    let spinner = STARTUP_SPINNER[tick % STARTUP_SPINNER.len()];
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new("spot-tui")
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::BOLD).fg(ACCENT)),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(format!("{spinner} connecting to spotify\u{2026}"))
            .alignment(Alignment::Center)
            .style(Style::default().fg(DIM)),
        rows[2],
    );
}

pub fn render(
    frame: &mut Frame,
    app: &AppState,
    scroll: &mut ScrollState,
    images: &mut ImageState,
) {
    if app.fullscreen && *app.nav.top() == Screen::NowPlaying {
        render_fullscreen(frame, app, images);
        render_overlays(
            frame,
            app,
            &mut scroll.playlist_picker,
            &mut scroll.quick_jump,
        );
        return;
    }

    let area = frame.area();
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
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
            |a: &crate::api::library::SavedAlbumSummary| {
                format!("{} \u{2014} {}", a.name, a.artist)
            },
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
        Screen::YourPlaylists => {
            render_your_playlists(frame, app, &mut scroll.playlists, main_area)
        }
        Screen::PlaylistDetail => {
            render_playlist_detail(frame, app, &mut scroll.playlist_detail, main_area)
        }
        Screen::Help => render_help(frame, main_area, &mut scroll.help),
        Screen::Queue => render_queue(frame, app, &mut scroll.queue, main_area),
        Screen::Devices => render_devices(frame, app, &mut scroll.devices, main_area),
        Screen::ArtistDetail => {
            render_artist_detail(frame, app, &mut scroll.artist_detail, main_area)
        }
        Screen::AlbumDetail => render_album_detail(frame, app, &mut scroll.album_detail, main_area),
    }
    render_playbar(frame, app, playbar_area);
    render_status(frame, app, status_area);
    render_overlays(
        frame,
        app,
        &mut scroll.playlist_picker,
        &mut scroll.quick_jump,
    );
}

/// Draws the active overlay, if any, over everything else this frame (fullscreen
/// included), so it is called last.
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
