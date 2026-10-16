//! Where the on-device models run: the graphics card or the CPU, and how
//! many CPU threads they get.
//!
//! The person can turn the graphics card off (Settings › AI, saved
//! as `useGpu` in settings.json and handed in with [`set_use_gpu`]); `KEN_GPU=off`
//! does the same from the environment, and the app sets it when Windows has
//! no Vulkan loader. A model that fails to load on the graphics card is loaded
//! again on the CPU (see the embedder and the transcriber).

use std::sync::atomic::{AtomicBool, Ordering};

static USE_GPU: AtomicBool = AtomicBool::new(true);

/// The person's choice, from settings.
pub fn set_use_gpu(on: bool) {
    USE_GPU.store(on, Ordering::SeqCst);
}

/// Whether models should be offered to the graphics card.
pub fn use_gpu() -> bool {
    let env_off = std::env::var("KEN_GPU").map(|v| v.eq_ignore_ascii_case("off") || v == "0").unwrap_or(false);
    !env_off && USE_GPU.load(Ordering::SeqCst)
}

/// Layers to offload: all of them, or none on the CPU.
pub fn gpu_layers() -> u32 {
    if use_gpu() {
        1000
    } else {
        0
    }
}

/// CPU threads for model work: the machine's parallelism less one for the
/// app, at least one. llama.cpp's and whisper.cpp's own default is 4, which
/// leaves most of a modern laptop idle.
pub fn threads() -> i32 {
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    n.saturating_sub(1).clamp(1, 64) as i32
}

/// The device the models run on, as a person reads it ("NVIDIA RTX PRO 2000"),
/// and its backend ("vulkan" | "metal" | "cpu").
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Device {
    pub name: Option<String>,
    pub backend: String,
}

/// The first graphics device ggml can use, when the graphics card is on.
#[cfg(feature = "local-llm")]
pub fn device() -> Device {
    if !use_gpu() {
        return Device { name: None, backend: "cpu".into() };
    }
    // The backend must be up before ggml lists its devices.
    if crate::local_llm::shared_backend().is_err() {
        return Device { name: None, backend: "cpu".into() };
    }
    use llama_cpp_2::LlamaBackendDeviceType as T;
    let devices = llama_cpp_2::list_llama_ggml_backend_devices();
    let pick = devices
        .iter()
        .find(|d| matches!(d.device_type, T::Gpu))
        .or_else(|| devices.iter().find(|d| matches!(d.device_type, T::IntegratedGpu | T::Accelerator)));
    match pick {
        Some(d) => Device {
            name: Some(if d.description.is_empty() { d.name.clone() } else { d.description.clone() }),
            backend: d.backend.to_lowercase(),
        },
        None => Device { name: None, backend: "cpu".into() },
    }
}

#[cfg(not(feature = "local-llm"))]
pub fn device() -> Device {
    Device { name: None, backend: "cpu".into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threads_leave_one_for_the_app_and_never_drop_below_one() {
        let t = threads();
        assert!(t >= 1);
        let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) as i32;
        assert!(t <= n.max(1));
    }

    #[test]
    fn the_setting_turns_offload_off() {
        set_use_gpu(false);
        assert_eq!(gpu_layers(), 0);
        set_use_gpu(true);
        if std::env::var_os("KEN_GPU").is_none() {
            assert_eq!(gpu_layers(), 1000);
        }
    }
}
