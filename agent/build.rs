//! Hands the linker the version script that fixes the library's exported symbols.

fn main() {
    println!("cargo:rerun-if-changed=version.map");
    println!("cargo:rerun-if-changed=build.rs");

    // macOS takes `-exported_symbols_list` instead.
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os != "linux" && os != "freebsd" {
        return;
    }

    let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") else {
        println!("cargo::error=CARGO_MANIFEST_DIR is unset, so version.map cannot be located");
        return;
    };
    println!("cargo:rustc-cdylib-link-arg=-Wl,--version-script={manifest_dir}/version.map");
}
