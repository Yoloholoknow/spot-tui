//! Keyboard handling.
//!
//! [`handle_key`] routes each key press. Overlays (confirm, text prompt,
//! playlist picker, quick jump) own the keyboard while open. Next come the
//! few keys that work everywhere, then the focused pane's handler: the
//! sidebar, or the screen on top of the stack. Every handler returns whether
//! it consumed the key; the ones that did not fall through to `common`,
//! which holds the keys shared by (almost) every screen (transport, volume,
//! fullscreen, help, ...). That is why a screen only lists what is special
//! about it.

mod browse;
mod common;
mod library;
mod lists;
mod now_playing;
mod overlays;
mod playlist_detail;
mod search;
mod sidebar;
mod tracks;

use crate::lyrics::romanizer::{self, Romanizer};
use crate::player::{cycle_repeat, cycle_shuffle};
use crate::position::PositionTracker;
use crate::services::Services;
use crate::state::{AppState, ConfirmAction, Focus, ListFilter, PendingConfirm, QuickJump, Screen};
use crate::ui::ScrollState;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use librespot_connect::Spirc;

/// Everything a key handler may read or change, borrowed field by field from
/// the main loop.
pub struct KeyCtx<'a> {
    pub app: &'a mut AppState,
    pub svc: &'a Services,
    pub spirc: &'a Spirc,
    pub tracker: &'a PositionTracker,
    pub scroll: &'a mut ScrollState,
    pub romanizer: &'a mut Romanizer,
    /// Generation of the track currently playing, for starting romanization.
    pub generation: u64,
    /// `confirm_quit` from the config.
    pub confirm_quit: bool,
    /// Set by a handler to end the main loop.
    pub quit: bool,
    /// Set by a handler to sign out: the main loop deletes the stored logins
    /// and returns to the signed-out screen.
    pub sign_out: bool,
}

/// A physical Shift+<letter>: the literal uppercase char (how most
/// terminals report it) or lowercase plus the SHIFT modifier (how some
/// report it instead).
pub fn is_shift_char(code: KeyCode, modifiers: KeyModifiers, upper: char, lower: char) -> bool {
    code == KeyCode::Char(upper)
        || (code == KeyCode::Char(lower) && modifiers.contains(KeyModifiers::SHIFT))
}

/// Like [`is_shift_char`], for the common case of matching a key event.
fn shift(key: KeyEvent, upper: char) -> bool {
    is_shift_char(key.code, key.modifiers, upper, upper.to_ascii_lowercase())
}

fn is_plain(key: KeyEvent, c: char) -> bool {
    key.code == KeyCode::Char(c)
        && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT)
}

fn is_ctrl(key: KeyEvent, c: char) -> bool {
    key.code == KeyCode::Char(c) && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Whether a printable key is about to be typed into a text field, so global
/// letter bindings must stay out of the way. Scoped to the field that is
/// actually on top with focus: `Tab` moves focus to the sidebar without
/// ending a filter edit, and a stale `editing` flag must not leave keys dead.
fn text_input_active(app: &AppState) -> bool {
    if app.nav.focus != Focus::Main {
        return false;
    }
    match *app.nav.top() {
        Screen::Search => true,
        Screen::LikedSongs => app.library.liked_songs_filter.editing,
        Screen::SavedAlbums => app.library.saved_albums_filter.editing,
        Screen::FollowedArtists => app.library.followed_artists_filter.editing,
        Screen::YourPlaylists => app.library.playlists_filter.editing,
        Screen::PlaylistDetail => app
            .playlist_detail
            .as_ref()
            .is_some_and(|pd| pd.filter.editing),
        _ => false,
    }
}

pub fn handle_key(ctx: &mut KeyCtx<'_>, key: KeyEvent) {
    // A result message lasts until the next key press.
    ctx.app.status = None;

    if is_ctrl(key, 'c') {
        ctx.quit = true;
        return;
    }

    // Overlays are checked before everything, `Tab` included, so none of
    // them leaks a key to the screen underneath. Confirm gates hardest.
    if ctx.app.pending_confirm.is_some() {
        overlays::handle_confirm(ctx, key.code);
        return;
    }
    if ctx.app.text_prompt.is_some() {
        overlays::handle_text_prompt(ctx, key.code);
        return;
    }
    if ctx.app.playlist_picker.is_some() {
        overlays::handle_picker(ctx, key.code);
        return;
    }
    if ctx.app.quick_jump.is_some() {
        overlays::handle_quick_jump(ctx, key);
        return;
    }

    // `Tab` is its own KeyCode, never a `Char`, so it cannot eat a literal
    // keystroke a text field wants.
    if key.code == KeyCode::Tab {
        ctx.app.nav.toggle_focus();
        return;
    }
    if is_ctrl(key, 'p') {
        open_quick_jump(ctx);
        return;
    }

    // Global letter keys. Each is skipped while a text field is typing; Ctrl,
    // Alt and (where it matters) Shift variants fall through to the screens,
    // which give them other meanings (Shift+S saves an album, Shift+R renames).
    if !text_input_active(ctx.app) {
        if is_plain(key, 's') {
            cycle_shuffle(ctx.app, ctx.spirc);
            return;
        }
        if is_plain(key, 'r') {
            cycle_repeat(ctx.app, ctx.spirc);
            return;
        }
        if key.code == KeyCode::Char('t')
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            ctx.app.romanize_lyrics = !ctx.app.romanize_lyrics;
            ctx.app.status = Some((
                romanizer::status(ctx.app.romanize_lyrics, &ctx.app.lyrics),
                false,
            ));
            ctx.romanizer.request(ctx.app, ctx.generation);
            return;
        }
        // Shift+Q asks first when `confirm_quit` is on. Plain `q` is
        // add-to-queue, so the single most dangerous key is not the easy one.
        if shift(key, 'Q') && ctx.confirm_quit {
            ctx.app.pending_confirm = Some(PendingConfirm {
                message: "Quit spot-tui? y/n".to_string(),
                action: ConfirmAction::Quit,
            });
            return;
        }
    }

    let consumed = if ctx.app.nav.focus == Focus::Sidebar {
        sidebar::handle(ctx, key)
    } else {
        match *ctx.app.nav.top() {
            Screen::Search => search::handle(ctx, key),
            Screen::NowPlaying => now_playing::handle(ctx, key),
            Screen::Library => library::home(ctx, key),
            Screen::LikedSongs => library::liked_songs(ctx, key),
            Screen::SavedAlbums => library::saved_albums(ctx, key),
            Screen::FollowedArtists => library::followed_artists(ctx, key),
            Screen::YourPlaylists => library::your_playlists(ctx, key),
            Screen::PlaylistDetail => playlist_detail::handle(ctx, key),
            Screen::Help => browse::help(ctx, key),
            Screen::Queue => browse::queue(ctx, key),
            Screen::Devices => browse::devices(ctx, key),
            Screen::ArtistDetail => browse::artist_detail(ctx, key),
            Screen::AlbumDetail => browse::album_detail(ctx, key),
        }
    };
    if !consumed {
        common::handle(ctx, key);
    }
}

/// Opens the palette, first starting the fetch for any lazily-loaded
/// category not yet visited this run so it can appear in results.
fn open_quick_jump(ctx: &mut KeyCtx<'_>) {
    for screen in [
        Screen::SavedAlbums,
        Screen::FollowedArtists,
        Screen::Devices,
    ] {
        ctx.svc.ensure_loaded(ctx.app, screen);
    }
    ctx.app.quick_jump = Some(QuickJump {
        filter: ListFilter::default(),
        selected: 0,
    });
}

/// Switches to `screen` from the sidebar or quick jump: collapses any
/// drill-down, focuses the main pane, and resets or loads what the screen
/// needs.
fn go_to_screen(ctx: &mut KeyCtx<'_>, screen: Screen) {
    ctx.app.nav.goto(screen);
    ctx.app.nav.focus = Focus::Main;
    if screen == Screen::Search {
        ctx.app.search.clear();
    }
    ctx.svc.ensure_loaded(ctx.app, screen);
}
