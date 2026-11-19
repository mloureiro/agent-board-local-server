#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use agent_board_local_server::paths::AppPaths;
use agent_board_local_server::secrets::Keychain;
use agent_board_local_server::worker::credentials::run_helper_if_requested;

/// The CLI runs this executable with the worker environment, whose config directory sits two levels under the root.
fn helper_keychain() -> Keychain {
    let root = std::env::var_os("AGENT_BOARD_CONFIG_DIR")
        .map(PathBuf::from)
        .and_then(|config| Some(config.parent()?.parent()?.to_path_buf()))
        .or_else(AppPaths::default_root);
    root.map_or_else(Keychain::app, |root| Keychain::for_root(&root))
}

fn main() {
    // The CLI runs this executable as its credentials command; that must finish before the app initialises.
    if let Some(code) = run_helper_if_requested(std::env::args_os(), &helper_keychain()) {
        std::process::exit(code);
    }
    agent_board_local_server::run()
}
