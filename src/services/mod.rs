//! Spotify Web API work that runs off the UI thread.
//!
//! Key handlers call a method on [`Services`], which spawns a task and
//! returns immediately. The task reports back over a channel as a
//! [`LibraryFetchResult`] (reads) or [`CrudResult`] (writes), and the main
//! loop drains those each frame through `Services::apply_*`. Every result
//! that targets one entity carries its URI, so a response that arrives after
//! the user has moved on can be recognised as stale and dropped.

mod results;

pub use results::{CrudResult, LibraryFetchResult, TrackView};

use crate::api;
use crate::api::library::PlaylistSummary;
use crate::api::search::TrackResult;
use crate::state::{AlbumDetailState, AppState, ArtistDetailState, Fetch, ListFilter, PlaylistDetailState, Screen};
use rspotify::AuthCodeSpotify;
use std::future::Future;
use std::sync::mpsc::{self, Receiver, Sender};

const NOT_READY: &str = "Spotify client not ready yet";

pub type SearchResult = Result<Vec<TrackResult>, String>;

/// Handle for starting Web API work: the client (once its token has loaded)
/// plus the sending half of each result channel.
pub struct Services {
    pub client: Option<AuthCodeSpotify>,
    library_tx: Sender<LibraryFetchResult>,
    crud_tx: Sender<CrudResult>,
    search_tx: Sender<SearchResult>,
}

/// The receiving halves, drained by the main loop.
pub struct ServiceReceivers {
    pub library: Receiver<LibraryFetchResult>,
    pub crud: Receiver<CrudResult>,
    pub search: Receiver<SearchResult>,
}

fn spawn_send<T, M>(tx: &Sender<M>, work: impl Future<Output = T> + Send + 'static, wrap: impl FnOnce(T) -> M + Send + 'static)
where
    T: Send + 'static,
    M: Send + 'static,
{
    let tx = tx.clone();
    tokio::spawn(async move {
        let _ = tx.send(wrap(work.await));
    });
}

impl Services {
    pub fn new() -> (Self, ServiceReceivers) {
        let (library_tx, library) = mpsc::channel();
        let (crud_tx, crud) = mpsc::channel();
        let (search_tx, search) = mpsc::channel();
        (Self { client: None, library_tx, crud_tx, search_tx }, ServiceReceivers { library, crud, search })
    }

    /// The client, or a "not ready" status message for the user.
    fn client_or_status(&self, app: &mut AppState) -> Option<AuthCodeSpotify> {
        if self.client.is_none() {
            app.status = Some((NOT_READY.to_string(), true));
        }
        self.client.clone()
    }

    /// Runs `job` against the client and delivers its result on the CRUD
    /// channel. `status` is shown while it is in flight.
    fn mutate<Fut>(&self, app: &mut AppState, status: Option<&str>, job: impl FnOnce(AuthCodeSpotify) -> Fut)
    where
        Fut: Future<Output = CrudResult> + Send + 'static,
    {
        let Some(client) = self.client_or_status(app) else { return };
        if let Some(status) = status {
            app.status = Some((format!("{status}\u{2026}"), false));
        }
        spawn_send(&self.crud_tx, job(client), |result| result);
    }

    /// Starts a library read into `slot`, or marks it failed when there is no
    /// client yet.
    fn load<T, Fut>(
        &self,
        slot: &mut Fetch<T>,
        call: impl FnOnce(AuthCodeSpotify) -> Fut,
        wrap: impl FnOnce(Result<T, String>) -> LibraryFetchResult + Send + 'static,
    ) where
        T: Send + 'static,
        Fut: Future<Output = Result<T, String>> + Send + 'static,
    {
        match self.client.clone() {
            Some(client) => {
                *slot = Fetch::Loading;
                spawn_send(&self.library_tx, call(client), wrap);
            }
            None => *slot = Fetch::Failed(NOT_READY.into()),
        }
    }

    // -- reads ------------------------------------------------------------

    pub fn refetch_playlists(&self, app: &mut AppState) {
        self.load(
            &mut app.library.playlists,
            |c| async move { api::library::your_playlists(&c).await.map_err(|e| e.to_string()) },
            LibraryFetchResult::Playlists,
        );
    }

    pub fn refetch_liked_songs(&self, app: &mut AppState) {
        self.load(
            &mut app.library.liked_songs,
            |c| async move { api::library::liked_songs(&c).await.map_err(|e| e.to_string()) },
            LibraryFetchResult::LikedSongs,
        );
    }

    pub fn refetch_saved_albums(&self, app: &mut AppState) {
        self.load(
            &mut app.library.saved_albums,
            |c| async move { api::library::saved_albums(&c).await.map_err(|e| e.to_string()) },
            LibraryFetchResult::SavedAlbums,
        );
    }

    pub fn refetch_followed_artists(&self, app: &mut AppState) {
        self.load(
            &mut app.library.followed_artists,
            |c| async move { api::library::followed_artists(&c).await.map_err(|e| e.to_string()) },
            LibraryFetchResult::FollowedArtists,
        );
    }

    pub fn refetch_devices(&self, app: &mut AppState) {
        self.load(
            &mut app.devices.fetch,
            |c| async move { api::devices::list_devices(&c).await },
            LibraryFetchResult::Devices,
        );
    }

    /// Unlike the lists above, the queue changes on its own (a track ends,
    /// another device skips), so the main loop polls this while it is on
    /// screen. It never shows a loading state, to avoid flicker.
    pub fn refetch_queue(&self) {
        if let Some(client) = self.client.clone() {
            spawn_send(&self.library_tx, async move { api::queue::current_queue(&client).await }, LibraryFetchResult::Queue);
        }
    }

    /// Fetches whatever a list screen needs that has not been loaded yet.
    pub fn ensure_loaded(&self, app: &mut AppState, screen: Screen) {
        match screen {
            Screen::LikedSongs if matches!(app.library.liked_songs, Fetch::NotStarted) => self.refetch_liked_songs(app),
            Screen::SavedAlbums if matches!(app.library.saved_albums, Fetch::NotStarted) => self.refetch_saved_albums(app),
            Screen::FollowedArtists if matches!(app.library.followed_artists, Fetch::NotStarted) => {
                self.refetch_followed_artists(app)
            }
            Screen::YourPlaylists if matches!(app.library.playlists, Fetch::NotStarted) => self.refetch_playlists(app),
            Screen::Devices if matches!(app.devices.fetch, Fetch::NotStarted) => self.refetch_devices(app),
            _ => {}
        }
    }

    pub fn refetch_playlist_tracks(&self, app: &mut AppState, playlist_uri: String) {
        let Some(pd) = app.playlist_detail.as_mut().filter(|pd| pd.playlist.uri == playlist_uri) else {
            // Not open: nothing on screen to mark loading, but still warm
            // the membership cache.
            self.spawn_playlist_tracks(playlist_uri);
            return;
        };
        match self.client {
            Some(_) => {
                pd.tracks = Fetch::Loading;
                self.spawn_playlist_tracks(playlist_uri);
            }
            None => pd.tracks = Fetch::Failed(NOT_READY.into()),
        }
    }

    fn spawn_playlist_tracks(&self, playlist_uri: String) {
        let Some(client) = self.client.clone() else { return };
        spawn_send(
            &self.library_tx,
            {
                let uri = playlist_uri.clone();
                async move { api::library::playlist_tracks(&client, &uri).await }
            },
            move |result| LibraryFetchResult::PlaylistTracks { playlist_uri, result },
        );
    }

    pub fn open_playlist_detail(&self, app: &mut AppState, playlist: PlaylistSummary) {
        app.nav.push(Screen::PlaylistDetail);
        let uri = playlist.uri.clone();
        app.playlist_detail = Some(PlaylistDetailState {
            playlist,
            tracks: Fetch::Loading,
            selected: 0,
            filter: ListFilter::default(),
            move_mode: None,
        });
        self.refetch_playlist_tracks(app, uri);
    }

    pub fn open_artist_detail(&self, app: &mut AppState, artist_uri: String) {
        if artist_uri.is_empty() {
            app.status = Some(("no artist info for this track".to_string(), true));
            return;
        }
        app.nav.push(Screen::ArtistDetail);
        let mut state = ArtistDetailState { artist_uri: artist_uri.clone(), detail: Fetch::Loading, selected: 0 };
        match self.client.clone() {
            Some(client) => spawn_send(
                &self.library_tx,
                {
                    let uri = artist_uri.clone();
                    async move { api::artist::get_artist_detail(&client, &uri).await }
                },
                move |result| LibraryFetchResult::ArtistDetail { artist_uri, result },
            ),
            None => state.detail = Fetch::Failed(NOT_READY.into()),
        }
        app.artist_detail = Some(state);
    }

    pub fn open_album_detail(&self, app: &mut AppState, album_uri: String) {
        if album_uri.is_empty() {
            app.status = Some(("no album info for this track".to_string(), true));
            return;
        }
        app.nav.push(Screen::AlbumDetail);
        let mut state = AlbumDetailState { album_uri: album_uri.clone(), detail: Fetch::Loading, selected: 0 };
        match self.client.clone() {
            Some(client) => spawn_send(
                &self.library_tx,
                {
                    let uri = album_uri.clone();
                    async move { api::album::get_album_detail(&client, &uri).await }
                },
                move |result| LibraryFetchResult::AlbumDetail { album_uri, result },
            ),
            None => state.detail = Fetch::Failed(NOT_READY.into()),
        }
        app.album_detail = Some(state);
    }

    /// Looks up a playing track's artist/album, then opens `view`. Used by
    /// Now Playing, which only has the track's own URI.
    pub fn open_track_view(&self, app: &mut AppState, track_uri: &str, view: TrackView) {
        let Some(client) = self.client_or_status(app) else { return };
        let uri = track_uri.to_string();
        spawn_send(
            &self.library_tx,
            async move { api::track::get_track_ids(&client, &uri).await },
            move |result| LibraryFetchResult::NowPlayingTrackIds { result, view },
        );
    }

    pub fn search(&self, app: &mut AppState, query: String) {
        let Some(client) = self.client_or_status(app) else { return };
        app.search.searching = true;
        app.search.error = None;
        // Dev Mode apps cap search at 10 results; anything higher is a 400.
        spawn_send(&self.search_tx, async move { api::search::search_tracks(&client, &query, 10).await.map_err(|e| e.to_string()) }, |r| r);
    }

    // -- writes -----------------------------------------------------------

    pub fn like_track(&self, app: &mut AppState, track_uri: String) {
        self.mutate(app, None, |c| async move {
            let result = api::library::like_track(&c, &track_uri).await;
            CrudResult::LikeToggled { track_uri, liked: true, result }
        });
    }

    pub fn unlike_track(&self, app: &mut AppState, track_uri: String) {
        self.mutate(app, Some("unliking"), |c| async move {
            let result = api::library::unlike_track(&c, &track_uri).await;
            CrudResult::LikeToggled { track_uri, liked: false, result }
        });
    }

    pub fn follow_artist(&self, app: &mut AppState, artist_uri: String) {
        self.mutate(app, None, |c| async move {
            let result = api::library::follow_artist(&c, &artist_uri).await;
            CrudResult::FollowToggled { artist_uri, followed: true, result }
        });
    }

    pub fn unfollow_artist(&self, app: &mut AppState, artist_uri: String) {
        self.mutate(app, Some("unfollowing"), |c| async move {
            let result = api::library::unfollow_artist(&c, &artist_uri).await;
            CrudResult::FollowToggled { artist_uri, followed: false, result }
        });
    }

    pub fn save_album(&self, app: &mut AppState, album_uri: String) {
        self.mutate(app, None, |c| async move {
            let result = api::library::save_album(&c, &album_uri).await;
            CrudResult::SaveToggled { album_uri, saved: true, result }
        });
    }

    pub fn unsave_album(&self, app: &mut AppState, album_uri: String) {
        self.mutate(app, Some("unsaving"), |c| async move {
            let result = api::library::unsave_album(&c, &album_uri).await;
            CrudResult::SaveToggled { album_uri, saved: false, result }
        });
    }

    pub fn create_playlist(&self, app: &mut AppState, name: String) {
        self.mutate(app, Some("creating playlist"), |c| async move {
            CrudResult::PlaylistCreated(api::playlists::create_playlist(&c, &name).await)
        });
    }

    pub fn rename_playlist(&self, app: &mut AppState, playlist_uri: String, new_name: String) {
        self.mutate(app, Some("renaming playlist"), |c| async move {
            let result = api::playlists::rename_playlist(&c, &playlist_uri, &new_name).await;
            CrudResult::PlaylistRenamed { playlist_uri, new_name, result }
        });
    }

    pub fn delete_playlist(&self, app: &mut AppState, playlist: PlaylistSummary) {
        self.mutate(app, Some(&format!("deleting \"{}\"", playlist.name)), |c| async move {
            let playlist_uri = playlist.uri;
            let result = api::playlists::delete_playlist(&c, &playlist_uri).await;
            CrudResult::PlaylistDeleted { playlist_uri, result }
        });
    }

    pub fn add_track_to_playlist(&self, app: &mut AppState, playlist_uri: String, track_uri: String) {
        self.mutate(app, Some("adding to playlist"), |c| async move {
            let result = api::playlists::add_track(&c, &playlist_uri, &track_uri).await;
            CrudResult::TrackAdded { playlist_uri, track_uri, result }
        });
    }

    /// Adds to a playlist, but first checks whether it already holds the
    /// track and, if so, asks for confirmation instead.
    ///
    /// Spotify has no "does this playlist contain X" endpoint, so the check
    /// fetches the whole playlist (which also refreshes the membership
    /// cache). If that fetch fails the add goes ahead unchecked.
    pub fn add_track_checked(&self, app: &mut AppState, playlist_uri: String, playlist_name: String, track_uri: String) {
        let library_tx = self.library_tx.clone();
        self.mutate(app, Some("adding to playlist"), |c| async move {
            match api::library::playlist_tracks(&c, &playlist_uri).await {
                Ok(tracks) => {
                    let existing = tracks.iter().find(|t| t.uri == track_uri).cloned();
                    let _ = library_tx
                        .send(LibraryFetchResult::PlaylistTracks { playlist_uri: playlist_uri.clone(), result: Ok(tracks) });
                    if let Some(existing) = existing {
                        let message = format!(
                            "\"{} \u{2014} {}\" is already in \"{playlist_name}\". Add it again anyway? y/n",
                            existing.artist, existing.title
                        );
                        return CrudResult::PlaylistAlreadyHasTrack { playlist_uri, track_uri, message };
                    }
                }
                Err(e) => log::warn!("duplicate check before add_track failed, adding anyway: {e}"),
            }
            let result = api::playlists::add_track(&c, &playlist_uri, &track_uri).await;
            CrudResult::TrackAdded { playlist_uri, track_uri, result }
        });
    }

    pub fn remove_track(&self, app: &mut AppState, playlist_uri: String, track_uri: String, occurrences: usize) {
        self.mutate(app, Some("removing track"), |c| async move {
            let result = api::playlists::remove_track(&c, &playlist_uri, &track_uri).await;
            CrudResult::TrackRemoved { playlist_uri, track_uri, occurrences, result }
        });
    }

    pub fn reorder_track(&self, app: &mut AppState, playlist_uri: String, from: usize, to: usize) {
        self.mutate(app, Some("reordering"), |c| async move {
            let result = api::playlists::reorder_track(&c, &playlist_uri, from, to).await;
            CrudResult::TrackReordered { playlist_uri, result }
        });
    }

    /// Appends to the playback queue. Not confirmed: it is non-destructive.
    pub fn add_to_queue(&self, app: &mut AppState, track_uri: String) {
        self.mutate(app, Some("adding to queue"), |c| async move {
            let result = api::queue::add_to_queue(&c, &track_uri).await;
            CrudResult::QueueAdded { track_uri, result }
        });
    }

    pub fn transfer_playback(&self, app: &mut AppState, device_id: String) {
        self.mutate(app, Some("transferring playback"), |c| async move {
            CrudResult::DeviceTransferred(api::devices::transfer_to(&c, &device_id).await)
        });
    }
}
