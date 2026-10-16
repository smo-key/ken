//! Ken starts on a Windows PC with no Vulkan driver (VMs, remote desktops,
//! the basic display adapter, locked-down images).
//!
//! whisper.cpp and llama.cpp are built with ggml's Vulkan backend, which
//! imports `vulkan-1.dll`. The DLL comes with GPU drivers, not with Windows,
//! so a plain import would stop Ken from starting where it is missing.
//! `build.rs` links it with `/DELAYLOAD`: the DLL is loaded on the first
//! Vulkan call instead, which ggml makes when it lists its backends.
//!
//! When the DLL is not there, the delay-load helper asks
//! [`delay_load_failure`] (installed in `main.rs` as `__pfnDliFailureHook2`).
//! It answers every Vulkan entry point with one that fails with
//! `VK_ERROR_INITIALIZATION_FAILED`: ggml's instance setup throws, its own
//! catch registers no Vulkan backend, and the models run on the CPU.
//! [`loader_present`] checks for the DLL up front, before any model loads.

use std::ffi::{c_char, c_void, CStr};

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
}

/// `DelayLoadProc` from delayimp.h: a procedure named or by ordinal.
#[repr(C)]
pub struct DelayLoadProc {
    import_by_name: i32,
    /// `LPCSTR szProcName` or `DWORD dwOrdinal`.
    name_or_ordinal: usize,
}

/// `DelayLoadInfo` from delayimp.h, as the helper hands it to a hook. Only
/// the DLL and procedure names are read; the rest keeps the layout.
#[repr(C)]
#[allow(dead_code)]
pub struct DelayLoadInfo {
    cb: u32,
    pidd: *const c_void,
    ppfn: *mut *const c_void,
    sz_dll: *const c_char,
    dlp: DelayLoadProc,
    hmod_cur: *mut c_void,
    pfn_cur: *const c_void,
    dw_last_error: u32,
}

/// `PfnDliHook` from delayimp.h.
pub type DelayHook = Option<unsafe extern "system" fn(u32, *const DelayLoadInfo) -> *const c_void>;

const DLI_FAIL_LOAD_LIB: u32 = 3;
const DLI_FAIL_GET_PROC: u32 = 4;
const VK_ERROR_INITIALIZATION_FAILED: i32 = -3;

/// Whether `vulkan-1.dll` can be loaded. Checked once; when it cannot, Ken
/// sets `KEN_GPU=off` (unless the person set it) so the models are loaded for
/// the CPU from the start.
pub fn loader_present() -> bool {
    static PRESENT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PRESENT.get_or_init(|| {
        let name: Vec<u16> = "vulkan-1.dll\0".encode_utf16().collect();
        // SAFETY: a NUL-terminated wide string that outlives the call. The
        // module stays loaded, which is what the delay-load helper reuses.
        !unsafe { LoadLibraryW(name.as_ptr()) }.is_null()
    })
}

/// Before any model loads: with no Vulkan loader, ask for the CPU.
pub fn prefer_cpu_without_loader() {
    if !loader_present() && std::env::var_os("KEN_GPU").is_none() {
        std::env::set_var("KEN_GPU", "off");
    }
}

/// The delay-load failure hook (see the module docs). Only `vulkan-1.dll` is
/// answered; any other DLL fails as it would without a hook.
///
/// # Safety
/// Called by the MSVC delay-load helper with a valid `DelayLoadInfo`.
pub unsafe extern "system" fn delay_load_failure(notify: u32, info: *const DelayLoadInfo) -> *const c_void {
    // SAFETY: the helper passes a valid pointer (or null, handled).
    let Some(info) = (unsafe { info.as_ref() }) else { return std::ptr::null() };
    // SAFETY: the helper's DLL name is a NUL-terminated string.
    if info.sz_dll.is_null() || !unsafe { CStr::from_ptr(info.sz_dll) }.to_bytes().eq_ignore_ascii_case(b"vulkan-1.dll") {
        return std::ptr::null();
    }
    match notify {
        // Any module will do: nothing in Ken's exe is named like a Vulkan
        // function, so every lookup in it fails and comes back below.
        // SAFETY: a null name asks for the exe's own module handle.
        DLI_FAIL_LOAD_LIB => (unsafe { GetModuleHandleW(std::ptr::null()) }) as *const c_void,
        DLI_FAIL_GET_PROC => {
            let by_name = info.dlp.import_by_name != 0 && info.dlp.name_or_ordinal > 0xFFFF;
            // SAFETY: an import by name carries a NUL-terminated name.
            let name = by_name.then(|| unsafe { CStr::from_ptr(info.dlp.name_or_ordinal as *const c_char) });
            stand_in(name.map(CStr::to_bytes))
        }
        _ => std::ptr::null(),
    }
}

/// The function that stands in for the Vulkan entry point `name`.
fn stand_in(name: Option<&[u8]>) -> *const c_void {
    if name == Some(b"vkGetInstanceProcAddr".as_slice()) {
        vk_get_instance_proc_addr as *const c_void
    } else {
        vk_unavailable as *const c_void
    }
}

/// `vkGetInstanceProcAddr` without a loader: every function it hands out
/// fails (vulkan.hpp's dispatcher looks them all up through this one).
unsafe extern "system" fn vk_get_instance_proc_addr(_instance: *mut c_void, name: *const c_char) -> *const c_void {
    // SAFETY: Vulkan passes a NUL-terminated function name.
    let name = (!name.is_null()).then(|| unsafe { CStr::from_ptr(name) }.to_bytes());
    stand_in(name)
}

/// Every other Vulkan entry point: `VK_ERROR_INITIALIZATION_FAILED`. The
/// arguments are ignored; on x64 Windows the caller cleans them up.
unsafe extern "system" fn vk_unavailable() -> i32 {
    VK_ERROR_INITIALIZATION_FAILED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_vulkan_is_answered_and_every_function_fails_cleanly() {
        let dll = c"vulkan-1.dll";
        let other = c"other.dll";
        let proc_name = c"vkCreateInstance";
        let info = |dll: &CStr, name: Option<&CStr>| DelayLoadInfo {
            cb: std::mem::size_of::<DelayLoadInfo>() as u32,
            pidd: std::ptr::null(),
            ppfn: std::ptr::null_mut(),
            sz_dll: dll.as_ptr(),
            dlp: DelayLoadProc {
                import_by_name: name.is_some() as i32,
                name_or_ordinal: name.map_or(7, |n| n.as_ptr() as usize),
            },
            hmod_cur: std::ptr::null_mut(),
            pfn_cur: std::ptr::null(),
            dw_last_error: 126,
        };
        unsafe {
            assert!(!delay_load_failure(DLI_FAIL_LOAD_LIB, &info(dll, None)).is_null());
            assert!(delay_load_failure(DLI_FAIL_LOAD_LIB, &info(other, None)).is_null());
            assert!(delay_load_failure(DLI_FAIL_GET_PROC, &info(other, Some(proc_name))).is_null());

            let f = delay_load_failure(DLI_FAIL_GET_PROC, &info(dll, Some(proc_name)));
            let f: unsafe extern "system" fn() -> i32 = std::mem::transmute(f);
            assert_eq!(f(), VK_ERROR_INITIALIZATION_FAILED);

            let gipa = delay_load_failure(DLI_FAIL_GET_PROC, &info(dll, Some(c"vkGetInstanceProcAddr")));
            let gipa: unsafe extern "system" fn(*mut c_void, *const c_char) -> *const c_void = std::mem::transmute(gipa);
            let enumerate = gipa(std::ptr::null_mut(), c"vkEnumerateInstanceVersion".as_ptr());
            let enumerate: unsafe extern "system" fn(*mut u32) -> i32 = std::mem::transmute(enumerate);
            let mut version = 0u32;
            assert_eq!(enumerate(&mut version), VK_ERROR_INITIALIZATION_FAILED);
        }
    }
}
