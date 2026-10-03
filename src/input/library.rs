//! The Library home menu and the four list screens under it.

use super::lists::{edit_filter, filter_hotkeys, move_selection};
use super::{common, shift, tracks, KeyCtx};
use crate::api::library::PlaylistSummary;
use crate::pins;
use crate::player::play_context;
use crate::state::{AppState, ConfirmAction, PendingConfirm, LIBRARY_ENTRIES};
use crossterm::event::{KeyCode, KeyEvent};

pub fn home(ctx: &mut KeyCtx<'_>, key: KeyEvent) -> bool {
    let lib = &mut ctx.app.library;
    if move_selection(&mut lib.home_selected, LIBRARY_ENTRIES.len(), key.code) {
        return true;
    }
    match key.code {
        KeyCode::Esc | KeyCode::Left => ctx.app.nav.escape(),
        KeyCode::Enter | KeyCode::Right => {
            let (_, screen) = LIBRARY_ENTRIES[lib.home_selected];
            ctx.app.nav.push(screen);
            ctx.svc.ensure_loaded(ctx.app, screen);
        }
        _ => return false,
    }
    true
}

pub fn liked_songs(ctx: &mut KeyCtx<'_>, key: KeyEvent) -> bool {
    let lib = &mut ctx.app.library;
    if edit_filter(&mut lib.liked_songs_filter, &mut lib.liked_songs_selected, key.code)
        || filter_hotkeys(&mut lib.liked_songs_filter, &mut lib.liked_songs_selected, key.code)
    {
        return true;
    }
    match key.code {
        // Every track here is already liked, so Shift+L is the *unlike*
        // direction (elsewhere it likes), and asks first.
        KeyCode::Char(_) if shift(key, 'L') => {
            if let Some(track) = ctx.app.selected_track() {
                ctx.app.pending_confirm = Some(PendingConfirm {
                    message: format!("Unlike \"{} \u{2014} {}\"? y/n", track.artist, track.title),
                    action: ConfirmAction::UnlikeTrack { track_uri: track.uri },
                });
            }
        }
        KeyCode::Esc | KeyCode::Left => ctx.app.nav.escape(),
        KeyCode::Up | KeyCode::Down => {
            let len = ctx.app.liked_display().len();
            move_selection(&mut ctx.app.library.liked_songs_selected, len, key.code);
        }
        // Enter, not Right: it plays and jumps to Now Playing.
        KeyCode::Enter => {
            if let Some(track) = ctx.app.selected_track() {
                play_context(ctx.app, ctx.spirc, track.uri, None, "Liked Songs".to_string());
            }
        }
        _ => return tracks::handle(ctx, key, true),
    }
    true
}

pub fn saved_albums(ctx: &mut KeyCtx<'_>, key: KeyEvent) -> bool {
    let lib = &mut ctx.app.library;
    if edit_filter(&mut lib.saved_albums_filter, &mut lib.saved_albums_selected, key.code)
        || filter_hotkeys(&mut lib.saved_albums_filter, &mut lib.saved_albums_selected, key.code)
    {
        return true;
    }
    let selected = ctx.app.library.saved_albums_selected;
    match key.code {
        // Every album here is already saved, so Shift+S is *unsave*; Album
        // Detail's own Shift+S saves.
        KeyCode::Char(_) if shift(key, 'S') => {
            if let Some(&(_, album)) = ctx.app.saved_albums_display().get(selected) {
                let (message, action) = (
                    format!("Unsave \"{} \u{2014} {}\"? y/n", album.name, album.artist),
                    ConfirmAction::UnsaveAlbum { album_uri: album.uri.clone() },
                );
                ctx.app.pending_confirm = Some(PendingConfirm { message, action });
            }
        }
        KeyCode::Esc | KeyCode::Left => ctx.app.nav.escape(),
        KeyCode::Up | KeyCode::Down => {
            let len = ctx.app.saved_albums_display().len();
            move_selection(&mut ctx.app.library.saved_albums_selected, len, key.code);
        }
        // Navigation only: opening an album plays nothing.
        KeyCode::Enter | KeyCode::Right => {
            let uri = ctx.app.saved_albums_display().get(selected).map(|&(_, a)| a.uri.clone());
            if let Some(uri) = uri {
                ctx.svc.open_album_detail(ctx.app, uri);
            }
        }
        _ => return false,
    }
    true
}

pub fn followed_artists(ctx: &mut KeyCtx<'_>, key: KeyEvent) -> bool {
    let lib = &mut ctx.app.library;
    if edit_filter(&mut lib.followed_artists_filter, &mut lib.followed_artists_selected, key.code)
        || filter_hotkeys(&mut lib.followed_artists_filter, &mut lib.followed_artists_selected, key.code)
    {
        return true;
    }
    let selected = ctx.app.library.followed_artists_selected;
    match key.code {
        // Every artist here is already followed, so Shift+F is *unfollow*;
        // Artist Detail's own Shift+F follows.
        KeyCode::Char(_) if shift(key, 'F') => {
            if let Some(&(_, artist)) = ctx.app.followed_artists_display().get(selected) {
                let (message, action) = (
                    format!("Unfollow \"{}\"? y/n", artist.name),
                    ConfirmAction::UnfollowArtist { artist_uri: artist.uri.clone() },
                );
                ctx.app.pending_confirm = Some(PendingConfirm { message, action });
            }
        }
        KeyCode::Esc | KeyCode::Left => ctx.app.nav.escape(),
        KeyCode::Up | KeyCode::Down => {
            let len = ctx.app.followed_artists_display().len();
            move_selection(&mut ctx.app.library.followed_artists_selected, len, key.code);
        }
        KeyCode::Enter | KeyCode::Right => {
            let uri = ctx.app.followed_artists_display().get(selected).map(|&(_, a)| a.uri.clone());
            if let Some(uri) = uri {
                ctx.svc.open_artist_detail(ctx.app, uri);
            }
        }
        _ => return false,
    }
    true
}

pub fn your_playlists(ctx: &mut KeyCtx<'_>, key: KeyEvent) -> bool {
    let lib = &mut ctx.app.library;
    if edit_filter(&mut lib.playlists_filter, &mut lib.playlists_selected, key.code)
        || filter_hotkeys(&mut lib.playlists_filter, &mut lib.playlists_selected, key.code)
    {
        return true;
    }
    match key.code {
        KeyCode::Char(_) if shift(key, 'R') => {
            if let Some(playlist) = selected_playlist(ctx.app) {
                common::rename_prompt(ctx, playlist);
            }
        }
        // Deleting a playlist is hard to reverse: always confirms.
        KeyCode::Char('d') => {
            if let Some(playlist) = selected_playlist(ctx.app) {
                common::delete_confirm(ctx, playlist);
            }
        }
        KeyCode::Char(_) if shift(key, 'P') => {
            if let Some(uri) = selected_playlist(ctx.app).map(|p| p.uri) {
                pins::toggle_in_place(&mut ctx.app.pinned_playlists, &uri);
                pins::save("playlists", &ctx.app.pinned_playlists);
            }
        }
        KeyCode::Esc | KeyCode::Left => ctx.app.nav.escape(),
        KeyCode::Up | KeyCode::Down => {
            let len = ctx.app.playlists_display().len();
            move_selection(&mut ctx.app.library.playlists_selected, len, key.code);
        }
        KeyCode::Enter | KeyCode::Right => {
            if let Some(playlist) = selected_playlist(ctx.app) {
                ctx.svc.open_playlist_detail(ctx.app, playlist);
            }
        }
        _ => return false,
    }
    true
}

fn selected_playlist(app: &AppState) -> Option<PlaylistSummary> {
    app.playlists_display().get(app.library.playlists_selected).map(|&(_, p)| p.clone())
}
