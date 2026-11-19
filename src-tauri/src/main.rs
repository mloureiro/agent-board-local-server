#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use agent_board_local_server::secrets::Keychain;
use agent_board_local_server::worker::credentials::run_helper_if_requested;

fn main() {
    // The CLI runs this executable as its credentials command; that must finish before the app initialises.
    if let Some(code) = run_helper_if_requested(std::env::args_os(), &Keychain::app()) {
        std::process::exit(code);
    }
    agent_board_local_server::run()
}
