//! Playlist mutation (Phase 5): create, rename, delete, add/remove a
//! track. Read paths (`your_playlists`, `playlist_tracks`) stay in
//! `api::library` -- this module is only the write side, split out
//! because "read a list" and "mutate one item" are different enough
//! concerns to earn their own file, matching the `api/` split's own
//! stated intent.
//!
//! Every function here returns `Result<_, String>`, not `ClientResult`,
//! for the same reason `api::library::playlist_tracks` does: converting
//! a caller-supplied URI via `PlaylistId`/`TrackId::from_id_or_uri` can
//! fail on bad input, a distinct failure mode `ClientError` has no
//! variant for, and every caller already stringifies errors immediately
//! anyway.

use rspotify::clients::OAuthClient;
use rspotify::model::{LibraryId, PlayableId, PlaylistId, TrackId};
use rspotify::prelude::Id;
use rspotify::AuthCodeSpotify;

use super::ensure_fresh;
use super::library::PlaylistSummary;

fn playlist_id(playlist_uri: &str) -> Result<PlaylistId<'_>, String> {
    PlaylistId::from_id_or_uri(playlist_uri).map_err(|e| e.to_string())
}

fn track_id(track_uri: &str) -> Result<TrackId<'_>, String> {
    TrackId::from_id_or_uri(track_uri).map_err(|e| e.to_string())
}

/// `user_playlist_create`'s user-id parameter is vestigial post-Feb-2026
/// (the request actually posts to `me/playlists`, ignoring it server-side)
/// but the method still requires a type-correct one -- `me()` is the same
/// call `spike.rs`'s own playlist-reorder spike already uses to get one.
pub async fn create_playlist(client: &AuthCodeSpotify, name: &str) -> Result<PlaylistSummary, String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before create_playlist failed, trying with existing token anyway: {e}");
    }
    let user_id = client.me().await.map_err(|e| e.to_string())?.id;
    // Private, non-collaborative, no description -- v1 exposes none of
    // those as options; a throwaway/working playlist shouldn't default
    // to public.
    let playlist = client
        .user_playlist_create(user_id, name, Some(false), Some(false), None)
        .await
        .map_err(|e| e.to_string())?;
    Ok(PlaylistSummary {
        uri: playlist.id.uri(),
        name: playlist.name,
        track_count: 0,
    })
}

/// Name-only -- no description/visibility editing in this phase.
///
/// `playlist_change_detail`'s parameter order is `(name, public,
/// description, collaborative)`, the *opposite* order of
/// `user_playlist_create`'s `(name, public, collaborative, description)`.
/// Both call sites are worth double-checking against rspotify's actual
/// signature before touching either -- transposing these compiles fine
/// (all four are `Option`) and silently sends the wrong thing.
pub async fn rename_playlist(client: &AuthCodeSpotify, playlist_uri: &str, new_name: &str) -> Result<(), String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before rename_playlist failed, trying with existing token anyway: {e}");
    }
    let id = playlist_id(playlist_uri)?;
    client
        .playlist_change_detail(id, Some(new_name), None, None, None)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// "Delete a playlist you own" is modeled by the Spotify Web API as
/// unfollowing it -- as of rspotify 0.16's Feb-2026-consolidation,
/// through the Library API (`library_remove`), not the now-deprecated
/// `playlist_unfollow`. Needs `user-library-modify`, already in `SCOPES`
/// for save/unsave-track -- no re-login required.
pub async fn delete_playlist(client: &AuthCodeSpotify, playlist_uri: &str) -> Result<(), String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before delete_playlist failed, trying with existing token anyway: {e}");
    }
    let id = playlist_id(playlist_uri)?;
    client
        .library_remove([LibraryId::Playlist(id)])
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Always appends (`position: None`) -- position-scoped insert is
/// supported by the underlying API but not exposed in this phase.
pub async fn add_track(client: &AuthCodeSpotify, playlist_uri: &str, track_uri: &str) -> Result<(), String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before add_track failed, trying with existing token anyway: {e}");
    }
    let playlist = playlist_id(playlist_uri)?;
    let track = track_id(track_uri)?;
    client
        .playlist_add_items(playlist, [PlayableId::Track(track)], None)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Removes every occurrence of `track_uri` in the playlist, not just the
/// selected position -- known, accepted limitation: a playlist with the
/// same track twice loses both copies. rspotify also exposes a
/// position-scoped variant (`playlist_remove_specific_occurrences_of_items`),
/// but current Spotify API docs for the Feb-2026-consolidated remove
/// endpoint don't confirm per-position removal is still honored post-
/// consolidation -- building a real mutation on an unproven call is worse
/// than a documented edge case. If this needs fixing, spike the
/// position-scoped call for real first, the same way Phase 0 spiked
/// reorder, before wiring it in.
pub async fn remove_track(client: &AuthCodeSpotify, playlist_uri: &str, track_uri: &str) -> Result<(), String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before remove_track failed, trying with existing token anyway: {e}");
    }
    let playlist = playlist_id(playlist_uri)?;
    let track = track_id(track_uri)?;
    client
        .playlist_remove_all_occurrences_of_items(playlist, [PlayableId::Track(track)], None)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// `insert_before`'s asymmetry, confirmed live against a real playlist in
/// Phase 0's spike (`spike.rs`): it is *not* simply `to_index`. Moving
/// down (`to_index > from_index`) needs `to_index + 1`; moving up needs
/// `to_index` with no `+1`. Passing the caller's intended target straight
/// through as `insert_before` is the off-by-one that failed on the first
/// spike attempt -- pulled out as its own pure function specifically so
/// that exact regression stays covered by a test, not just a comment.
fn insert_before_for_move(from_index: usize, to_index: usize) -> usize {
    if to_index > from_index {
        to_index + 1
    } else {
        to_index
    }
}

#[cfg(test)]
mod reorder_tests {
    use super::*;

    #[test]
    fn moving_down_needs_target_plus_one() {
        assert_eq!(insert_before_for_move(0, 2), 3);
    }

    #[test]
    fn moving_up_needs_target_with_no_plus_one() {
        assert_eq!(insert_before_for_move(2, 0), 0);
    }

    #[test]
    fn no_net_movement_is_a_harmless_identity_call() {
        assert_eq!(insert_before_for_move(1, 1), 1);
    }
}

/// Moves a single track (`range_length: Some(1)`) from `from_index` to
/// end up at `to_index`.
pub async fn reorder_track(
    client: &AuthCodeSpotify,
    playlist_uri: &str,
    from_index: usize,
    to_index: usize,
) -> Result<(), String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before reorder_track failed, trying with existing token anyway: {e}");
    }
    let id = playlist_id(playlist_uri)?;
    let insert_before = insert_before_for_move(from_index, to_index);
    client
        .playlist_reorder_items(id, Some(from_index as i32), Some(insert_before as i32), Some(1), None)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
