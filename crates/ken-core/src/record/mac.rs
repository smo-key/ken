//! macOS capture backends and TCC permission probes. Everything here is a thin
//! shell over the pure `record` core; it is verified by hand (audio hardware
//! and TCC prompts can't be unit-tested).

use std::sync::mpsc::{self, Sender};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::record::CaptureSource;
use crate::{Error, Result};

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

/// Microphone capture via cpal. The cpal `Stream` is `!Send` on CoreAudio, so
/// the stream is built and played on its OWN thread which then parks until a
/// stop signal arrives; only the `Sender` lives in the struct.
///
/// Per the record seam, the source hands the sink device-native interleaved f32
/// frames plus the device rate/channel count; the session owns the
/// downmix + resample (`ingest_frames`), so this backend does NO resampling.
pub struct MicSource {
    device_name: Option<String>,
    stop_tx: Option<Sender<()>>,
}

impl MicSource {
    pub fn new(device_name: Option<String>) -> Self {
        MicSource { device_name, stop_tx: None }
    }
}

impl CaptureSource for MicSource {
    fn start(&mut self, sink: Box<dyn FnMut(&[f32], u32, u16) + Send>) -> Result<()> {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let device_name = self.device_name.clone();

        std::thread::spawn(move || {
            let build = || -> Result<cpal::Stream> {
                let host = cpal::default_host();
                // cpal 0.18.1 removed `DeviceTrait::name()`; a device's name is
                // its `Display` impl, so we match by `to_string()`.
                let device = match &device_name {
                    Some(n) => host
                        .input_devices()
                        .ok()
                        .and_then(|mut it| it.find(|d| d.to_string() == *n))
                        .ok_or_else(|| Error::Other("that microphone isn't available".into()))?,
                    None => host
                        .default_input_device()
                        .ok_or_else(|| Error::Other("no microphone found".into()))?,
                };
                let supported = device
                    .default_input_config()
                    .map_err(|e| Error::Other(format!("microphone config error: {e}")))?;
                // cpal 0.18.1: `SampleRate`/`ChannelCount` are plain `u32`/`u16`
                // aliases now, so `sample_rate()` returns the rate directly (no
                // `.0` tuple field as in older cpal).
                let rate = supported.sample_rate();
                let channels = supported.channels();
                let fmt = supported.sample_format();
                // `build_input_stream` takes the config by value in 0.18.1.
                let config: cpal::StreamConfig = supported.into();

                let mut sink = sink;
                let err_fn = |e| eprintln!("microphone stream error: {e}");

                let stream = match fmt {
                    cpal::SampleFormat::F32 => device.build_input_stream(
                        config,
                        move |data: &[f32], _: &_| sink(data, rate, channels),
                        err_fn,
                        None,
                    ),
                    cpal::SampleFormat::I16 => device.build_input_stream(
                        config,
                        move |data: &[i16], _: &_| {
                            let f: Vec<f32> = data.iter().map(|&s| s as f32 / 32768.0).collect();
                            sink(&f, rate, channels);
                        },
                        err_fn,
                        None,
                    ),
                    cpal::SampleFormat::U16 => device.build_input_stream(
                        config,
                        move |data: &[u16], _: &_| {
                            let f: Vec<f32> =
                                data.iter().map(|&s| (s as f32 - 32768.0) / 32768.0).collect();
                            sink(&f, rate, channels);
                        },
                        err_fn,
                        None,
                    ),
                    other => return Err(Error::Other(format!("unsupported audio format: {other:?}"))),
                }
                .map_err(|e| Error::Other(format!("couldn't open the microphone: {e}")))?;

                stream
                    .play()
                    .map_err(|e| Error::Other(format!("couldn't start the microphone: {e}")))?;
                Ok(stream)
            };

            match build() {
                Ok(stream) => {
                    let _ = ready_tx.send(Ok(()));
                    let _ = stop_rx.recv(); // park until stop
                    drop(stream); // ends capture on this thread
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        });

        // Surface a build/permission error synchronously to the caller.
        match ready_rx.recv() {
            Ok(Ok(())) => {
                self.stop_tx = Some(stop_tx);
                Ok(())
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err(Error::Other("microphone thread failed to start".into())),
        }
    }

    fn stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
    }
}

// ---------------------------------------------------------------------------
// ScreenCaptureKit — compile-probe (Task 13)
//
// Confirmed screencapturekit 8.0.0 API surface (verified against the crate
// source), for the system-audio backend below:
//   - screencapturekit::shareable_content::SCShareableContent::get()
//         -> Result<SCShareableContent, SCError>
//   - SCShareableContent::displays()                 -> Vec<SCDisplay>
//   - SCContentFilter::create()
//         .with_display(&SCDisplay)
//         .with_excluding_windows(&[&SCWindow])       // empty slice ok
//         .build()                                     -> SCContentFilter
//   - SCStreamConfiguration::new()
//         .with_captures_audio(true)                   // infallible builder
//         .with_sample_rate(impl Into<i32>)
//         .with_channel_count(impl Into<i32>)          -> Self
//   - SCStream::new(&filter, &config)                  -> SCStream
//   - SCStream::add_output_handler(handler, SCStreamOutputType::Audio) (&mut self)
//   - SCStream::start_capture() / stop_capture()       -> Result<(), SCError> (&self)
//   - trait SCStreamOutputTrait::did_output_sample_buffer(
//         &self, sample: CMSampleBuffer, of_type: SCStreamOutputType)
//   - SCStreamOutputType::{Screen, Audio, Microphone}
//   - CMSampleBufferExt::audio_buffer_list(&self) -> Option<AudioBufferList>
//   - AudioBufferList::{num_buffers(), get(i) -> Option<&AudioBuffer>}
//   - AudioBuffer::{data() -> &[u8], number_channels: u32}   (32-bit float PCM)
// ---------------------------------------------------------------------------

/// Compile-probe: confirm the pinned ScreenCaptureKit v8 API by listing
/// shareable displays synchronously. Its only purpose is to lock the exact
/// module paths / method names before the capture backend below (the crate's
/// API churns across majors). Kept as a hand-run sanity check.
#[allow(dead_code)]
pub fn sck_probe() -> Result<usize> {
    use screencapturekit::shareable_content::SCShareableContent;
    let content = SCShareableContent::get()
        .map_err(|e| Error::Other(format!("ScreenCaptureKit unavailable: {e}")))?;
    Ok(content.displays().len())
}
