//! Windows system audio ("Them") and the microphone privacy switch. System
//! audio is WASAPI loopback through cpal: an input stream opened on the
//! default output device captures what it plays. Loopback delivers nothing
//! while nothing plays, so the session pads that silence back in (see
//! `record::silence_gap`) to keep Me and Them on one clock. Verified by hand.

use std::sync::mpsc::Sender;

use ::cpal::traits::{DeviceTrait, HostTrait};

use crate::record::{CaptureSource, PermissionStatus};
use crate::{Error, Result};

/// What the default output device plays, captured (WASAPI loopback).
pub struct SystemAudioSource {
    stop_tx: Option<Sender<()>>,
}

impl SystemAudioSource {
    pub fn new() -> Self {
        SystemAudioSource { stop_tx: None }
    }
}

impl Default for SystemAudioSource {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureSource for SystemAudioSource {
    fn start(&mut self, sink: Box<dyn FnMut(&[f32], u32, u16) + Send>) -> Result<()> {
        let stop = super::cpal::run_stream(
            move || {
                let host = ::cpal::default_host();
                let device = host
                    .default_output_device()
                    .ok_or_else(|| Error::Other("no speaker or headset to record system audio from".into()))?;
                let supported = device
                    .default_output_config()
                    .map_err(|e| Error::Other(format!("system audio config error: {e}")))?;
                // An input stream on an output device: cpal sets
                // AUDCLNT_STREAMFLAGS_LOOPBACK for it.
                super::cpal::play_input(&device, supported, sink, "system audio")
            },
            "system audio",
        )?;
        self.stop_tx = Some(stop);
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
    }
}

const CONSENT: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";

/// The microphone privacy switch, read from the CapabilityAccessManager
/// consent store: off for the whole PC (HKLM), for apps (HKCU), or for
/// desktop apps (HKCU `NonPackaged`) reads as denied. No prompt.
pub fn mic_permission() -> PermissionStatus {
    let values = [
        read_value(windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE, CONSENT),
        read_value(windows_sys::Win32::System::Registry::HKEY_CURRENT_USER, CONSENT),
        read_value(windows_sys::Win32::System::Registry::HKEY_CURRENT_USER, &format!(r"{CONSENT}\NonPackaged")),
    ];
    consent(&values)
}

/// Denied when any switch says `Deny`; otherwise granted (an absent value is
/// the default, which allows).
pub fn consent(values: &[Option<String>]) -> PermissionStatus {
    if values.iter().flatten().any(|v| v.trim().eq_ignore_ascii_case("deny")) {
        PermissionStatus::Denied
    } else {
        PermissionStatus::Granted
    }
}

/// The string `Value` under `key`, if it is there.
fn read_value(root: windows_sys::Win32::System::Registry::HKEY, key: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_SZ};
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (key_w, name_w) = (wide(key), wide("Value"));
    let mut buf = [0u16; 64];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: both names are NUL-terminated UTF-16; `buf` is valid for `len`
    // bytes and RegGetValueW writes at most that many.
    let status = unsafe {
        RegGetValueW(root, key_w.as_ptr(), name_w.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len)
    };
    if status != 0 {
        return None;
    }
    let chars = (len as usize / 2).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..chars]).trim_end_matches('\0').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_deny_switch_denies_the_microphone() {
        assert_eq!(consent(&[None, None, None]), PermissionStatus::Granted);
        assert_eq!(consent(&[Some("Allow".into()), Some("Allow".into()), None]), PermissionStatus::Granted);
        assert_eq!(consent(&[Some("Allow".into()), None, Some("Deny".into())]), PermissionStatus::Denied);
        assert_eq!(consent(&[Some("Deny".into()), Some("Allow".into()), None]), PermissionStatus::Denied);
    }

    #[test]
    fn the_registry_read_does_not_fail_on_this_machine() {
        // Whatever the switch says, reading it returns a status.
        let p = mic_permission();
        assert!(matches!(p, PermissionStatus::Granted | PermissionStatus::Denied));
    }
}
