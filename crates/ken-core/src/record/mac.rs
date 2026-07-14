//! macOS capture backends and TCC permission probes. Everything here is a thin
//! shell over the pure `record` core; it is verified by hand (audio hardware
//! and TCC prompts can't be unit-tested).

use cpal::traits::HostTrait;

/// (id, display name) for each available input device. cpal identifies devices
/// by name on CoreAudio, so id == name here.
///
/// NOTE (cpal 0.18.1 API): `DeviceTrait::name()` was removed; a device's
/// human-readable name now comes from its `Display` impl (`device.to_string()`).
pub fn list_input_devices() -> Vec<(String, String)> {
    let host = cpal::default_host();
    let mut out = Vec::new();
    if let Ok(devices) = host.input_devices() {
        for d in devices {
            let name = d.to_string();
            out.push((name.clone(), name));
        }
    }
    out
}
