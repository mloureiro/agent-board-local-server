use tauri_build::{AppManifest, Attributes};

fn main() {
    println!(
        "cargo:rustc-env=TARGET_TRIPLE={}",
        std::env::var("TARGET").expect("cargo sets TARGET for build scripts")
    );
    tauri_build::try_build(
        Attributes::new()
            .app_manifest(AppManifest::new().commands(&["enrol_submit", "enrol_cancel"])),
    )
    .expect("the Tauri build failed");
}
