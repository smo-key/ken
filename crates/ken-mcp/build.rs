fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Same as src-tauri/build.rs: the Swift static bridges reached through
    // ken-core need the /usr/lib/swift rpath, and Cargo does not propagate a
    // dependency's rustc-link-arg to the binary that links it.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
    // Same as src-tauri/build.rs: the embedding model's llama.cpp imports
    // vulkan-1.dll, which ships with GPU drivers, so it is delay-loaded and
    // the server starts without one (it then searches by keyword only).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        println!("cargo:rustc-link-arg-bins=/DELAYLOAD:vulkan-1.dll");
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }
}
