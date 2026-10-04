//! Help, Queue, Devices, Artist Detail and Album Detail.

use super::lists::move_selection;
use super::{KeyCtx, shift, tracks};
use crate::player::play_context;
use crate::state::{Fetch, Screen};
use crossterm::event::{KeyCode, KeyEvent};

/// Help scrolls by rows. The offset is clamped inside `render_help` against
/// the real rendered height, so these can move it blindly.
pub fn help(ctx: &mut KeyCtx<'_>, key: KeyEvent) -> bool {
    let scroll = &mut ctx.scroll.help;
    match key.code {
        KeyCode::Esc | KeyCode::Left => {
            *scroll = 0;
            ctx.app.nav.escape();
        }
        KeyCode::Char('l') => {
            *scroll = 0;
            ctx.app.nav.goto(Screen::Library);
        }
        KeyCode::Up => *scroll = scroll.saturating_sub(1),
        KeyCode::Down => *scroll = scroll.saturating_add(1),
        KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
        KeyCode::PageDown => *scroll = scroll.saturating_add(10),
        _ => return false,
    }
    true
}

/// No remove or reorder here: the public Web API has no such endpoint for the
/// queue. Adding a queued track to a playlist is the one real mutation.
pub fn queue(ctx: &mut KeyCtx<'_>, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc | KeyCode::Left => ctx.app.nav.escape(),
        KeyCode::Up | KeyCode::Down => {
            let len = match &ctx.app.queue.fetch {
                Fetch::Ready(summary) => summary.queue.len(),
                _ => 0,
            };
            move_selection(&mut ctx.app.queue.selected, len, key.code);
        }
        _ => return tracks::handle(ctx, key, false),
    }
    true
}

pub fn devices(ctx: &mut KeyCtx<'_>, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc | KeyCode::Left => ctx.app.nav.escape(),
        KeyCode::Char(_) if shift(key, 'R') => ctx.svc.refetch_devices(ctx.app),
        KeyCode::Up | KeyCode::Down => {
            let len = match &ctx.app.devices.fetch {
                Fetch::Ready(items) => items.len(),
                _ => 0,
            };
            move_selection(&mut ctx.app.devices.selected, len, key.code);
        }
        KeyCode::Enter => {
            let id = match &ctx.app.devices.fetch {
                Fetch::Ready(items) => items.get(ctx.app.devices.selected).map(|d| d.id.clone()),
                _ => None,
            };
            if let Some(id) = id {
                ctx.svc.transfer_playback(ctx.app, id);
            }
        }
        _ => return false,
    }
    true
}

pub fn artist_detail(ctx: &mut KeyCtx<'_>, key: KeyEvent) -> bool {
    match key.code {
        // Follows the artist this screen is about; unfollow lives on the
        // Followed Artists list.
        KeyCode::Char(_) if shift(key, 'F') => {
            if let Some(state) = &ctx.app.artist_detail {
                ctx.svc.follow_artist(ctx.app, state.artist_uri.clone());
            }
        }
        KeyCode::Esc | KeyCode::Left => ctx.app.nav.escape(),
        KeyCode::Up | KeyCode::Down => {
            let Some(state) = &mut ctx.app.artist_detail else {
                return true;
            };
            let len = match &state.detail {
                Fetch::Ready(artist) => artist.albums.len(),
                _ => 0,
            };
            move_selection(&mut state.selected, len, key.code);
        }
        // Navigation only: opening an album plays nothing.
        KeyCode::Enter | KeyCode::Right => {
            let uri = ctx
                .app
                .artist_detail
                .as_ref()
                .and_then(|state| match &state.detail {
                    Fetch::Ready(artist) => {
                        artist.albums.get(state.selected).map(|a| a.uri.clone())
                    }
                    _ => None,
                });
            if let Some(uri) = uri {
                ctx.svc.open_album_detail(ctx.app, uri);
            }
        }
        _ => return false,
    }
    true
}

pub fn album_detail(ctx: &mut KeyCtx<'_>, key: KeyEvent) -> bool {
    match key.code {
        // Saves the album being viewed; unsave lives on Saved Albums.
        KeyCode::Char(_) if shift(key, 'S') => {
            if let Some(state) = &ctx.app.album_detail {
                ctx.svc.save_album(ctx.app, state.album_uri.clone());
            }
        }
        // `v` here is the album's own artist, not the selected track's.
        KeyCode::Char('v') => {
            let uri = ctx
                .app
                .album_detail
                .as_ref()
                .and_then(|state| match &state.detail {
                    Fetch::Ready(album) => Some(album.artist_uri.clone()),
                    _ => None,
                });
            if let Some(uri) = uri {
                ctx.svc.open_artist_detail(ctx.app, uri);
            }
        }
        KeyCode::Esc | KeyCode::Left => ctx.app.nav.escape(),
        KeyCode::Up | KeyCode::Down => {
            let Some(state) = &mut ctx.app.album_detail else {
                return true;
            };
            let len = match &state.detail {
                Fetch::Ready(album) => album.tracks.len(),
                _ => 0,
            };
            move_selection(&mut state.selected, len, key.code);
        }
        // Enter, not Right: it plays and jumps to Now Playing.
        KeyCode::Enter => {
            let Some(state) = &ctx.app.album_detail else {
                return true;
            };
            if let Fetch::Ready(album) = &state.detail
                && state.selected < album.tracks.len()
            {
                let (uri, name, index) =
                    (album.uri.clone(), album.name.clone(), state.selected as u32);
                play_context(ctx.app, ctx.spirc, uri, Some(index), name);
            }
        }
        _ => return tracks::handle(ctx, key, true),
    }
    true
}
