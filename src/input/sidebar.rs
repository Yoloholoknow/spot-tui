//! Keys while the sidebar has focus.

use super::{KeyCtx, common, go_to_screen, is_plain, is_shift_char};
use crate::pins;
use crate::state::{Focus, Screen, SidebarRow, sidebar_rows};
use crossterm::event::{KeyCode, KeyEvent};

pub fn handle(ctx: &mut KeyCtx<'_>, key: KeyEvent) -> bool {
    match key.code {
        // `?`, `f` and `l` hand focus to the main pane like every other
        // sidebar activation; without that, Esc would do nothing in Help.
        KeyCode::Char('?') => {
            ctx.app.nav.push(Screen::Help);
            ctx.app.nav.focus = Focus::Main;
        }
        KeyCode::Char('f') => {
            common::toggle_or_enter_fullscreen(ctx);
            ctx.app.nav.focus = Focus::Main;
        }
        KeyCode::Char('l') => {
            ctx.app.nav.goto(Screen::Library);
            ctx.app.nav.focus = Focus::Main;
        }
        KeyCode::Char('/') => go_to_screen(ctx, Screen::Search),
        // Same shortcut as the Now Playing pane, which focus often isn't on.
        KeyCode::Char(_) if is_plain(key, 'q') && *ctx.app.nav.top() == Screen::NowPlaying => {
            go_to_screen(ctx, Screen::Queue)
        }
        KeyCode::Char(c) if is_shift_char(KeyCode::Char(c), key.modifiers, 'P', 'p') => {
            pin_selected_playlist(ctx)
        }
        KeyCode::Up => ctx.app.sidebar_sel = ctx.app.sidebar_sel.saturating_sub(1),
        KeyCode::Down => {
            let rows = sidebar_rows(ctx.app).len();
            ctx.app.sidebar_sel = (ctx.app.sidebar_sel + 1).min(rows.saturating_sub(1));
        }
        // Right drills into the selected row, like yazi/ranger. Left has
        // nothing further left to go to.
        KeyCode::Enter | KeyCode::Right => activate_selected(ctx),
        _ => return false,
    }
    true
}

fn pin_selected_playlist(ctx: &mut KeyCtx<'_>) {
    let uri = match sidebar_rows(ctx.app).get(ctx.app.sidebar_sel) {
        Some(SidebarRow::Playlist(p)) => Some(p.uri.clone()),
        _ => None,
    };
    if let Some(uri) = uri {
        pins::toggle_in_place(&mut ctx.app.pinned_playlists, &uri);
        pins::save("playlists", &ctx.app.pinned_playlists);
    }
}

fn activate_selected(ctx: &mut KeyCtx<'_>) {
    // Resolved to an owned value first: the row borrows `app`, which the
    // action then mutates.
    enum Action {
        Goto(Screen),
        OpenPlaylist(crate::api::library::PlaylistSummary),
    }
    let action = match sidebar_rows(ctx.app).get(ctx.app.sidebar_sel) {
        Some(SidebarRow::Menu(_, screen)) => Some(Action::Goto(*screen)),
        Some(SidebarRow::Playlist(p)) => Some(Action::OpenPlaylist((*p).clone())),
        None => None,
    };
    match action {
        Some(Action::Goto(screen)) => go_to_screen(ctx, screen),
        Some(Action::OpenPlaylist(playlist)) => {
            ctx.svc.open_playlist_detail(ctx.app, playlist);
            ctx.app.nav.focus = Focus::Main;
        }
        None => {}
    }
}
