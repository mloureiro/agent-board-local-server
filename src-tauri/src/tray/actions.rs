use std::sync::Arc;

use tauri::AppHandle;

use super::menu::{proxy_port, CONNECT_WORKER, OPEN_BOARD};
use super::open_board::open_board;
use crate::enrol_window;
use crate::supervisor::table::PROXY;
use crate::supervisor::StatusBoard;

pub fn handle_click(app: &AppHandle, id: &str, board: &Arc<StatusBoard>) {
    match id {
        OPEN_BOARD => {
            if let Some(port) = proxy_port(board.get(PROXY).as_ref()) {
                tauri::async_runtime::spawn(async move {
                    open_board(port).await;
                });
            }
        }
        CONNECT_WORKER => enrol_window::open(app),
        _ => {}
    }
}
