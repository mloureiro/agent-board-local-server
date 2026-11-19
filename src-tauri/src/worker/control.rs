use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use super::credentials::{discard_pending, enrolled};
use super::enrol::{enrol, BoardInit, EnrolError, InitRunner};
use super::layout;
use super::processes::{Processes, WorkerSetup, WORKER_LISTEN};
use super::shutdown::{kill_stragglers, pause_runs};
use crate::runtime::PROXY;
use crate::secrets::SecretStore;
use crate::supervisor::{Status, StatusBoard};

const START_PAUSE_GRACE: Duration = Duration::from_secs(5);
const QUIT_PAUSE_GRACE: Duration = Duration::from_secs(20);

#[derive(Default)]
struct State {
    started: bool,
    stopping: bool,
}

pub struct WorkerControl {
    setup: Arc<WorkerSetup>,
    secrets: Arc<dyn SecretStore>,
    board: Arc<StatusBoard>,
    processes: Processes,
    init: BoardInit,
    machine: String,
    state: Mutex<State>,
}

impl WorkerControl {
    pub fn new(
        setup: WorkerSetup,
        secrets: Arc<dyn SecretStore>,
        board: Arc<StatusBoard>,
        machine: String,
    ) -> Self {
        let init = BoardInit {
            board: setup.board.clone(),
            env: setup.env.clone(),
        };
        let setup = Arc::new(setup);
        Self {
            processes: Processes::new(setup.clone(), board.clone()),
            setup,
            secrets,
            board,
            init,
            machine,
            state: Mutex::default(),
        }
    }

    pub fn prepare(
        &self,
        bundled_board: &std::path::Path,
        app_executable: &std::path::Path,
    ) -> Result<(), String> {
        layout::ensure(&self.setup.paths, bundled_board, app_executable)
            .map_err(|error| error.to_string())?;
        discard_pending(self.secrets.as_ref()).map_err(|error| error.to_string())
    }

    fn blocked(&self, reason: String) {
        self.board.set(WORKER_LISTEN, Status::Blocked { reason });
    }

    fn proxy_url(&self) -> Option<String> {
        match self.board.get(PROXY) {
            Some(Status::Running { port }) => Some(format!("http://127.0.0.1:{port}")),
            _ => None,
        }
    }

    pub async fn proxy_ready(&self) {
        let mut state = self.state.lock().await;
        let Some(url) = self.proxy_url() else {
            return;
        };
        if state.stopping || state.started {
            return;
        }
        match enrolled(self.secrets.as_ref()) {
            Ok(true) => self.connect(&mut state, &url).await,
            Ok(false) => self.board.set(WORKER_LISTEN, Status::NotConnected),
            Err(error) => self.blocked(error.to_string()),
        }
    }

    async fn connect(&self, state: &mut State, url: &str) {
        pause_runs(&self.setup, START_PAUSE_GRACE).await;
        if !self.web_json_names(url) {
            if let Err(message) = self.init.run_init(url, &self.machine).await {
                return self.blocked(format!("board init: {message}"));
            }
        }
        self.processes.start().await;
        state.started = true;
    }

    fn web_json_names(&self, url: &str) -> bool {
        std::fs::read(self.setup.paths.worker_config_dir().join("web.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|json| {
                json.get("url")?
                    .as_str()
                    .map(|seen| seen.trim_end_matches('/') == url)
            })
            .unwrap_or(false)
    }

    pub async fn enrol(&self, token: &str, data_key: &str) -> Result<(), EnrolError> {
        let mut state = self.state.lock().await;
        if state.stopping {
            return Err(EnrolError::new("the app is quitting"));
        }
        let Some(url) = self.proxy_url() else {
            return Err(EnrolError::new("the board is not running yet"));
        };
        enrol(
            self.secrets.as_ref(),
            &self.init,
            &url,
            &self.machine,
            token,
            data_key,
        )
        .await?;
        if state.started {
            self.processes.stop().await;
            state.started = false;
        }
        self.connect(&mut state, &url).await;
        Ok(())
    }

    pub async fn stop(&self) {
        let mut state = self.state.lock().await;
        state.stopping = true;
        self.processes.stop().await;
        if state.started {
            pause_runs(&self.setup, QUIT_PAUSE_GRACE).await;
        }
        kill_stragglers(&self.setup.paths.worker_run_dir()).await;
    }
}
