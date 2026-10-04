//! Result types sent back by `Services` tasks, and how each is applied to
//! `AppState`.

use super::Services;
use crate::api;
use crate::api::library::{FollowedArtist, PlaylistSummary, SavedAlbumSummary};
use crate::api::search::TrackResult;
use crate::state::{AppState, ConfirmAction, Fetch, PendingConfirm, Screen};
use std::collections::HashSet;

/// Reads. Each carries the data, or an error message to show in place of it.
pub enum LibraryFetchResult {
    LikedSongs(Result<Vec<TrackResult>, String>),
    SavedAlbums(Result<Vec<SavedAlbumSummary>, String>),
    FollowedArtists(Result<Vec<FollowedArtist>, String>),
    Playlists(Result<Vec<PlaylistSummary>, String>),
    PlaylistTracks {
        playlist_uri: String,
        result: Result<Vec<TrackResult>, String>,
    },
    Queue(Result<api::queue::QueueSummary, String>),
    Devices(Result<Vec<api::devices::DeviceSummary>, String>),
    ArtistDetail {
        artist_uri: String,
        result: Result<api::artist::ArtistDetail, String>,
    },
    AlbumDetail {
        album_uri: String,
        result: Result<api::album::AlbumDetail, String>,
    },
    /// The playing track's artist/album ids, fetched so Now Playing can open
    /// whichever page `view` asks for.
    NowPlayingTrackIds {
        result: Result<api::track::TrackIds, String>,
        view: TrackView,
    },
}

/// Which of a track's pages to open once its ids arrive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackView {
    Artist,
    Album,
}

/// Writes. Each carries which direction it went (liked vs. unliked, ...)
/// rather than inferring it from the result, so the status message and the
/// list refetch are right even on failure.
pub enum CrudResult {
    PlaylistCreated(Result<PlaylistSummary, String>),
    PlaylistRenamed {
        playlist_uri: String,
        new_name: String,
        result: Result<(), String>,
    },
    PlaylistDeleted {
        playlist_uri: String,
        result: Result<(), String>,
    },
    TrackAdded {
        playlist_uri: String,
        track_uri: String,
        result: Result<(), String>,
    },
    /// The target playlist already holds the track (checked live before
    /// adding); `message` is the ready-made confirmation prompt.
    PlaylistAlreadyHasTrack {
        playlist_uri: String,
        track_uri: String,
        message: String,
    },
    TrackRemoved {
        playlist_uri: String,
        track_uri: String,
        occurrences: usize,
        result: Result<(), String>,
    },
    TrackReordered {
        playlist_uri: String,
        result: Result<(), String>,
    },
    DeviceTransferred(Result<(), String>),
    LikeToggled {
        track_uri: String,
        liked: bool,
        result: Result<(), String>,
    },
    FollowToggled {
        artist_uri: String,
        followed: bool,
        result: Result<(), String>,
    },
    SaveToggled {
        album_uri: String,
        saved: bool,
        result: Result<(), String>,
    },
    QueueAdded {
        track_uri: String,
        result: Result<(), String>,
    },
}

fn into_fetch<T>(result: Result<T, String>) -> Fetch<T> {
    result.map_or_else(Fetch::Failed, Fetch::Ready)
}

fn set_status(app: &mut AppState, message: impl Into<String>, is_error: bool) {
    app.status = Some((message.into(), is_error));
}

/// Adjusts a playlist's `track_count` by `delta` in the library list and in
/// the open Playlist Detail. Applied locally after a successful add/remove
/// because Spotify's `/me/playlists` often still returns the old count right
/// after a write, so a refetch would show a stale number.
fn bump_track_count(app: &mut AppState, playlist_uri: &str, delta: i64) {
    let bump = |count: &mut u32| *count = (*count as i64 + delta).max(0) as u32;
    if let Fetch::Ready(items) = &mut app.library.playlists
        && let Some(p) = items.iter_mut().find(|p| p.uri == playlist_uri)
    {
        bump(&mut p.track_count);
    }
    if let Some(pd) = &mut app.playlist_detail
        && pd.playlist.uri == playlist_uri
    {
        bump(&mut pd.playlist.track_count);
    }
}

impl Services {
    pub fn apply_library_result(&self, app: &mut AppState, result: LibraryFetchResult) {
        match result {
            LibraryFetchResult::LikedSongs(r) => app.library.liked_songs = into_fetch(r),
            LibraryFetchResult::SavedAlbums(r) => app.library.saved_albums = into_fetch(r),
            LibraryFetchResult::FollowedArtists(r) => app.library.followed_artists = into_fetch(r),
            LibraryFetchResult::Playlists(r) => app.library.playlists = into_fetch(r),
            LibraryFetchResult::PlaylistTracks {
                playlist_uri,
                result,
            } => {
                // The membership cache is correct for the URI it was fetched
                // under even if the user has since left that playlist.
                if let Ok(tracks) = &result {
                    app.playlist_membership.insert(
                        playlist_uri.clone(),
                        tracks.iter().map(|t| t.uri.clone()).collect(),
                    );
                }
                if let Some(pd) = app
                    .playlist_detail
                    .as_mut()
                    .filter(|pd| pd.playlist.uri == playlist_uri)
                {
                    pd.tracks = into_fetch(result);
                }
            }
            LibraryFetchResult::Queue(r) => app.queue.fetch = into_fetch(r),
            LibraryFetchResult::Devices(r) => app.devices.fetch = into_fetch(r),
            LibraryFetchResult::ArtistDetail { artist_uri, result } => {
                if let Some(state) = app
                    .artist_detail
                    .as_mut()
                    .filter(|s| s.artist_uri == artist_uri)
                {
                    state.detail = into_fetch(result);
                }
            }
            LibraryFetchResult::AlbumDetail { album_uri, result } => {
                if let Some(state) = app
                    .album_detail
                    .as_mut()
                    .filter(|s| s.album_uri == album_uri)
                {
                    state.detail = into_fetch(result);
                }
            }
            LibraryFetchResult::NowPlayingTrackIds { result, view } => match result {
                Ok(ids) => match view {
                    TrackView::Artist => self.open_artist_detail(app, ids.artist_uri),
                    TrackView::Album => self.open_album_detail(app, ids.album_uri),
                },
                Err(e) => set_status(app, format!("couldn't look up this track: {e}"), true),
            },
        }
    }

    pub fn apply_crud_result(&self, app: &mut AppState, result: CrudResult) {
        let detail_is_open = |app: &AppState, uri: &str| {
            app.playlist_detail
                .as_ref()
                .is_some_and(|pd| pd.playlist.uri == uri)
        };
        match result {
            CrudResult::PlaylistCreated(Ok(summary)) => {
                set_status(app, "playlist created", false);
                // A new playlist provably holds nothing.
                app.playlist_membership.insert(summary.uri, HashSet::new());
                self.refetch_playlists(app);
            }
            CrudResult::PlaylistCreated(Err(e)) => {
                set_status(app, format!("create playlist failed: {e}"), true)
            }

            CrudResult::PlaylistRenamed {
                playlist_uri,
                new_name,
                result: Ok(()),
            } => {
                set_status(app, "playlist renamed", false);
                // Playlist Detail keeps its own copy of the playlist, which
                // refetching the list does not touch.
                if let Some(pd) = app
                    .playlist_detail
                    .as_mut()
                    .filter(|pd| pd.playlist.uri == playlist_uri)
                {
                    pd.playlist.name = new_name;
                }
                self.refetch_playlists(app);
            }
            CrudResult::PlaylistRenamed { result: Err(e), .. } => {
                set_status(app, format!("rename failed: {e}"), true)
            }

            CrudResult::PlaylistDeleted {
                playlist_uri,
                result: Ok(()),
            } => {
                set_status(app, "playlist deleted", false);
                app.playlist_membership.remove(&playlist_uri);
                if detail_is_open(app, &playlist_uri) {
                    app.playlist_detail = None;
                    app.nav.goto(Screen::YourPlaylists);
                }
                self.refetch_playlists(app);
            }
            CrudResult::PlaylistDeleted { result: Err(e), .. } => {
                set_status(app, format!("delete failed: {e}"), true)
            }

            CrudResult::TrackAdded {
                playlist_uri,
                track_uri,
                result: Ok(()),
            } => {
                set_status(app, "added to playlist", false);
                bump_track_count(app, &playlist_uri, 1);
                // Write through to the membership cache so the picker shows
                // the new membership immediately.
                app.playlist_membership
                    .entry(playlist_uri.clone())
                    .or_default()
                    .insert(track_uri);
                if detail_is_open(app, &playlist_uri) {
                    self.refetch_playlist_tracks(app, playlist_uri);
                }
            }
            CrudResult::TrackAdded { result: Err(e), .. } => {
                set_status(app, format!("add to playlist failed: {e}"), true)
            }

            CrudResult::PlaylistAlreadyHasTrack {
                playlist_uri,
                track_uri,
                message,
            } => {
                app.pending_confirm = Some(PendingConfirm {
                    message,
                    action: ConfirmAction::AddTrackAnyway {
                        playlist_uri,
                        track_uri,
                    },
                });
            }

            CrudResult::TrackRemoved {
                playlist_uri,
                track_uri,
                occurrences,
                result: Ok(()),
            } => {
                set_status(app, "removed from playlist", false);
                // Spotify removes every copy in one call, so the count can
                // drop by more than one.
                bump_track_count(app, &playlist_uri, -(occurrences as i64));
                if let Some(set) = app.playlist_membership.get_mut(&playlist_uri) {
                    set.remove(&track_uri);
                }
                if detail_is_open(app, &playlist_uri) {
                    self.refetch_playlist_tracks(app, playlist_uri);
                }
            }
            CrudResult::TrackRemoved { result: Err(e), .. } => {
                set_status(app, format!("remove track failed: {e}"), true)
            }

            // Move mode already reordered the local list optimistically, so a
            // refetch reconciles with the server on success and undoes the
            // move on failure.
            CrudResult::TrackReordered {
                playlist_uri,
                result,
            } => {
                match result {
                    Ok(()) => set_status(app, "reordered", false),
                    Err(e) => set_status(app, format!("reorder failed: {e}"), true),
                }
                if detail_is_open(app, &playlist_uri) {
                    self.refetch_playlist_tracks(app, playlist_uri);
                }
            }

            CrudResult::DeviceTransferred(Ok(())) => {
                set_status(app, "playback transferred", false);
                // Moves the active-device marker.
                self.refetch_devices(app);
            }
            CrudResult::DeviceTransferred(Err(e)) => {
                set_status(app, format!("transfer failed: {e}"), true)
            }

            CrudResult::QueueAdded {
                result: Ok(()),
                track_uri,
            } => {
                log::info!("add_to_queue[{track_uri}]: ok");
                set_status(app, "added to queue", false);
            }
            CrudResult::QueueAdded {
                result: Err(e),
                track_uri,
            } => {
                log::warn!("add_to_queue[{track_uri}]: failed: {e}");
                set_status(app, format!("couldn't add to queue: {e}"), true);
            }

            CrudResult::LikeToggled {
                result: Ok(()),
                liked,
                track_uri,
            } => {
                log::info!("like_track[{track_uri}]: liked={liked}");
                set_status(app, if liked { "liked" } else { "unliked" }, false);
                self.refetch_liked_songs(app);
            }
            CrudResult::LikeToggled {
                result: Err(e),
                liked,
                track_uri,
            } => {
                let verb = if liked { "like" } else { "unlike" };
                log::warn!("like_track[{track_uri}]: {verb} failed: {e}");
                set_status(app, format!("{verb} failed: {e}"), true);
            }

            CrudResult::FollowToggled {
                result: Ok(()),
                followed,
                artist_uri,
            } => {
                log::info!("follow_artist[{artist_uri}]: followed={followed}");
                set_status(app, if followed { "followed" } else { "unfollowed" }, false);
                self.refetch_followed_artists(app);
            }
            CrudResult::FollowToggled {
                result: Err(e),
                followed,
                artist_uri,
            } => {
                let verb = if followed { "follow" } else { "unfollow" };
                log::warn!("follow_artist[{artist_uri}]: {verb} failed: {e}");
                set_status(app, format!("{verb} failed: {e}"), true);
            }

            CrudResult::SaveToggled {
                result: Ok(()),
                saved,
                album_uri,
            } => {
                log::info!("save_album[{album_uri}]: saved={saved}");
                set_status(app, if saved { "saved" } else { "unsaved" }, false);
                self.refetch_saved_albums(app);
            }
            CrudResult::SaveToggled {
                result: Err(e),
                saved,
                album_uri,
            } => {
                let verb = if saved { "save" } else { "unsave" };
                log::warn!("save_album[{album_uri}]: {verb} failed: {e}");
                set_status(app, format!("{verb} failed: {e}"), true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ListFilter;
    use crate::state::PlaylistDetailState;

    fn playlist(uri: &str, count: u32) -> PlaylistSummary {
        PlaylistSummary {
            uri: uri.to_string(),
            name: format!("name of {uri}"),
            track_count: count,
        }
    }

    fn track(uri: &str) -> TrackResult {
        TrackResult {
            uri: uri.to_string(),
            title: "t".into(),
            artist: "a".into(),
            album: "al".into(),
            artist_uri: String::new(),
            album_uri: String::new(),
        }
    }

    fn app_with_open_playlist(uri: &str, count: u32) -> AppState {
        let mut app = AppState::new(false, HashSet::new(), HashSet::new(), 0);
        app.library.playlists = Fetch::Ready(vec![playlist(uri, count)]);
        app.playlist_detail = Some(PlaylistDetailState {
            playlist: playlist(uri, count),
            tracks: Fetch::Ready(vec![track("spotify:track:1")]),
            selected: 0,
            filter: ListFilter::default(),
            move_mode: None,
        });
        app
    }

    fn count_in_list(app: &AppState) -> u32 {
        match &app.library.playlists {
            Fetch::Ready(items) => items[0].track_count,
            _ => panic!("playlists not ready"),
        }
    }

    #[test]
    fn adding_a_track_bumps_both_copies_of_the_count_and_the_membership_cache() {
        let (svc, _rx) = Services::new();
        let mut app = app_with_open_playlist("spotify:playlist:p", 10);
        svc.apply_crud_result(
            &mut app,
            CrudResult::TrackAdded {
                playlist_uri: "spotify:playlist:p".into(),
                track_uri: "spotify:track:9".into(),
                result: Ok(()),
            },
        );
        assert_eq!(count_in_list(&app), 11);
        assert_eq!(
            app.playlist_detail.as_ref().unwrap().playlist.track_count,
            11
        );
        assert!(app.playlist_membership["spotify:playlist:p"].contains("spotify:track:9"));
        assert_eq!(app.status, Some(("added to playlist".to_string(), false)));
    }

    #[test]
    fn removing_every_copy_drops_the_count_by_the_number_of_copies() {
        let (svc, _rx) = Services::new();
        let mut app = app_with_open_playlist("spotify:playlist:p", 10);
        svc.apply_crud_result(
            &mut app,
            CrudResult::TrackRemoved {
                playlist_uri: "spotify:playlist:p".into(),
                track_uri: "spotify:track:1".into(),
                occurrences: 3,
                result: Ok(()),
            },
        );
        assert_eq!(count_in_list(&app), 7);
    }

    #[test]
    fn the_count_never_goes_below_zero() {
        let mut app = app_with_open_playlist("spotify:playlist:p", 1);
        bump_track_count(&mut app, "spotify:playlist:p", -5);
        assert_eq!(count_in_list(&app), 0);
    }

    #[test]
    fn a_failed_write_reports_the_error_and_changes_nothing_else() {
        let (svc, _rx) = Services::new();
        let mut app = app_with_open_playlist("spotify:playlist:p", 10);
        svc.apply_crud_result(
            &mut app,
            CrudResult::TrackAdded {
                playlist_uri: "spotify:playlist:p".into(),
                track_uri: "spotify:track:9".into(),
                result: Err("boom".into()),
            },
        );
        assert_eq!(count_in_list(&app), 10);
        assert_eq!(
            app.status,
            Some(("add to playlist failed: boom".to_string(), true))
        );
    }

    #[test]
    fn deleting_the_open_playlist_backs_out_of_its_detail_screen() {
        let (svc, _rx) = Services::new();
        let mut app = app_with_open_playlist("spotify:playlist:p", 10);
        app.nav.push(Screen::PlaylistDetail);
        svc.apply_crud_result(
            &mut app,
            CrudResult::PlaylistDeleted {
                playlist_uri: "spotify:playlist:p".into(),
                result: Ok(()),
            },
        );
        assert!(app.playlist_detail.is_none());
        assert_eq!(*app.nav.top(), Screen::YourPlaylists);
    }

    #[test]
    fn a_duplicate_add_asks_for_confirmation_instead_of_adding() {
        let (svc, _rx) = Services::new();
        let mut app = app_with_open_playlist("spotify:playlist:p", 10);
        svc.apply_crud_result(
            &mut app,
            CrudResult::PlaylistAlreadyHasTrack {
                playlist_uri: "spotify:playlist:p".into(),
                track_uri: "spotify:track:1".into(),
                message: "again?".into(),
            },
        );
        let confirm = app.pending_confirm.expect("a confirmation");
        assert_eq!(confirm.message, "again?");
        assert!(matches!(
            confirm.action,
            ConfirmAction::AddTrackAnyway { .. }
        ));
    }

    #[test]
    fn a_stale_track_list_does_not_overwrite_the_playlist_now_showing() {
        let (svc, _rx) = Services::new();
        let mut app = app_with_open_playlist("spotify:playlist:p", 10);
        svc.apply_library_result(
            &mut app,
            LibraryFetchResult::PlaylistTracks {
                playlist_uri: "spotify:playlist:other".into(),
                result: Ok(vec![track("spotify:track:a"), track("spotify:track:b")]),
            },
        );
        let Fetch::Ready(shown) = &app.playlist_detail.as_ref().unwrap().tracks else {
            panic!("not ready")
        };
        assert_eq!(shown.len(), 1);
        // The data is still correct for the playlist it was fetched under.
        assert_eq!(app.playlist_membership["spotify:playlist:other"].len(), 2);
    }

    #[test]
    fn a_fetch_error_becomes_a_failed_state() {
        let (svc, _rx) = Services::new();
        let mut app = AppState::new(false, HashSet::new(), HashSet::new(), 0);
        svc.apply_library_result(&mut app, LibraryFetchResult::LikedSongs(Err("nope".into())));
        assert!(matches!(&app.library.liked_songs, Fetch::Failed(e) if e == "nope"));
    }

    #[test]
    fn reads_without_a_client_fail_visibly_instead_of_loading_forever() {
        let (svc, _rx) = Services::new();
        let mut app = AppState::new(false, HashSet::new(), HashSet::new(), 0);
        svc.refetch_playlists(&mut app);
        assert!(matches!(&app.library.playlists, Fetch::Failed(_)));
    }
}
