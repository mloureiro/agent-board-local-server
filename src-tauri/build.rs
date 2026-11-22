use std::path::Path;

use tauri_build::{AppManifest, Attributes};

fn main() {
    println!(
        "cargo:rustc-env=TARGET_TRIPLE={}",
        std::env::var("TARGET").expect("cargo sets TARGET for build scripts")
    );
    std::fs::create_dir_all("resources").expect("create the resources folder");
    println!("cargo:rerun-if-changed=resources");

    // tauri-build refuses externalBin and resources paths that do not exist yet, and the bundler reads tauri.conf.json itself, so this override never reaches a bundle.
    if !Path::new("resources/core-build.json").exists()
        && std::env::var_os("TAURI_CONFIG").is_none()
    {
        std::env::set_var(
            "TAURI_CONFIG",
            r#"{"bundle":{"externalBin":null,"resources":null}}"#,
        );
        println!("cargo:warning=the core is not staged: run pnpm core:build before pnpm dev, build, test:smoke or acceptance");
    }

    tauri_build::try_build(
        Attributes::new()
            .app_manifest(AppManifest::new().commands(&["enrol_submit", "enrol_cancel"])),
    )
    .expect("the Tauri build failed");
}
