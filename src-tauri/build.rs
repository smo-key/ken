fn main() {
    // The Swift static bridges pulled in via ken-core (screencapturekit and
    // friends) reference @rpath/libswift_Concurrency.dylib. Those crates ask for
    // the /usr/lib/swift rpath in their own build scripts, but Cargo does not
    // propagate a dependency's rustc-link-arg to the downstream binary, so the
    // final executable has to add the rpath itself or dyld fails at launch.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
    // ggml's Vulkan backend imports vulkan-1.dll, which ships with GPU
    // drivers, not with Windows. Delay-load it so Ken.exe starts without
    // one; src/vulkan.rs answers for it when it is missing (CPU models).
    // Only the app's exe: the test binaries keep the plain import.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        println!("cargo:rustc-link-arg-bins=/DELAYLOAD:vulkan-1.dll");
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }
    tauri_build::build()
}
