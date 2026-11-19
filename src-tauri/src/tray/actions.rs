use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::async_runtime::JoinHandle;

use super::enrol_dialog::{connect_worker, Enroller, Osascript};
use super::menu::{proxy_port, CONNECT_WORKER, OPEN_BOARD};
use super::open_board::open_board;
use crate::runtime::{Handle, Running, PROXY};
use crate::supervisor::StatusBoard;
use crate::worker::enrol::EnrolError;

static DIALOG_OPEN: AtomicBool = AtomicBool::new(false);
static DIALOG_TASK: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

struct RunningWorker(Running);

impl Enroller for RunningWorker {
    async fn enrol(&self, token: &str, data_key: &str) -> Result<(), EnrolError> {
        let worker = self.0.lock().await.as_ref().and_then(Handle::worker);
        match worker {
            Some(worker) => worker.enrol(token, data_key).await,
            None => Err(EnrolError::new("the app is quitting")),
        }
    }
}

struct DialogSlot;

impl Drop for DialogSlot {
    fn drop(&mut self) {
        DIALOG_OPEN.store(false, Ordering::SeqCst);
    }
}

pub fn handle_click(id: &str, board: &Arc<StatusBoard>, running: &Running) {
    match id {
        OPEN_BOARD => {
            if let Some(port) = proxy_port(board.get(PROXY).as_ref()) {
                tauri::async_runtime::spawn(async move {
                    open_board(port).await;
                });
            }
        }
        CONNECT_WORKER => start_dialogs(running.clone()),
        _ => {}
    }
}

fn start_dialogs(running: Running) {
    if DIALOG_OPEN.swap(true, Ordering::SeqCst) {
        return;
    }
    let task = tauri::async_runtime::spawn(async move {
        let _slot = DialogSlot;
        connect_worker(&Osascript, &RunningWorker(running)).await;
    });
    *DIALOG_TASK.lock().expect("dialog task lock") = Some(task);
}

/// Dropping the flow kills the `osascript` it is waiting on, so no dialog outlives the app.
pub fn abort_dialogs() {
    if let Some(task) = DIALOG_TASK.lock().expect("dialog task lock").take() {
        task.abort();
    }
}
