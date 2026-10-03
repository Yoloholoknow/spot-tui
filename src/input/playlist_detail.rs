//! The Playlist Detail screen, including its move (reorder) mode.

use super::lists::{edit_filter, filter_hotkeys, move_selection};
use super::{common, shift, tracks, KeyCtx};
use crate::pins;
use crate::player::play_context;
use crate::state::{
    move_item_down, move_item_to, move_item_up, AppState, ConfirmAction, Fetch, PendingConfirm, TextPrompt,
    TextPromptAction,
};
use crossterm::event::{KeyCode, KeyEvent};

pub fn handle(ctx: &mut KeyCtx<'_>, key: KeyEvent) -> bool {
    let Some(pd) = &mut ctx.app.playlist_detail else { return false };
    if edit_filter(&mut pd.filter, &mut pd.selected, key.code) {
        return true;
    }
    // While a track is being moved, every other key is swallowed: editing the
    // playlist mid-reorder would race the pending local move.
    if pd.move_mode.is_some() {
        handle_move_mode(ctx, key.code);
        return true;
    }
    if filter_hotkeys(&mut pd.filter, &mut pd.selected, key.code) {
        return true;
    }
    match key.code {
        KeyCode::Char('m') => enter_move_mode(ctx.app),
        // Shift+D deletes the open playlist; plain `d` removes the selected
        // track. Shift+R renames the playlist, not a track.
        KeyCode::Char(_) if shift(key, 'D') => {
            if let Some(pd) = &ctx.app.playlist_detail {
                common::delete_confirm(ctx, pd.playlist.clone());
            }
        }
        KeyCode::Char(_) if shift(key, 'R') => {
            if let Some(pd) = &ctx.app.playlist_detail {
                common::rename_prompt(ctx, pd.playlist.clone());
            }
        }
        KeyCode::Char('d') => confirm_remove_track(ctx.app),
        KeyCode::Char(_) if shift(key, 'P') => {
            if let Some(track) = ctx.app.selected_track() {
                pins::toggle_in_place(&mut ctx.app.pinned_tracks, &track.uri);
                pins::save("tracks", &ctx.app.pinned_tracks);
            }
        }
        KeyCode::Esc | KeyCode::Left => ctx.app.nav.escape(),
        KeyCode::Up | KeyCode::Down => {
            let len = ctx.app.playlist_detail_display().len();
            if let Some(pd) = &mut ctx.app.playlist_detail {
                move_selection(&mut pd.selected, len, key.code);
            }
        }
        // Enter, not Right: it plays and jumps to Now Playing.
        KeyCode::Enter => play_from_selected(ctx),
        _ => return tracks::handle(ctx, key, true),
    }
    true
}

/// Loads the whole playlist as the playback context, starting at the selected
/// track, so `n`/`p` walk the real playlist. The index sent is the track's
/// position in the unfiltered playlist (which `filtered_sorted` carries), not
/// its display row.
fn play_from_selected(ctx: &mut KeyCtx<'_>) {
    let Some(pd) = &ctx.app.playlist_detail else { return };
    let Some(&(original_index, _)) = ctx.app.playlist_detail_display().get(pd.selected) else { return };
    let (uri, name) = (pd.playlist.uri.clone(), pd.playlist.name.clone());
    play_context(ctx.app, ctx.spirc, uri, Some(original_index as u32), name);
}

/// `Up`/`Down` relocate the track locally (no network call per keystroke),
/// `Enter` confirms with one `reorder_track` for the net displacement, `Esc`
/// walks it back to where it started, `g` jumps to a typed position.
fn handle_move_mode(ctx: &mut KeyCtx<'_>, code: KeyCode) {
    let Some(pd) = &mut ctx.app.playlist_detail else { return };
    match code {
        KeyCode::Up => {
            if let Fetch::Ready(items) = &mut pd.tracks {
                pd.selected = move_item_up(items, pd.selected);
            }
        }
        KeyCode::Down => {
            if let Fetch::Ready(items) = &mut pd.tracks {
                pd.selected = move_item_down(items, pd.selected);
            }
        }
        KeyCode::Enter => {
            if let Some(start) = pd.move_mode.take() {
                let (end, uri) = (pd.selected, pd.playlist.uri.clone());
                resume_normal_selection(ctx.app);
                if start != end {
                    ctx.svc.reorder_track(ctx.app, uri, start, end);
                }
            }
        }
        KeyCode::Esc => {
            if let Some(start) = pd.move_mode.take()
                && let Fetch::Ready(items) = &mut pd.tracks
            {
                pd.selected = move_item_to(items, pd.selected, start);
            }
            resume_normal_selection(ctx.app);
        }
        KeyCode::Char('g') => {
            let len = match &pd.tracks {
                Fetch::Ready(items) => items.len(),
                _ => 0,
            };
            if len > 1 {
                ctx.app.text_prompt = Some(TextPrompt::new(
                    format!("Move to position (1-{len})"),
                    "",
                    TextPromptAction::MoveToPosition,
                ));
            } else {
                ctx.app.status = Some(("only one track -- nowhere to move it".to_string(), true));
            }
        }
        _ => {}
    }
}

/// Move mode ends: converts `selected` from a raw array index back to its
/// row in the normal pinned-first display, so the highlight stays on the same
/// track when a pin is active.
fn resume_normal_selection(app: &mut AppState) {
    let Some(pd) = &app.playlist_detail else { return };
    let real_index = pd.selected;
    let row = app.playlist_detail_display().iter().position(|&(i, _)| i == real_index).unwrap_or(real_index);
    if let Some(pd) = &mut app.playlist_detail {
        pd.selected = row;
    }
}

/// Pins don't block move mode: it simply skips the pinned-first bubbling, so
/// display position equals array position whatever is pinned. A filter or
/// sort would also reorder the display, which move mode cannot undo, so
/// those must be off.
fn enter_move_mode(app: &mut AppState) {
    let refusal = match &app.playlist_detail {
        None => Some("no playlist open"),
        Some(pd) if pd.filter.editing || !pd.filter.query.is_empty() || pd.filter.sort_alpha => {
            Some("clear the filter and turn off sort to reorder")
        }
        Some(pd) if !matches!(pd.tracks, Fetch::Ready(_)) => Some("still loading"),
        Some(_) => None,
    };
    if let Some(reason) = refusal {
        app.status = Some((reason.to_string(), true));
        return;
    }
    // `selected` is a display row; move mode treats it as a raw index, so
    // convert first or a pin would make it move the wrong track.
    let Some(pd) = &app.playlist_detail else { return };
    let real_index = app.playlist_detail_display().get(pd.selected).map(|&(i, _)| i);
    if let (Some(real_index), Some(pd)) = (real_index, &mut app.playlist_detail) {
        pd.selected = real_index;
        pd.move_mode = Some(real_index);
    }
}

/// Spotify's remove endpoint can only remove every copy of a duplicated
/// track at once (a position-scoped call was tried and behaved
/// inconsistently), so the prompt says so rather than surprising the user
/// with a bigger deletion than they asked for.
fn confirm_remove_track(app: &mut AppState) {
    let (Some(track), Some(pd)) = (app.selected_track(), &app.playlist_detail) else { return };
    let Fetch::Ready(items) = &pd.tracks else { return };
    let occurrences = items.iter().filter(|t| t.uri == track.uri).count();
    let message = if occurrences > 1 {
        format!(
            "\"{} \u{2014} {}\" appears {occurrences} times in this playlist -- Spotify's API can only remove ALL copies at once, not a single one. Remove all {occurrences}? y/n",
            track.artist, track.title
        )
    } else {
        format!("Remove \"{} \u{2014} {}\" from this playlist? y/n", track.artist, track.title)
    };
    let action = ConfirmAction::RemoveTrack { playlist_uri: pd.playlist.uri.clone(), track_uri: track.uri, occurrences };
    app.pending_confirm = Some(PendingConfirm { message, action });
}
