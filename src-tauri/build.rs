fn main() {
    // The Swift static bridges pulled in via ken-core (screencapturekit and
    // friends) reference @rpath/libswift_Concurrency.dylib. Those crates ask for
    // the /usr/lib/swift rpath in their own build scripts, but Cargo does not
    // propagate a dependency's rustc-link-arg to the downstream binary, so the
    // final executable has to add the rpath itself or dyld fails at launch.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
    tauri_build::build()
}
