//! CanopusConnect: the host calls (`send`, `listen`) and the device list. The BAL2D module on the
//! watch is addressed by its module id.

use std::{collections::HashSet, sync::Mutex};

use bal2d_proto::{FRAME_MAX, MODULE_ID};

use crate::{
    astrobox::psys_host_v4::{
        canopusconnect::{self as host, Payload},
        device,
    },
    state::{self, DeviceInfo},
};

static LISTENING: Mutex<Option<HashSet<String>>> = Mutex::new(None);

pub async fn refresh_devices() {
    let devices = device::get_connected_device_list()
        .await
        .into_iter()
        .map(|d| DeviceInfo { addr: d.addr, name: d.name })
        .collect::<Vec<_>>();
    let known: HashSet<String> = devices.iter().map(|d| d.addr.clone()).collect();
    state::with_state(|s| {
        s.devices = devices;
        if !s.devices.iter().any(|d| d.addr == s.selected_addr) {
            s.selected_addr = s.devices.first().map(|d| d.addr.clone()).unwrap_or_default();
            s.hello = None;
            s.packs.clear();
        }
    });
    // A device that went away forgot the registration.
    if let Some(listening) = LISTENING.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        listening.retain(|addr| known.contains(addr));
    }
}

/// Asks the device to push the module's messages to this phone; once per device.
pub async fn ensure_listening(addr: &str) -> Result<(), String> {
    let already = LISTENING.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|s| s.contains(addr));
    if already {
        return Ok(());
    }
    host::listen(addr.to_string(), MODULE_ID.to_string()).await?;
    LISTENING.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).insert(addr.to_string());
    Ok(())
}

/// One frame to the module; resolves when the module has taken it.
pub async fn send(addr: &str, frame: &[u8]) -> Result<(), String> {
    if frame.len() > FRAME_MAX {
        return Err("frame is longer than 480 bytes".into());
    }
    host::send(addr.to_string(), MODULE_ID.to_string(), Payload::Binary(frame.to_vec())).await
}

/// The frame of a `canopusconnect.on-message` call, if it is binary and from the BAL2D module.
pub fn binary_frame(module_id: &str, payload: Payload) -> Option<Vec<u8>> {
    match payload {
        Payload::Binary(bytes) if module_id == MODULE_ID => Some(bytes),
        _ => None,
    }
}
