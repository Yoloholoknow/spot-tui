//! Keys shared by (almost) every screen. Reached only when the focused
//! screen's own handler did not consume the key.

use super::{shift, KeyCtx};
use crate::player::{mute_toggle, seek_target_ms, SEEK_STEP_MS};
use crate::api::library::PlaylistSummary;
use crate::state::{ConfirmAction, PendingConfirm, Screen, TextPrompt, TextPromptAction};
use crate::terminal::tmux_toggle_zoom;
use crossterm::event::{KeyCode, KeyEvent};
use std::time::Instant;

pub fn handle(ctx: &mut KeyCtx<'_>, key: KeyEvent) {
    match key.code {
        KeyCode::Char(_) if shift(key, 'Q') => ctx.quit = true,
        KeyCode::Char('?') if *ctx.app.nav.top() != Screen::Help => ctx.app.nav.push(Screen::Help),
        KeyCode::Char(' ') => {
            let _ = ctx.spirc.play_pause();
        }
        KeyCode::Char('n') => {
            let _ = ctx.spirc.next();
        }
        KeyCode::Char('p') => {
            let _ = ctx.spirc.prev();
        }
        KeyCode::Char('+') => {
            let _ = ctx.spirc.volume_up();
        }
        KeyCode::Char('-') => {
            let _ = ctx.spirc.volume_down();
        }
        KeyCode::Char('m') => toggle_mute(ctx),
        KeyCode::Char('f') => toggle_or_enter_fullscreen(ctx),
        // `goto`, not `push`, so repeated presses never pile up depth.
        KeyCode::Char('l') => ctx.app.nav.goto(Screen::Library),
        KeyCode::Char('c') => new_playlist_prompt(ctx),
        _ => {}
    }
}

pub fn new_playlist_prompt(ctx: &mut KeyCtx<'_>) {
    ctx.app.text_prompt = Some(TextPrompt::new("New playlist name", "", TextPromptAction::CreatePlaylist));
}

pub fn toggle_mute(ctx: &mut KeyCtx<'_>) {
    let target = mute_toggle(ctx.app.volume, &mut ctx.app.muted_volume);
    let _ = ctx.spirc.set_volume(target);
}

/// Already on Now Playing: toggles fullscreen in place. Anywhere else: jumps
/// straight to the fullscreen view, since flipping the flag would have no
/// visible effect until Now Playing was reached some other way.
pub fn toggle_or_enter_fullscreen(ctx: &mut KeyCtx<'_>) {
    if *ctx.app.nav.top() == Screen::NowPlaying {
        ctx.app.fullscreen = !ctx.app.fullscreen;
    } else {
        ctx.app.fullscreen = true;
        ctx.app.nav.goto(Screen::NowPlaying);
    }
    tmux_toggle_zoom();
}

/// Seeks by `direction` (+1 / -1) steps from the live playback position.
pub fn seek(ctx: &mut KeyCtx<'_>, direction: i64) {
    let target = seek_target_ms(
        ctx.tracker.progress_ms(Instant::now()) as i64,
        direction * SEEK_STEP_MS,
        ctx.app.duration.as_millis() as i64,
    );
    let _ = ctx.spirc.set_position_ms(target);
}

pub fn rename_prompt(ctx: &mut KeyCtx<'_>, playlist: PlaylistSummary) {
    ctx.app.text_prompt =
        Some(TextPrompt::new("Rename playlist", playlist.name.clone(), TextPromptAction::RenamePlaylist(playlist)));
}

/// Always confirms: deleting a playlist is hard to reverse.
pub fn delete_confirm(ctx: &mut KeyCtx<'_>, playlist: PlaylistSummary) {
    ctx.app.pending_confirm = Some(PendingConfirm {
        message: format!("Delete playlist \"{}\"? y/n", playlist.name),
        action: ConfirmAction::DeletePlaylist(playlist),
    });
}
