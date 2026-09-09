//! Connect devices (Phase 8): list them, transfer playback to one.
//! `device()`/`transfer_playback()`, not `Spirc::transfer` -- Phase 0's
//! spike (`spike.rs::spike_devices_and_transfer`) already established why:
//! `Spirc::transfer()` only lets a device reclaim *itself*, there is no
//! public `Spirc` method to push playback to a *different* device. This
//! is the same mechanism that spike proved works live (confirmed by the
//! user's own account: "it redirects audio to my alexa echo dot").

use rspotify::clients::OAuthClient;
use rspotify::model::Device;
use rspotify::AuthCodeSpotify;

use super::ensure_fresh;

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceSummary {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub is_active: bool,
    pub volume_percent: Option<u32>,
}

/// Devices with no `id` are skipped -- rare/restricted devices the Web
/// API itself reports as untargetable, and `transfer_playback` needs an
/// id regardless.
fn to_summary(d: Device) -> Option<DeviceSummary> {
    Some(DeviceSummary {
        id: d.id?,
        name: d.name,
        kind: <&str>::from(&d._type).to_string(),
        is_active: d.is_active,
        volume_percent: d.volume_percent,
    })
}

pub async fn list_devices(client: &AuthCodeSpotify) -> Result<Vec<DeviceSummary>, String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before list_devices failed, trying with existing token anyway: {e}");
    }
    let devices = client.device().await.map_err(|e| e.to_string())?;
    Ok(devices.into_iter().filter_map(to_summary).collect())
}

/// `play: false` keeps whatever play/pause state the session already had
/// (per rspotify's own doc comment on `transfer_playback`'s `play` arg) --
/// the least surprising choice for "just move my playback over," not
/// "also force it to start playing."
pub async fn transfer_to(client: &AuthCodeSpotify, device_id: &str) -> Result<(), String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before transfer_to failed, trying with existing token anyway: {e}");
    }
    client
        .transfer_playback(device_id, Some(false))
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
