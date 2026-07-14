fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Same as src-tauri/build.rs: the Swift static bridges reached through
    // ken-core need the /usr/lib/swift rpath, and Cargo does not propagate a
    // dependency's rustc-link-arg to the binary that links it.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
