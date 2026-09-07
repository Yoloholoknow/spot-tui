//! Phase 0 spikes (design-scope plan): verify the two primitives the rest
//! of the plan assumes work, before any UI is built around them.
//!
//! Run via `spot-tui --spike-phase0`. Prints straight to stdout (no TUI is
//! up yet in this mode, so the usual stdout/stderr-vs-raw-mode concern
//! doesn't apply). Temporary, throwaway code -- delete this file and its
//! `mod spike;` + flag check in `main.rs` once real Devices (Phase 8) and
//! Playlist reorder (Phase 6) screens exist and cover the same ground.
//!
//! Finding worth keeping even after this file is deleted: `Spirc::transfer`
//! only lets a device reclaim *itself* (it calls librespot's internal
//! spclient endpoint with the same device_id as both source and target --
//! confirmed by reading librespot-connect 0.8.0's source). It is not the
//! primitive for pushing playback to a *different* device. That's rspotify's
//! `device()` + `transfer_playback()` instead -- the public, documented
//! Spotify Web API, already covered by scopes this app already requests.

use rspotify::clients::{BaseClient, OAuthClient};
use rspotify::model::{LibraryId, PlayableId, TrackId};
use rspotify::AuthCodeSpotify;

pub async fn run_phase0(client: &AuthCodeSpotify) -> Result<(), String> {
    println!("=== Phase 0 spike: devices + transfer ===");
    spike_devices_and_transfer(client).await;

    println!("\n=== Phase 0 spike: playlist reorder ===");
    spike_playlist_reorder(client).await?;

    Ok(())
}

async fn spike_devices_and_transfer(client: &AuthCodeSpotify) {
    let devices = match client.device().await {
        Ok(d) => d,
        Err(e) => {
            println!("device() failed: {e}");
            return;
        }
    };

    if devices.is_empty() {
        println!("no devices returned -- is anything with Spotify open right now?");
        return;
    }

    for d in &devices {
        println!(
            "  {} | active={} | {:?} | id={}",
            d.name,
            d.is_active,
            d._type,
            d.id.as_deref().unwrap_or("<none>")
        );
    }

    let target = devices.iter().find(|d| !d.is_active && d.id.is_some());
    match target {
        None => {
            println!("only one device online (or no inactive device has an id) -- open Spotify on a second device (phone, etc.) and re-run to test cross-device transfer.");
        }
        Some(d) => {
            let id = d.id.clone().unwrap();
            println!("attempting transfer_playback -> \"{}\" ({id})", d.name);
            match client.transfer_playback(&id, Some(true)).await {
                Ok(()) => println!(
                    "transfer_playback returned Ok -- CHECK \"{}\" NOW: is it actually playing? \
                     (a 200 here doesn't guarantee real playback state changed -- this needs your eyes.) \
                     Re-run this spike to transfer back.",
                    d.name
                ),
                Err(e) => println!("transfer_playback failed: {e}"),
            }
        }
    }
}

async fn spike_playlist_reorder(client: &AuthCodeSpotify) -> Result<(), String> {
    let user_id = client.me().await.map_err(|e| e.to_string())?.id;

    let playlist = client
        .user_playlist_create(
            user_id,
            "spot-tui phase0 spike (safe to delete)",
            Some(false),
            Some(false),
            Some("temporary playlist created by spot-tui's Phase 0 API spike -- deleted automatically at the end of this run"),
        )
        .await
        .map_err(|e| e.to_string())?;
    println!("created scratch playlist: {} ({})", playlist.name, playlist.id);

    // Real tracks, fetched live rather than hardcoded IDs that might not
    // exist / might not be playable in this account's market.
    let seed_tracks = crate::api::search::search_tracks(client, "yung kai", 3)
        .await
        .map_err(|e| e.to_string())?;
    if seed_tracks.len() < 3 {
        cleanup_playlist(client, playlist.id.clone()).await;
        return Err(format!(
            "needed 3 seed tracks to test reorder, search only returned {}",
            seed_tracks.len()
        ));
    }

    let track_ids: Vec<TrackId> = seed_tracks
        .iter()
        .map(|t| TrackId::from_uri(&t.uri).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    let playable: Vec<PlayableId> = track_ids.iter().cloned().map(PlayableId::Track).collect();

    client
        .playlist_add_items(playlist.id.clone(), playable, None)
        .await
        .map_err(|e| e.to_string())?;

    let start = fetch_track_names(client, playlist.id.clone()).await?;
    println!("start:                    {start:?}");

    // Round 1: move index 0 to the end of a 3-item list. `insert_before` is
    // indexed against the array BEFORE removal -- Spotify's own documented
    // convention for "append to the end" is insert_before = original length
    // (3 here), not the last valid index (2). Moving down: insert_before = target + 1.
    client
        .playlist_reorder_items(playlist.id.clone(), Some(0), Some(3), Some(1), None)
        .await
        .map_err(|e| e.to_string())?;
    let after_down = fetch_track_names(client, playlist.id.clone()).await?;
    println!("after move 0->end (down): {after_down:?}");
    let expected_down = vec![start[1].clone(), start[2].clone(), start[0].clone()];
    check("move down (insert_before = target + 1)", &after_down, &expected_down);

    // Round 2: move the item now at the end (index 2) back to the front.
    // Moving up: insert_before = target index directly, no +1 -- confirms
    // the asymmetry rather than assuming it holds both directions.
    client
        .playlist_reorder_items(playlist.id.clone(), Some(2), Some(0), Some(1), None)
        .await
        .map_err(|e| e.to_string())?;
    let after_up = fetch_track_names(client, playlist.id.clone()).await?;
    println!("after move end->0 (up):   {after_up:?}");
    let expected_up = vec![after_down[2].clone(), after_down[0].clone(), after_down[1].clone()];
    check("move up (insert_before = target, no +1)", &after_up, &expected_up);

    cleanup_playlist(client, playlist.id.clone()).await;
    Ok(())
}

fn check(label: &str, actual: &[String], expected: &[String]) {
    if actual == expected {
        println!("PASS: {label}");
    } else {
        println!("FAIL: {label}\n  expected: {expected:?}\n  got:      {actual:?}");
    }
}

async fn fetch_track_names(
    client: &AuthCodeSpotify,
    playlist_id: rspotify::model::PlaylistId<'static>,
) -> Result<Vec<String>, String> {
    let page = client
        .playlist_items_manual(playlist_id, None, None, None, None)
        .await
        .map_err(|e| e.to_string())?;

    Ok(page
        .items
        .into_iter()
        .map(|item| match item.item {
            Some(rspotify::model::PlayableItem::Track(t)) => t.name,
            Some(rspotify::model::PlayableItem::Episode(e)) => e.name,
            _ => "<unknown>".to_string(),
        })
        .collect())
}

async fn cleanup_playlist(client: &AuthCodeSpotify, playlist_id: rspotify::model::PlaylistId<'static>) {
    match client.library_remove([LibraryId::Playlist(playlist_id)]).await {
        Ok(()) => println!("cleaned up: scratch playlist removed."),
        Err(e) => println!(
            "WARNING: failed to auto-remove the scratch playlist, delete it by hand: {e}"
        ),
    }
}
