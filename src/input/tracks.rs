//! Keys for the track under the cursor, shared by every screen that lists
//! tracks (Liked Songs, Playlist Detail, Queue, Album Detail).

use super::{shift, KeyCtx};
use crate::state::{ListFilter, PlaylistPicker};
use crossterm::event::{KeyCode, KeyEvent};

/// `Shift+L` like, `q` add to queue, `a` add to playlist, `v` open the
/// track's album, `Shift+V` open its artist. Screens where a key means
/// something else handle it before calling this. Returns whether the key
/// was one of these (even if no track was selected), so it never falls
/// through to a global binding.
pub fn handle(ctx: &mut KeyCtx<'_>, key: KeyEvent, allow_queue: bool) -> bool {
    match key.code {
        KeyCode::Char(_) if shift(key, 'L') => {
            if let Some(track) = ctx.app.selected_track() {
                ctx.svc.like_track(ctx.app, track.uri);
            }
        }
        KeyCode::Char('q') if allow_queue => {
            if let Some(track) = ctx.app.selected_track() {
                ctx.svc.add_to_queue(ctx.app, track.uri);
            }
        }
        KeyCode::Char('a') => {
            if let Some(track) = ctx.app.selected_track() {
                open_picker(ctx, track.uri);
            }
        }
        KeyCode::Char(_) if shift(key, 'V') => {
            if let Some(track) = ctx.app.selected_track() {
                ctx.svc.open_artist_detail(ctx.app, track.artist_uri);
            }
        }
        // `v` is the album, the more common destination from a track.
        KeyCode::Char('v') => {
            if let Some(track) = ctx.app.selected_track() {
                ctx.svc.open_album_detail(ctx.app, track.album_uri);
            }
        }
        _ => return false,
    }
    true
}

pub fn open_picker(ctx: &mut KeyCtx<'_>, track_uri: String) {
    ctx.app.playlist_picker = Some(PlaylistPicker { track_uri, selected: 0, filter: ListFilter::default() });
}
