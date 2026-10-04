//! The four overlays that own the keyboard while open: yes/no confirm, text
//! prompt, add-to-playlist picker and the quick-jump palette.

use super::{KeyCtx, go_to_screen, is_ctrl};
use crate::player::play_context;
use crate::state::{ConfirmAction, Focus, QuickJumpKind, Screen, TextPromptAction};
use crossterm::event::{KeyCode, KeyEvent};

/// `y`/`Enter` confirms and fires the action; `n`/`Esc` cancels; every other
/// key is swallowed so a stray keypress can neither confirm nor dismiss a
/// destructive action.
pub fn handle_confirm(ctx: &mut KeyCtx<'_>, code: KeyCode) {
    match code {
        KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
            if let Some(confirm) = ctx.app.pending_confirm.take() {
                fire_confirmed(ctx, confirm.action);
            }
        }
        KeyCode::Char('n' | 'N') | KeyCode::Esc => ctx.app.pending_confirm = None,
        _ => {}
    }
}

fn fire_confirmed(ctx: &mut KeyCtx<'_>, action: ConfirmAction) {
    let (app, svc) = (&mut *ctx.app, ctx.svc);
    match action {
        ConfirmAction::Quit => ctx.quit = true,
        ConfirmAction::DeletePlaylist(playlist) => svc.delete_playlist(app, playlist),
        ConfirmAction::RemoveTrack {
            playlist_uri,
            track_uri,
            occurrences,
        } => svc.remove_track(app, playlist_uri, track_uri, occurrences),
        ConfirmAction::AddTrackAnyway {
            playlist_uri,
            track_uri,
        } => svc.add_track_to_playlist(app, playlist_uri, track_uri),
        ConfirmAction::UnlikeTrack { track_uri } => svc.unlike_track(app, track_uri),
        ConfirmAction::UnfollowArtist { artist_uri } => svc.unfollow_artist(app, artist_uri),
        ConfirmAction::UnsaveAlbum { album_uri } => svc.unsave_album(app, album_uri),
    }
}

/// `Enter` submits; `Esc` cancels outright (a name that was never submitted
/// has no "keep it applied" state). Everything else edits the field.
pub fn handle_text_prompt(ctx: &mut KeyCtx<'_>, code: KeyCode) {
    match code {
        KeyCode::Esc => ctx.app.text_prompt = None,
        KeyCode::Enter => submit_text_prompt(ctx),
        _ => {
            let Some(prompt) = &mut ctx.app.text_prompt else {
                return;
            };
            match code {
                KeyCode::Backspace => prompt.backspace_at_cursor(),
                KeyCode::Left => prompt.cursor_left(),
                KeyCode::Right => prompt.cursor_right(),
                KeyCode::Char(c) => prompt.insert_at_cursor(c),
                _ => {}
            }
        }
    }
}

fn submit_text_prompt(ctx: &mut KeyCtx<'_>) {
    let Some(prompt) = ctx.app.text_prompt.take() else {
        return;
    };
    // Purely local: needs neither a name nor the API.
    if matches!(prompt.action, TextPromptAction::MoveToPosition) {
        apply_move_to_position(ctx, &prompt.query);
        return;
    }
    let name = prompt.query.trim().to_string();
    if name.is_empty() {
        ctx.app.status = Some(("name can't be empty".to_string(), true));
        return;
    }
    match prompt.action {
        TextPromptAction::CreatePlaylist => ctx.svc.create_playlist(ctx.app, name),
        TextPromptAction::RenamePlaylist(playlist) => {
            ctx.svc.rename_playlist(ctx.app, playlist.uri, name)
        }
        TextPromptAction::MoveToPosition => {}
    }
}

/// Move mode's `g`: splices the moving track to a typed 1-based position
/// instead of nudging it one slot per keystroke. A purely local reorder of
/// the fetched list; move mode stays active, so `Enter` still sends one
/// `reorder_track` for the net displacement and `Esc` still walks it back.
fn apply_move_to_position(ctx: &mut KeyCtx<'_>, input: &str) {
    let app = &mut *ctx.app;
    let Some(pd) = &mut app.playlist_detail else {
        return;
    };
    if pd.move_mode.is_none() {
        return;
    }
    let crate::state::Fetch::Ready(items) = &mut pd.tracks else {
        return;
    };
    match crate::state::parse_move_position(input, items.len()) {
        Ok(target) => pd.selected = crate::state::move_item_to(items, pd.selected, target),
        Err(message) => app.status = Some((message, true)),
    }
}

/// `Up`/`Down` move the selection, `Enter` adds the captured track to the
/// selected playlist, `Esc` cancels. Every printable key narrows the list.
pub fn handle_picker(ctx: &mut KeyCtx<'_>, code: KeyCode) {
    match code {
        KeyCode::Esc => ctx.app.playlist_picker = None,
        KeyCode::Up | KeyCode::Down => {
            let len = ctx.app.picker_display().len();
            if let Some(picker) = &mut ctx.app.playlist_picker {
                super::lists::move_selection(&mut picker.selected, len, code);
            }
        }
        KeyCode::Enter => {
            let picked = ctx.app.playlist_picker.as_ref().and_then(|picker| {
                ctx.app
                    .picker_display()
                    .get(picker.selected)
                    .map(|&(_, p)| (p.uri.clone(), p.name.clone()))
            });
            let Some(picker) = ctx.app.playlist_picker.take() else {
                return;
            };
            if let Some((playlist_uri, playlist_name)) = picked {
                ctx.svc
                    .add_track_checked(ctx.app, playlist_uri, playlist_name, picker.track_uri);
            }
        }
        _ => {
            let Some(picker) = &mut ctx.app.playlist_picker else {
                return;
            };
            match code {
                KeyCode::Left => picker.filter.cursor_left(),
                KeyCode::Right => picker.filter.cursor_right(),
                KeyCode::Backspace => {
                    picker.filter.backspace_at_cursor();
                    picker.selected = 0;
                }
                KeyCode::Char(c) => {
                    picker.filter.insert_at_cursor(c);
                    picker.selected = 0;
                }
                _ => {}
            }
        }
    }
}

/// Same shape as the picker, plus `Ctrl+P` again to close (toggle) instead
/// of inserting a stray `p`.
pub fn handle_quick_jump(ctx: &mut KeyCtx<'_>, key: KeyEvent) {
    if is_ctrl(key, 'p') {
        ctx.app.quick_jump = None;
        return;
    }
    let code = key.code;
    match code {
        KeyCode::Esc => ctx.app.quick_jump = None,
        KeyCode::Up | KeyCode::Down => {
            let len = ctx.app.quick_jump_matches().len();
            if let Some(qj) = &mut ctx.app.quick_jump {
                super::lists::move_selection(&mut qj.selected, len, code);
            }
        }
        KeyCode::Enter => {
            let picked = ctx.app.quick_jump.as_ref().and_then(|qj| {
                ctx.app
                    .quick_jump_matches()
                    .get(qj.selected)
                    .map(|e| e.kind.clone())
            });
            ctx.app.quick_jump = None;
            if let Some(kind) = picked {
                activate_quick_jump(ctx, kind);
            }
        }
        _ => {
            let Some(qj) = &mut ctx.app.quick_jump else {
                return;
            };
            match code {
                KeyCode::Left => qj.filter.cursor_left(),
                KeyCode::Right => qj.filter.cursor_right(),
                KeyCode::Backspace => {
                    qj.filter.backspace_at_cursor();
                    qj.selected = 0;
                }
                KeyCode::Char(c) => {
                    qj.filter.insert_at_cursor(c);
                    qj.selected = 0;
                }
                _ => {}
            }
        }
    }
}

/// Does what the entity's own screen does on `Enter`. Playlists, artists and
/// albums collapse the stack to `[NowPlaying]` first so the destination lands
/// at a clean `[NowPlaying, X]` ("teleport"), unlike Help, which drills in
/// and returns to wherever it was opened from.
fn activate_quick_jump(ctx: &mut KeyCtx<'_>, kind: QuickJumpKind) {
    match kind {
        QuickJumpKind::Screen(Screen::Help) => {
            ctx.app.nav.push(Screen::Help);
            ctx.app.nav.focus = Focus::Main;
        }
        QuickJumpKind::Screen(screen) => go_to_screen(ctx, screen),
        QuickJumpKind::Playlist(playlist) => {
            ctx.app.nav.goto(Screen::NowPlaying);
            ctx.svc.open_playlist_detail(ctx.app, playlist);
            ctx.app.nav.focus = Focus::Main;
        }
        QuickJumpKind::Artist { uri } => {
            ctx.app.nav.goto(Screen::NowPlaying);
            ctx.svc.open_artist_detail(ctx.app, uri);
            ctx.app.nav.focus = Focus::Main;
        }
        QuickJumpKind::Album { uri } => {
            ctx.app.nav.goto(Screen::NowPlaying);
            ctx.svc.open_album_detail(ctx.app, uri);
            ctx.app.nav.focus = Focus::Main;
        }
        QuickJumpKind::Track(track) => play_context(
            ctx.app,
            ctx.spirc,
            track.uri,
            None,
            "Liked Songs".to_string(),
        ),
        // Stays wherever the user was.
        QuickJumpKind::Device(device) => ctx.svc.transfer_playback(ctx.app, device.id),
    }
}
