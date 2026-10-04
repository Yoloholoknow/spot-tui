//! The Search screen. Every printable key has to reach the query box, so
//! actions on a result use Ctrl/Alt + arrow instead of letters, and this
//! handler consumes every key (nothing falls through to the global keys).

use super::KeyCtx;
use crate::player::play_context;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn handle(ctx: &mut KeyCtx<'_>, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let search = &mut ctx.app.search;
    match key.code {
        // Not Left: Left/Right move the cursor within the query, so they
        // cannot also mean "leave".
        KeyCode::Esc => ctx.app.nav.escape(),
        KeyCode::Backspace => {
            search.backspace_at_cursor();
            search.results.clear();
            search.error = None;
        }
        KeyCode::Char(c) => {
            search.insert_at_cursor(c);
            search.results.clear();
            search.error = None;
        }
        KeyCode::Left => search.cursor_left(),
        // Ctrl+Right: the result's album. Alt+Right: its artist.
        KeyCode::Right if ctrl => {
            if let Some(track) = ctx.app.selected_track() {
                ctx.svc.open_album_detail(ctx.app, track.album_uri);
            }
        }
        KeyCode::Right if alt => {
            if let Some(track) = ctx.app.selected_track() {
                ctx.svc.open_artist_detail(ctx.app, track.artist_uri);
            }
        }
        KeyCode::Right => search.cursor_right(),
        // Ctrl+Up, not Ctrl+L: terminals and multiplexers commonly reserve
        // Ctrl+L, so it never reached the app. Always *like*: results are not
        // a "you already have this" list, so there is no unlike direction.
        KeyCode::Up if ctrl => {
            if let Some(track) = ctx.app.selected_track() {
                ctx.svc.like_track(ctx.app, track.uri);
            }
        }
        KeyCode::Up => search.selected = search.selected.saturating_sub(1),
        // Ctrl+Down: add to a playlist without leaving Search or playing it.
        KeyCode::Down if ctrl => {
            if let Some(track) = ctx.app.selected_track() {
                super::tracks::open_picker(ctx, track.uri);
            }
        }
        // Alt+Down: add to the queue. Both modified arms must precede the
        // plain one, which matches regardless of modifiers.
        KeyCode::Down if alt => {
            if let Some(track) = ctx.app.selected_track() {
                ctx.svc.add_to_queue(ctx.app, track.uri);
            }
        }
        KeyCode::Down => {
            if !search.results.is_empty() {
                search.selected = (search.selected + 1).min(search.results.len() - 1);
            }
        }
        // Enter, not Right: it plays and jumps to Now Playing, a real "leave
        // here" side effect. Right means "go deeper".
        KeyCode::Enter => {
            if search.results.is_empty() {
                if !search.query.trim().is_empty() {
                    let query = search.query.clone();
                    ctx.svc.search(ctx.app, query);
                }
            } else if let Some(track) = ctx.app.selected_track() {
                play_context(ctx.app, ctx.spirc, track.uri, None, "Search".to_string());
            }
        }
        _ => {}
    }
    true
}
