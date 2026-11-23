#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use agent_board_local_server::paths::AppPaths;
use agent_board_local_server::secrets::Keychain;
use agent_board_local_server::uninstall;
use agent_board_local_server::worker::credentials::{helper_keychain, run_helper_if_requested};

fn helper_store() -> Keychain {
    helper_keychain().unwrap_or_else(|| {
        AppPaths::default_root().map_or_else(Keychain::app, |root| Keychain::for_root(&root))
    })
}

fn main() {
    // The CLI runs this executable as its credentials command; that must finish before the app initialises.
    if let Some(code) = run_helper_if_requested(std::env::args_os(), Some(&helper_store())) {
        std::process::exit(code);
    }
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match uninstall::parse_args(&args) {
        Some(Ok(options)) => std::process::exit(uninstall::run(options)),
        Some(Err(usage)) => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
        None => agent_board_local_server::run(),
    }
}
