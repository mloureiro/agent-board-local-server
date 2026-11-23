use std::future::Future;
use std::sync::Arc;

use tauri::{AppHandle, Manager};

use super::menu::{
    proxy_port, CHECK_WORKER, CONNECT_WORKER, DISCONNECT_WORKER, OPEN_BOARD, OPEN_LOGS,
    OPEN_SETTINGS, RESTART_SERVER, RESTART_WORKER,
};
use super::open_board::{open_board, open_folder, open_text};
use crate::activity::{Activity, Kind};
use crate::applog::LogFile;
use crate::enrol_window;
use crate::paths::AppPaths;
use crate::runtime::{self, Running};
use crate::supervisor::table::PROXY;
use crate::supervisor::StatusBoard;
use crate::worker::control::ActionError;

fn run_action<T, F>(app: &AppHandle, name: &'static str, action: F)
where
    T: Send + 'static,
    F: Future<Output = Result<T, ActionError>> + Send + 'static,
{
    let log = app.state::<LogFile>().inner().clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = action.await {
            log.write(&format!("{name}: {error}"));
        }
    });
}

pub fn handle_click(app: &AppHandle, id: &str, board: &Arc<StatusBoard>) {
    let running = app.state::<Running>().inner().clone();
    let paths = app.state::<AppPaths>().inner().clone();
    let activity = app.state::<Arc<Activity>>().inner().clone();
    match id {
        OPEN_BOARD => {
            if let Some(port) = proxy_port(board.get(PROXY).as_ref()) {
                tauri::async_runtime::spawn(async move {
                    open_board(port).await;
                });
            }
        }
        CONNECT_WORKER => enrol_window::open(app),
        RESTART_SERVER => run_action(app, "Restart server", async move {
            runtime::restart_server(&running).await
        }),
        RESTART_WORKER => run_action(app, "Restart worker", async move {
            runtime::restart_worker(&running).await
        }),
        DISCONNECT_WORKER => run_action(app, "Disconnect worker", async move {
            runtime::disconnect_worker(&running).await
        }),
        CHECK_WORKER => {
            let board = board.clone();
            run_action(app, "Check worker", async move {
                let log = runtime::check_worker(&running, &board).await?;
                open_text(&log).await;
                Ok(())
            });
        }
        OPEN_LOGS => run_action(app, "Open logs folder", async move {
            let _gate = activity.begin(Kind::Recovery).map_err(ActionError::Busy)?;
            open_folder(&paths.logs_dir()).await;
            Ok(())
        }),
        OPEN_SETTINGS => run_action(app, "Open settings file", async move {
            let _gate = activity.begin(Kind::Recovery).map_err(ActionError::Busy)?;
            open_text(&paths.settings_file()).await;
            Ok(())
        }),
        _ => {}
    }
}
