// Playlist writes: create, rename, delete, add and remove a track, reorder.
// Reads live in `api::library`.
//
// Functions return `Result<_, String>` because converting a caller-supplied URI
// via `PlaylistId`/`TrackId::from_id_or_uri` can fail on bad input, which
// `ClientError` has no variant for.

use rspotify::AuthCodeSpotify;
use rspotify::clients::OAuthClient;
use rspotify::model::{LibraryId, PlayableId, PlaylistId, TrackId};
use rspotify::prelude::Id;

use super::ensure_fresh;
use super::library::PlaylistSummary;

fn playlist_id(playlist_uri: &str) -> Result<PlaylistId<'_>, String> {
    PlaylistId::from_id_or_uri(playlist_uri).map_err(|e| e.to_string())
}

fn track_id(track_uri: &str) -> Result<TrackId<'_>, String> {
    TrackId::from_id_or_uri(track_uri).map_err(|e| e.to_string())
}

/// `user_playlist_create`'s user-id parameter is vestigial since Feb 2026 (the
/// request posts to `me/playlists`) but still has to be type-correct, hence `me()`.
pub async fn create_playlist(
    client: &AuthCodeSpotify,
    name: &str,
) -> Result<PlaylistSummary, String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!(
            "token refresh before create_playlist failed, trying with existing token anyway: {e}"
        );
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

/// Name only. Note `playlist_change_detail` takes `(name, public, description,
/// collaborative)`, the opposite order of `user_playlist_create`'s
/// `(name, public, collaborative, description)`; transposing them compiles (all
/// `Option`) and silently sends the wrong thing.
pub async fn rename_playlist(
    client: &AuthCodeSpotify,
    playlist_uri: &str,
    new_name: &str,
) -> Result<(), String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!(
            "token refresh before rename_playlist failed, trying with existing token anyway: {e}"
        );
    }
    let id = playlist_id(playlist_uri)?;
    client
        .playlist_change_detail(id, Some(new_name), None, None, None)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Deleting a playlist you own is unfollowing it, which since Feb 2026 goes
/// through the Library API (`library_remove`) rather than the deprecated
/// `playlist_unfollow`. Needs `user-library-modify`, already in `SCOPES`.
pub async fn delete_playlist(client: &AuthCodeSpotify, playlist_uri: &str) -> Result<(), String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!(
            "token refresh before delete_playlist failed, trying with existing token anyway: {e}"
        );
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
pub async fn add_track(
    client: &AuthCodeSpotify,
    playlist_uri: &str,
    track_uri: &str,
) -> Result<(), String> {
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

/// Removes every occurrence of `track_uri`, not just the selected one: a platform
/// limitation. rspotify's position-scoped variant
/// (`playlist_remove_specific_occurrences_of_items`) was tried against a real
/// playlist holding a track twice and was worse: without a `snapshot_id` it
/// removed both copies anyway, and with one it removed neither (a silent no-op).
/// Non-deterministic behaviour on a destructive call is no alternative, so the
/// Playlist Detail `d` handler warns the user when the track has duplicates.
pub async fn remove_track(
    client: &AuthCodeSpotify,
    playlist_uri: &str,
    track_uri: &str,
) -> Result<(), String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!(
            "token refresh before remove_track failed, trying with existing token anyway: {e}"
        );
    }
    let playlist = playlist_id(playlist_uri)?;
    let track = track_id(track_uri)?;
    client
        .playlist_remove_all_occurrences_of_items(playlist, [PlayableId::Track(track)], None)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// `insert_before` is not simply `to_index`: moving down needs `to_index + 1`,
/// moving up needs `to_index`. Passing the target straight through is an
/// off-by-one. A pure function so the regression stays under test.
fn insert_before_for_move(from_index: usize, to_index: usize) -> usize {
    if to_index > from_index {
        to_index + 1
    } else {
        to_index
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
        log::warn!(
            "token refresh before reorder_track failed, trying with existing token anyway: {e}"
        );
    }
    let id = playlist_id(playlist_uri)?;
    let insert_before = insert_before_for_move(from_index, to_index);
    client
        .playlist_reorder_items(
            id,
            Some(from_index as i32),
            Some(insert_before as i32),
            Some(1),
            None,
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
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
