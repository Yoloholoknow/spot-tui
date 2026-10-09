//! The Now Playing screen. "The selected track" here means the track that is
//! playing, not a list row.

use super::{KeyCtx, common, go_to_screen, is_plain, is_shift_char};
use crate::services::TrackView;
use crate::state::Screen;
use crossterm::event::{KeyCode, KeyEvent};

pub fn handle(ctx: &mut KeyCtx<'_>, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => ctx.app.nav.escape(),
        KeyCode::Char('/') => {
            ctx.app.nav.push(Screen::Search);
            ctx.app.search.clear();
        }
        // Shift+L / Shift+V before plain `v`: a terminal that reports Shift+V
        // as lowercase plus a modifier would otherwise never reach it.
        KeyCode::Char(c) if is_shift_char(KeyCode::Char(c), key.modifiers, 'L', 'l') => {
            if let Some(uri) = playing_uri(ctx) {
                // A toggle, like the heart on the screen: unlike when already liked.
                if ctx.app.current_track_liked() == Some(true) {
                    // Whichever of its ids was saved.
                    let ids = ctx
                        .app
                        .liked_check
                        .as_ref()
                        .map_or_else(|| vec![uri], |(ids, _)| ids.clone());
                    for id in ids {
                        ctx.svc.unlike_track(ctx.app, id);
                    }
                } else {
                    ctx.svc.like_track(ctx.app, uri);
                }
            }
        }
        KeyCode::Char(c) if is_shift_char(KeyCode::Char(c), key.modifiers, 'V', 'v') => {
            open_track_view(ctx, TrackView::Artist)
        }
        KeyCode::Char('v') => open_track_view(ctx, TrackView::Album),
        KeyCode::Char(_) if is_plain(key, 'q') => go_to_screen(ctx, Screen::Queue),
        KeyCode::Char('a') => {
            if let Some(uri) = playing_uri(ctx) {
                super::tracks::open_picker(ctx, uri);
            }
        }
        KeyCode::Up => {
            let _ = ctx.spirc.volume_up();
        }
        KeyCode::Down => {
            let _ = ctx.spirc.volume_down();
        }
        KeyCode::Left => common::seek(ctx, -1),
        KeyCode::Right => common::seek(ctx, 1),
        _ => return false,
    }
    true
}

fn playing_uri(ctx: &KeyCtx<'_>) -> Option<String> {
    ctx.tracker.current_track_id().map(str::to_string)
}

fn open_track_view(ctx: &mut KeyCtx<'_>, view: TrackView) {
    match playing_uri(ctx) {
        Some(uri) => ctx.svc.open_track_view(ctx.app, &uri, view),
        None => ctx.app.status = Some(("nothing playing".to_string(), true)),
    }
}
