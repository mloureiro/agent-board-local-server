use std::sync::{Arc, Mutex};

use tauri::{
    AppHandle, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent,
};
use tokio::task::AbortHandle;

use super::session::{Enroller, Phase, Session};
use crate::runtime::{Handle, Running};
use crate::worker::control::Enrolled;
use crate::worker::enrol::EnrolError;

const LABEL: &str = "enrol";
const PAGE: &str = "enrol.html";

struct RunningWorker(Running);

impl Enroller for RunningWorker {
    async fn enrol(&self, token: &str, data_key: &str) -> Result<Enrolled, EnrolError> {
        let worker = self.0.lock().await.as_ref().and_then(Handle::worker);
        match worker {
            Some(worker) => worker.enrol(token, data_key).await,
            None => Err(EnrolError::new("the app is quitting")),
        }
    }
}

pub struct EnrolWindow {
    session: Arc<Session<RunningWorker>>,
    task: Mutex<Option<AbortHandle>>,
}

pub fn manage(app: &AppHandle, running: Running) {
    app.manage(EnrolWindow {
        session: Arc::new(Session::new(RunningWorker(running))),
        task: Mutex::new(None),
    });
}

/// A second click on "Connect worker…" brings the open window forward instead of opening another.
pub fn open(app: &AppHandle) {
    if let Some(existing) = app.get_webview_window(LABEL) {
        let _ = existing.unminimize();
        let _ = existing.set_focus();
        return;
    }
    let state = app.state::<EnrolWindow>();
    state.session.reset();
    let built = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App(PAGE.into()))
        .title("Connect worker")
        .inner_size(480.0, 520.0)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .center()
        .build();
    match built {
        Ok(window) => keep_open_while_connecting(&window, state.session.clone()),
        Err(error) => eprintln!("cannot open the enrolment window: {error}"),
    }
}

fn keep_open_while_connecting(window: &WebviewWindow, session: Arc<Session<RunningWorker>>) {
    let _ = window.set_focus();
    window.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            if !session.can_close() {
                api.prevent_close();
            }
        }
    });
}

/// For Quit and for the system ending the app: the check that waits on the board is dropped and the window goes.
pub fn close(app: &AppHandle) {
    if let Some(state) = app.try_state::<EnrolWindow>() {
        if let Some(task) = state.task.lock().expect("enrol task lock").take() {
            task.abort();
        }
    }
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.destroy();
    }
}

#[tauri::command]
pub async fn enrol_submit(
    state: State<'_, EnrolWindow>,
    token: String,
    data_key: String,
) -> Result<Phase, String> {
    let session = state.session.clone();
    let task = tokio::spawn(async move { session.submit(&token, &data_key).await });
    *state.task.lock().expect("enrol task lock") = Some(task.abort_handle());
    task.await.map_err(|_| "the app is quitting".to_owned())
}

/// Refused while the board is being asked.
#[tauri::command]
pub fn enrol_cancel(app: AppHandle, state: State<'_, EnrolWindow>) -> bool {
    if !state.session.can_close() {
        return false;
    }
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.close();
    }
    true
}
