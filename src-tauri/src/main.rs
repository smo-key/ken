#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// The MSVC delay-load helper's failure hook. `vulkan-1.dll` is delay-loaded
/// (build.rs); when a PC has no Vulkan driver, this answers for it so the
/// models run on the CPU instead of Ken failing to start (see
/// `ken_app_lib::vulkan`). Defined here, in the exe's own object, so it
/// takes the place of delayimp.lib's empty default.
#[cfg(all(windows, target_env = "msvc"))]
#[no_mangle]
#[used]
#[allow(non_upper_case_globals)]
pub static __pfnDliFailureHook2: ken_app_lib::vulkan::DelayHook = Some(ken_app_lib::vulkan::delay_load_failure);

fn main() {
    ken_app_lib::run()
}
