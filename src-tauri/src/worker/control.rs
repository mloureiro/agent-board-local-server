use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use super::check;
use super::credentials::{discard_pending, enrolled, pending_present};
use super::enrol::{enrol, BoardInit, EnrolError, InitReport, InitRunner};
use super::layout;
use super::processes::{Processes, WorkerSetup};
use super::shutdown::{kill_stragglers, pause_runs};
use crate::secrets::SecretStore;
use crate::supervisor::table::{PROXY, WORKER_LISTEN};
use crate::supervisor::{Status, StatusBoard};

const START_PAUSE_GRACE: Duration = Duration::from_secs(5);
const QUIT_PAUSE_GRACE: Duration = Duration::from_secs(20);

#[derive(Default)]
struct State {
    started: bool,
    /// Detached runs outlive the supervised processes, so once the worker has started, stopping it still owes a pause.
    runs_possible: bool,
    stopping: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enrolled {
    pub machine: Option<String>,
}

pub trait Host: Send + Sync {
    fn stop_processes(&self) -> impl Future<Output = ()> + Send;
    fn start_processes(&self) -> impl Future<Output = ()> + Send;
    fn pause_runs(&self, grace: Duration) -> impl Future<Output = ()> + Send;
    fn kill_stragglers(&self) -> impl Future<Output = ()> + Send;
    fn web_json_names(&self, url: &str) -> bool;
}

pub struct ProcessHost {
    setup: Arc<WorkerSetup>,
    processes: Processes,
}

impl Host for ProcessHost {
    async fn stop_processes(&self) {
        self.processes.stop().await;
    }

    async fn start_processes(&self) {
        self.processes.start().await;
    }

    async fn pause_runs(&self, grace: Duration) {
        pause_runs(&self.setup, grace).await;
    }

    async fn kill_stragglers(&self) {
        kill_stragglers(&self.setup.paths.worker_run_dir()).await;
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
}

pub struct WorkerControl<H = ProcessHost, I = BoardInit> {
    host: H,
    secrets: Arc<dyn SecretStore>,
    board: Arc<StatusBoard>,
    init: I,
    state: Mutex<State>,
}

impl WorkerControl {
    pub fn new(setup: WorkerSetup, secrets: Arc<dyn SecretStore>, board: Arc<StatusBoard>) -> Self {
        let init = BoardInit {
            board: setup.board.clone(),
            env: setup.env.clone(),
        };
        let setup = Arc::new(setup);
        let host = ProcessHost {
            processes: Processes::new(setup.clone(), board.clone()),
            setup,
        };
        Self::with_parts(host, init, secrets, board)
    }

    pub fn prepare(
        &self,
        bundled_board: &std::path::Path,
        app_executable: &std::path::Path,
    ) -> Result<(), String> {
        layout::ensure(&self.host.setup.paths, bundled_board, app_executable)
            .map_err(|error| error.to_string())?;
        discard_pending(self.secrets.as_ref()).map_err(|error| error.to_string())
    }

    pub fn check_log(&self) -> std::path::PathBuf {
        self.host.setup.paths.log_file("worker-check")
    }

    pub async fn check(&self) -> usize {
        let setup = &self.host.setup;
        check::run(&setup.board, &setup.env, &self.check_log()).await
    }
}

impl<H: Host, I: InitRunner> WorkerControl<H, I> {
    pub fn with_parts(
        host: H,
        init: I,
        secrets: Arc<dyn SecretStore>,
        board: Arc<StatusBoard>,
    ) -> Self {
        Self {
            host,
            secrets,
            board,
            init,
            state: Mutex::default(),
        }
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
        self.host.pause_runs(START_PAUSE_GRACE).await;
        if !self.host.web_json_names(url) {
            if let Err(message) = self.init.run_init(url).await {
                return self.blocked(format!("board init: {message}"));
            }
        }
        self.host.start_processes().await;
        state.started = true;
        state.runs_possible = true;
    }

    pub async fn enrol(&self, token: &str, data_key: &str) -> Result<Enrolled, EnrolError> {
        let mut state = self.state.lock().await;
        if state.stopping {
            return Err(EnrolError::new("the app is quitting"));
        }
        let Some(url) = self.proxy_url() else {
            return Err(EnrolError::new("the board is not running yet"));
        };
        let was_started = state.started;
        if was_started {
            self.host.stop_processes().await;
            state.started = false;
            self.host.pause_runs(START_PAUSE_GRACE).await;
        }
        let outcome = enrol(self.secrets.as_ref(), &self.init, &url, token, data_key).await;
        match outcome {
            Ok(InitReport { machine }) => {
                self.connect(&mut state, &url).await;
                Ok(Enrolled { machine })
            }
            Err(error) => {
                if was_started {
                    self.resume_with_enrolled_pair(&mut state, &url).await;
                }
                Err(error)
            }
        }
    }

    async fn resume_with_enrolled_pair(&self, state: &mut State, url: &str) {
        match pending_present(self.secrets.as_ref()) {
            Ok(false) => self.connect(state, url).await,
            Ok(true) => self.blocked("a refused credential pair is still in the Keychain".into()),
            Err(error) => self.blocked(error.to_string()),
        }
    }

    pub async fn stop(&self) {
        let mut state = self.state.lock().await;
        state.stopping = true;
        self.host.stop_processes().await;
        if state.runs_possible {
            self.host.pause_runs(QUIT_PAUSE_GRACE).await;
        }
        self.host.kill_stragglers().await;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use crate::paths::AppPaths;
    use crate::secrets::testing::DeletesFailWhenArmed;
    use crate::secrets::MemoryStore;
    use crate::worker::credentials::{DATA_KEY_ACCOUNT, TOKEN_ACCOUNT};

    use super::*;

    type Events = Arc<StdMutex<Vec<&'static str>>>;

    struct FakeHost {
        events: Events,
        web_json_url: &'static str,
    }

    impl Host for FakeHost {
        async fn stop_processes(&self) {
            self.events.lock().unwrap().push("stop");
        }

        async fn start_processes(&self) {
            self.events.lock().unwrap().push("start");
        }

        async fn pause_runs(&self, _grace: Duration) {
            self.events.lock().unwrap().push("pause");
        }

        async fn kill_stragglers(&self) {
            self.events.lock().unwrap().push("kill");
        }

        fn web_json_names(&self, url: &str) -> bool {
            url == self.web_json_url
        }
    }

    enum Init {
        Accepts,
        Refuses,
        RefusesAndLocksTheStore(Arc<DeletesFailWhenArmed>),
        Hangs,
    }

    struct FakeInit {
        events: Events,
        behaviour: Init,
    }

    impl InitRunner for FakeInit {
        async fn run_init(&self, _url: &str) -> Result<InitReport, String> {
            self.events.lock().unwrap().push("init");
            match &self.behaviour {
                Init::Accepts => Ok(InitReport {
                    machine: Some("tnk_one".into()),
                }),
                Init::Refuses => Err("refused".into()),
                Init::RefusesAndLocksTheStore(store) => {
                    store.arm();
                    Err("refused".into())
                }
                Init::Hangs => std::future::pending().await,
            }
        }
    }

    struct Rig {
        control: WorkerControl<FakeHost, FakeInit>,
        events: Events,
        board: Arc<StatusBoard>,
    }

    fn rig(behaviour: Init, store: Arc<dyn SecretStore>) -> Rig {
        rig_with_web_json(behaviour, store, "http://127.0.0.1:4785")
    }

    fn rig_with_web_json(
        behaviour: Init,
        store: Arc<dyn SecretStore>,
        web_json_url: &'static str,
    ) -> Rig {
        let events: Events = Arc::default();
        let board = Arc::new(StatusBoard::default());
        board.set(PROXY, Status::Running { port: 4785 });
        let control = WorkerControl::with_parts(
            FakeHost {
                events: events.clone(),
                web_json_url,
            },
            FakeInit {
                events: events.clone(),
                behaviour,
            },
            store,
            board.clone(),
        );
        Rig {
            control,
            events,
            board,
        }
    }

    fn enrolled_store() -> Arc<MemoryStore> {
        let store = Arc::new(MemoryStore::default());
        store.set(TOKEN_ACCOUNT, "abm_old").unwrap();
        store.set(DATA_KEY_ACCOUNT, "abk1_old").unwrap();
        store
    }

    async fn started(rig: &Rig) {
        rig.control.proxy_ready().await;
        rig.events.lock().unwrap().clear();
    }

    fn seen(rig: &Rig) -> Vec<&'static str> {
        rig.events.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn a_running_worker_is_stopped_and_its_runs_paused_before_the_new_pair_is_checked() {
        let rig = rig(Init::Accepts, enrolled_store());
        started(&rig).await;

        rig.control.enrol("abm_new", "abk1_new").await.unwrap();

        assert_eq!(seen(&rig), ["stop", "pause", "init", "pause", "start"]);
    }

    #[tokio::test]
    async fn an_accepted_enrolment_reports_the_machine_the_board_named() {
        let rig = rig(Init::Accepts, enrolled_store());
        started(&rig).await;

        let done = rig.control.enrol("abm_new", "abk1_new").await.unwrap();

        assert_eq!(done.machine.as_deref(), Some("tnk_one"));
    }

    #[tokio::test]
    async fn after_a_refusal_the_worker_resumes_with_the_enrolled_pair() {
        let rig = rig(Init::Refuses, enrolled_store());
        started(&rig).await;

        rig.control.enrol("abm_new", "abk1_new").await.unwrap_err();

        assert_eq!(seen(&rig), ["stop", "pause", "init", "pause", "start"]);
    }

    #[tokio::test]
    async fn the_worker_stays_down_while_a_refused_pair_cannot_be_removed() {
        let store = Arc::new(DeletesFailWhenArmed::default());
        store.set(TOKEN_ACCOUNT, "abm_old").unwrap();
        store.set(DATA_KEY_ACCOUNT, "abk1_old").unwrap();
        let rig = rig(Init::RefusesAndLocksTheStore(store.clone()), store);
        started(&rig).await;

        rig.control.enrol("abm_new", "abk1_new").await.unwrap_err();

        assert_eq!(seen(&rig), ["stop", "pause", "init"]);
        assert!(matches!(
            rig.board.get(WORKER_LISTEN),
            Some(Status::Blocked { .. })
        ));
    }

    #[tokio::test]
    async fn a_first_enrolment_has_nothing_to_stop_or_pause_before_the_check() {
        let rig = rig(Init::Accepts, Arc::new(MemoryStore::default()));
        rig.control.proxy_ready().await;

        rig.control.enrol("abm_new", "abk1_new").await.unwrap();

        assert_eq!(seen(&rig), ["init", "pause", "start"]);
    }

    #[tokio::test]
    async fn quitting_during_a_re_enrolment_still_pauses_the_runs_before_killing_stragglers() {
        let rig = rig(Init::Hangs, enrolled_store());
        started(&rig).await;

        let interrupted = tokio::time::timeout(
            Duration::from_millis(100),
            rig.control.enrol("abm_new", "abk1_new"),
        )
        .await;
        assert!(interrupted.is_err());
        rig.events.lock().unwrap().clear();
        rig.control.stop().await;

        assert_eq!(seen(&rig), ["stop", "pause", "kill"]);
    }

    #[tokio::test]
    async fn quitting_a_worker_that_never_started_pauses_nothing() {
        let rig = rig(Init::Accepts, Arc::new(MemoryStore::default()));
        rig.control.proxy_ready().await;

        rig.control.stop().await;

        assert_eq!(seen(&rig), ["stop", "kill"]);
    }
    #[tokio::test]
    async fn a_moved_proxy_port_reruns_board_init_before_the_worker_starts() {
        let rig = rig_with_web_json(Init::Accepts, enrolled_store(), "http://127.0.0.1:4790");

        rig.control.proxy_ready().await;

        assert_eq!(seen(&rig), ["pause", "init", "start"]);
    }

    #[tokio::test]
    async fn a_web_json_naming_the_running_proxy_starts_the_worker_without_board_init() {
        let rig = rig(Init::Accepts, enrolled_store());

        rig.control.proxy_ready().await;

        assert_eq!(seen(&rig), ["pause", "start"]);
    }

    #[tokio::test]
    async fn a_refused_reinit_at_start_leaves_the_worker_blocked_and_stopped() {
        let rig = rig_with_web_json(Init::Refuses, enrolled_store(), "http://127.0.0.1:4790");

        rig.control.proxy_ready().await;

        assert_eq!(seen(&rig), ["pause", "init"]);
        let Some(Status::Blocked { reason }) = rig.board.get(WORKER_LISTEN) else {
            panic!("the worker should be blocked");
        };
        assert!(reason.starts_with("board init:"));
    }

    fn host_over(root: &Path) -> ProcessHost {
        let setup = Arc::new(WorkerSetup {
            paths: AppPaths::new(root),
            board: PathBuf::new(),
            env: BTreeMap::new(),
            lockf: PathBuf::new(),
        });
        ProcessHost {
            processes: Processes::new(setup.clone(), Arc::new(StatusBoard::default())),
            setup,
        }
    }

    fn web_json(root: &Path, contents: &str) {
        let dir = AppPaths::new(root).worker_config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("web.json"), contents).unwrap();
    }

    const URL: &str = "http://127.0.0.1:4785";

    #[test]
    fn a_web_json_naming_the_proxy_matches_with_or_without_a_trailing_slash() {
        let root = tempfile::tempdir().unwrap();
        let host = host_over(root.path());

        web_json(root.path(), r#"{"url":"http://127.0.0.1:4785"}"#);
        assert!(host.web_json_names(URL));
        web_json(root.path(), r#"{"url":"http://127.0.0.1:4785/"}"#);
        assert!(host.web_json_names(URL));
    }

    #[test]
    fn a_web_json_naming_another_port_does_not_match() {
        let root = tempfile::tempdir().unwrap();
        web_json(root.path(), r#"{"url":"http://127.0.0.1:4790"}"#);

        assert!(!host_over(root.path()).web_json_names(URL));
    }

    #[test]
    fn a_missing_web_json_names_nothing() {
        let root = tempfile::tempdir().unwrap();

        assert!(!host_over(root.path()).web_json_names(URL));
    }

    #[test]
    fn an_unreadable_web_json_names_nothing() {
        let root = tempfile::tempdir().unwrap();
        let host = host_over(root.path());

        for contents in ["not json", r#"{"url":4785}"#, "{}"] {
            web_json(root.path(), contents);
            assert!(!host.web_json_names(URL), "{contents}");
        }
    }
}
