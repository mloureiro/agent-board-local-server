use std::sync::Arc;

use tauri::{AppHandle, Manager};

use super::menu::{proxy_port, CHECK_WORKER, CONNECT_WORKER, OPEN_BOARD};
use super::open_board::{open_board, open_text};
use crate::enrol_window;
use crate::runtime::{self, Running};
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
        CHECK_WORKER => {
            let running = app.state::<Running>().inner().clone();
            let board = board.clone();
            tauri::async_runtime::spawn(async move {
                if let Some(log) = runtime::check_worker(&running, &board).await {
                    open_text(&log).await;
                }
            });
        }
        _ => {}
    }
}
