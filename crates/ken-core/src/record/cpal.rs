//! Microphone capture through cpal, on every OS (CoreAudio on macOS, WASAPI
//! on Windows, ALSA elsewhere). A thin shell over the pure `record` core,
//! verified by hand: audio hardware can't be unit-tested.

use std::sync::mpsc::{self, Sender};

use ::cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::record::CaptureSource;
use crate::{Error, Result};

/// (id, display name) for each available input device. cpal identifies devices
/// by name, so id == name here.
///
/// NOTE (cpal 0.18.1 API): `DeviceTrait::name()` was removed; a device's
/// human-readable name now comes from its `Display` impl (`device.to_string()`).
pub fn list_input_devices() -> Vec<(String, String)> {
    let host = ::cpal::default_host();
    let mut out = Vec::new();
    if let Ok(devices) = host.input_devices() {
        for d in devices {
            let name = d.to_string();
            out.push((name.clone(), name));
        }
    }
    out
}

/// Whether the OS has a default microphone.
pub fn has_input() -> bool {
    ::cpal::default_host().default_input_device().is_some()
}

/// Whether the OS has a default output device (what system audio is
/// captured from on Windows).
pub fn has_output() -> bool {
    ::cpal::default_host().default_output_device().is_some()
}

/// The sink a cpal stream feeds: device-native interleaved f32 frames, rate,
/// channel count.
pub(crate) type Sink = Box<dyn FnMut(&[f32], u32, u16) + Send>;

/// Build and play an input stream on `device` with `supported`'s config,
/// converting every sample format to f32 for `sink`. For an output device on
/// WASAPI cpal opens it as loopback: what the device plays, captured.
pub(crate) fn play_input(
    device: &::cpal::Device,
    supported: ::cpal::SupportedStreamConfig,
    sink: Sink,
    what: &str,
) -> Result<::cpal::Stream> {
    // cpal 0.18.1: `SampleRate`/`ChannelCount` are plain `u32`/`u16`
    // aliases, so `sample_rate()` returns the rate directly.
    let rate = supported.sample_rate();
    let channels = supported.channels();
    let fmt = supported.sample_format();
    // `build_input_stream` takes the config by value in 0.18.1.
    let config: ::cpal::StreamConfig = supported.into();
    let mut sink = sink;
    let label = what.to_string();
    let err_fn = move |e| eprintln!("{label} stream error: {e}");
    let stream = match fmt {
        ::cpal::SampleFormat::F32 => device.build_input_stream(config, move |data: &[f32], _: &_| sink(data, rate, channels), err_fn, None),
        ::cpal::SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _: &_| {
                let f: Vec<f32> = data.iter().map(|&s| s as f32 / 32768.0).collect();
                sink(&f, rate, channels);
            },
            err_fn,
            None,
        ),
        ::cpal::SampleFormat::I32 => device.build_input_stream(
            config,
            move |data: &[i32], _: &_| {
                let f: Vec<f32> = data.iter().map(|&s| s as f32 / 2_147_483_648.0).collect();
                sink(&f, rate, channels);
            },
            err_fn,
            None,
        ),
        ::cpal::SampleFormat::U16 => device.build_input_stream(
            config,
            move |data: &[u16], _: &_| {
                let f: Vec<f32> = data.iter().map(|&s| (s as f32 - 32768.0) / 32768.0).collect();
                sink(&f, rate, channels);
            },
            err_fn,
            None,
        ),
        other => return Err(Error::Other(format!("unsupported audio format: {other:?}"))),
    }
    .map_err(|e| Error::Other(format!("couldn't open the {what}: {e}")))?;
    stream.play().map_err(|e| Error::Other(format!("couldn't start the {what}: {e}")))?;
    Ok(stream)
}

/// Run a cpal stream on its own thread (the `Stream` is `!Send` on some
/// hosts) until the returned sender is used or dropped. `build` runs on that
/// thread; its error comes back synchronously.
pub(crate) fn run_stream(build: impl FnOnce() -> Result<::cpal::Stream> + Send + 'static, what: &str) -> Result<Sender<()>> {
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
    std::thread::spawn(move || match build() {
        Ok(stream) => {
            let _ = ready_tx.send(Ok(()));
            let _ = stop_rx.recv(); // park until stop
            drop(stream); // ends capture on this thread
        }
        Err(e) => {
            let _ = ready_tx.send(Err(e));
        }
    });
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(stop_tx),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(Error::Other(format!("the {what} thread failed to start"))),
    }
}

/// Microphone capture via cpal. The stream is built and played on its own
/// thread, which parks until a stop signal arrives; only the `Sender` lives
/// in the struct.
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
        let device_name = self.device_name.clone();
        let stop = run_stream(
            move || {
                let host = ::cpal::default_host();
                let device = match &device_name {
                    Some(n) => host
                        .input_devices()
                        .ok()
                        .and_then(|mut it| it.find(|d| d.to_string() == *n))
                        .ok_or_else(|| Error::Other("that microphone isn't available".into()))?,
                    None => host.default_input_device().ok_or_else(|| Error::Other("no microphone found".into()))?,
                };
                let supported =
                    device.default_input_config().map_err(|e| Error::Other(format!("microphone config error: {e}")))?;
                play_input(&device, supported, sink, "microphone")
            },
            "microphone",
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
