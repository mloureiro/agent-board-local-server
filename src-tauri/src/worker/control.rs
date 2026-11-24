use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use super::check;
use super::credentials::{
    discard_pending, enrolled, forget, promote_pending, read_enrolled, restore_enrolled, Pair,
    PromoteFailure,
};
use super::enrol::{validate, BoardInit, EnrolError, InitReport, InitRunner};
use super::env::with_path;
use super::layout;
use super::processes::{Processes, WorkerSetup, WorkerStart};
use super::shutdown::{kill_stragglers, pause_runs};
use super::units;
use crate::activity::{Activity, Guard, Kind};
use crate::secrets::{SecretError, SecretStore};
use crate::supervisor::table::{PROXY, WORKER, WORKER_LISTEN};
use crate::supervisor::{Status, StatusBoard};

const START_PAUSE_GRACE: Duration = Duration::from_secs(5);
const QUIT_PAUSE_GRACE: Duration = Duration::from_secs(20);

#[derive(Default)]
struct State {
    started: bool,
    /// Detached runs outlive the supervised processes, so once the worker has started, stopping it still owes a pause.
    runs_possible: bool,
    stopping: bool,
    env: Option<Arc<BTreeMap<String, String>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enrolled {
    pub machine: Option<String>,
}

#[derive(Debug)]
pub enum ActionError {
    Busy(Kind),
    Failed(String),
}

impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy(Kind::Quit) => f.write_str("the app is quitting"),
            Self::Busy(kind) => write!(f, "{kind} is running"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ActionError {}

impl From<SecretError> for ActionError {
    fn from(error: SecretError) -> Self {
        Self::Failed(error.to_string())
    }
}

pub trait Host: Send + Sync {
    fn stop_processes(&self) -> impl Future<Output = ()> + Send;
    fn read_units(&self) -> impl Future<Output = Result<WorkerStart, String>> + Send;
    fn start_processes(&self, start: &WorkerStart) -> impl Future<Output = ()> + Send;
    fn pause_runs(
        &self,
        env: &BTreeMap<String, String>,
        grace: Duration,
    ) -> impl Future<Output = ()> + Send;
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

    async fn read_units(&self) -> Result<WorkerStart, String> {
        let setup = &self.setup;
        let units = units::read(&setup.board, &setup.base_env, &setup.app_log).await?;
        let env = with_path(
            &setup.base_env,
            &setup.paths,
            &setup.worker_path,
            &units.path,
        );
        Ok(WorkerStart {
            env: Arc::new(env),
            units,
        })
    }

    async fn start_processes(&self, start: &WorkerStart) {
        self.processes.start(start).await;
    }

    async fn pause_runs(&self, env: &BTreeMap<String, String>, grace: Duration) {
        pause_runs(&self.setup, env, grace).await;
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
    activity: Arc<Activity>,
    state: Mutex<State>,
}

impl WorkerControl {
    pub fn new(
        setup: WorkerSetup,
        secrets: Arc<dyn SecretStore>,
        board: Arc<StatusBoard>,
        activity: Arc<Activity>,
    ) -> Self {
        let init = BoardInit {
            board: setup.board.clone(),
            pending_credentials: setup.paths.worker_credentials_pending(),
        };
        let setup = Arc::new(setup);
        let host = ProcessHost {
            processes: Processes::new(setup.clone(), board.clone()),
            setup,
        };
        Self::with_parts(host, init, secrets, board, activity)
    }

    pub fn prepare(
        &self,
        bundled_board: &std::path::Path,
        app_executable: &std::path::Path,
    ) -> Result<(), String> {
        let laid_out = layout::ensure(&self.host.setup.paths, bundled_board, app_executable)
            .map_err(|error| error.to_string());
        let discarded = discard_pending(self.secrets.as_ref()).map_err(|error| error.to_string());
        laid_out.and(discarded)
    }

    pub fn check_log(&self) -> std::path::PathBuf {
        self.host.setup.paths.log_file("worker-check")
    }

    pub async fn check(&self) -> Result<usize, ActionError> {
        let _gate = self.begin(Kind::Recovery)?;
        let setup = &self.host.setup;
        let started = self.state.lock().await.env.clone();
        let env = match started {
            Some(env) => env,
            None => match self.host.read_units().await {
                Ok(start) => start.env,
                Err(reason) => return Ok(check::fail(&self.check_log(), "worker units", &reason)),
            },
        };
        Ok(check::run(&setup.board, &env, &self.check_log()).await)
    }
}

impl<H: Host, I: InitRunner> WorkerControl<H, I> {
    pub fn with_parts(
        host: H,
        init: I,
        secrets: Arc<dyn SecretStore>,
        board: Arc<StatusBoard>,
        activity: Arc<Activity>,
    ) -> Self {
        Self {
            host,
            secrets,
            board,
            init,
            activity,
            state: Mutex::default(),
        }
    }

    fn begin(&self, kind: Kind) -> Result<Guard, ActionError> {
        self.activity.begin(kind).map_err(ActionError::Busy)
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
        let start = match self.host.read_units().await {
            Ok(start) => start,
            Err(reason) => return self.blocked(format!("worker units: {reason}")),
        };
        state.env = Some(start.env.clone());
        self.host.pause_runs(&start.env, START_PAUSE_GRACE).await;
        if !self.host.web_json_names(url) {
            if let Err(message) = self.init.run_init(url, Pair::Enrolled, &start.env).await {
                return self.blocked(format!("board init: {message}"));
            }
        }
        self.host.start_processes(&start).await;
        state.started = true;
        state.runs_possible = true;
    }

    pub async fn enrol(&self, token: &str, data_key: &str) -> Result<Enrolled, EnrolError> {
        let _gate = self
            .begin(Kind::Enrolment)
            .map_err(|error| EnrolError::new(error.to_string()))?;
        let mut state = self.state.lock().await;
        if state.stopping {
            return Err(EnrolError::new("the app is quitting"));
        }
        let Some(url) = self.proxy_url() else {
            return Err(EnrolError::new("the board is not running yet"));
        };
        let start = self
            .host
            .read_units()
            .await
            .map_err(|reason| EnrolError::new(format!("worker units: {reason}")))?;
        let InitReport { machine } = validate(
            self.secrets.as_ref(),
            &self.init,
            &url,
            token,
            data_key,
            &start.env,
        )
        .await?;
        let previous = read_enrolled(self.secrets.as_ref())?;
        let was_started = state.started;
        if was_started {
            self.host.stop_processes().await;
            state.started = false;
        }
        state.env = Some(start.env.clone());
        self.host.pause_runs(&start.env, START_PAUSE_GRACE).await;
        match promote_pending(self.secrets.as_ref()) {
            Ok(()) | Err(PromoteFailure::Discard(_)) => {}
            Err(PromoteFailure::Token(error)) => {
                return self
                    .abandon_promotion(&mut state, &start, was_started, error.to_string())
                    .await;
            }
            Err(PromoteFailure::DataKey(error)) => {
                if let Err(undo) = self.restore(previous.as_ref()) {
                    let mut message =
                        format!("{error}; the previous pair could not be restored ({undo})");
                    let cleaned =
                        forget(self.secrets.as_ref()).and(discard_pending(self.secrets.as_ref()));
                    if let Err(cleanup) = cleaned {
                        message.push_str(&format!(
                            "; the mixed pair could not be removed from the Keychain ({cleanup})"
                        ));
                    }
                    self.blocked(message.clone());
                    return Err(EnrolError::new(message));
                }
                return self
                    .abandon_promotion(&mut state, &start, was_started, error.to_string())
                    .await;
            }
        }
        self.host.start_processes(&start).await;
        state.started = true;
        state.runs_possible = true;
        Ok(Enrolled { machine })
    }

    fn restore(
        &self,
        previous: Option<&super::credentials::Credentials>,
    ) -> Result<(), SecretError> {
        match previous {
            Some(pair) => restore_enrolled(self.secrets.as_ref(), pair),
            None => {
                forget(self.secrets.as_ref())?;
                self.board.set(WORKER_LISTEN, Status::NotConnected);
                Ok(())
            }
        }
    }

    async fn abandon_promotion(
        &self,
        state: &mut State,
        start: &WorkerStart,
        was_started: bool,
        message: String,
    ) -> Result<Enrolled, EnrolError> {
        let _ = discard_pending(self.secrets.as_ref());
        if was_started {
            self.host.start_processes(start).await;
            state.started = true;
        }
        Err(EnrolError::new(message))
    }

    async fn pause_with_last_env(&self, state: &State) {
        if let Some(env) = &state.env {
            self.host.pause_runs(env, QUIT_PAUSE_GRACE).await;
        }
    }

    pub async fn restart(&self) -> Result<(), ActionError> {
        let _gate = self.begin(Kind::Recovery)?;
        let mut state = self.state.lock().await;
        if state.stopping {
            return Err(ActionError::Failed("the app is quitting".into()));
        }
        if state.started {
            self.host.stop_processes().await;
            state.started = false;
            match self.host.read_units().await {
                Ok(start) => {
                    state.env = Some(start.env.clone());
                    self.host.start_processes(&start).await;
                    state.started = true;
                }
                Err(reason) => self.blocked(format!("worker units: {reason}")),
            }
            return Ok(());
        }
        if !enrolled(self.secrets.as_ref())? {
            return Err(ActionError::Failed("the worker is not connected".into()));
        }
        let Some(url) = self.proxy_url() else {
            return Err(ActionError::Failed("the board is not running yet".into()));
        };
        self.connect(&mut state, &url).await;
        Ok(())
    }

    pub async fn disconnect(&self) -> Result<(), ActionError> {
        let _gate = self.begin(Kind::Recovery)?;
        let mut state = self.state.lock().await;
        if state.stopping {
            return Err(ActionError::Failed("the app is quitting".into()));
        }
        self.host.stop_processes().await;
        state.started = false;
        if state.runs_possible {
            self.pause_with_last_env(&state).await;
            state.runs_possible = false;
        }
        let forgotten = forget(self.secrets.as_ref()).and(discard_pending(self.secrets.as_ref()));
        if let Err(error) = forgotten {
            self.blocked(error.to_string());
            return Err(error.into());
        }
        self.board.set(WORKER_LISTEN, Status::NotConnected);
        self.board.set(WORKER, Status::Stopped);
        Ok(())
    }

    pub async fn stop(&self) {
        let mut state = self.state.lock().await;
        state.stopping = true;
        self.host.stop_processes().await;
        if state.runs_possible {
            self.pause_with_last_env(&state).await;
        }
        self.host.kill_stragglers().await;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use crate::applog::LogFile;
    use crate::paths::AppPaths;
    use crate::secrets::testing::FailsAtCall;
    use crate::secrets::MemoryStore;
    use crate::worker::credentials::{
        pending_present, write_pending, DATA_KEY_ACCOUNT, TOKEN_ACCOUNT,
    };

    use super::*;
    use crate::worker::units::{ServiceUnit, TickUnit, WorkerUnits};

    type Events = Arc<StdMutex<Vec<&'static str>>>;

    struct FakeHost {
        events: Events,
        web_json_url: &'static str,
        units_readable: Arc<AtomicBool>,
        reads: AtomicUsize,
        paused_with: Arc<StdMutex<Vec<String>>>,
        started_with: Arc<StdMutex<Vec<String>>>,
    }

    fn path_of(env: &BTreeMap<String, String>) -> String {
        env.get("PATH").cloned().unwrap_or_default()
    }

    impl Host for FakeHost {
        async fn stop_processes(&self) {
            self.events.lock().unwrap().push("stop");
        }

        async fn read_units(&self) -> Result<WorkerStart, String> {
            self.events.lock().unwrap().push("units");
            if !self.units_readable.load(Ordering::SeqCst) {
                return Err("the script exited with 1".into());
            }
            let read = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
            let service = |row| ServiceUnit {
                row,
                program: "/cli/board".into(),
                args: Vec::new(),
            };
            Ok(WorkerStart {
                env: Arc::new(BTreeMap::from([(
                    "PATH".to_owned(),
                    format!("/read-{read}"),
                )])),
                units: WorkerUnits {
                    path: "/units".into(),
                    dispatch: TickUnit {
                        row: crate::supervisor::table::DISPATCH,
                        program: "/cli/board".into(),
                        args: Vec::new(),
                        every: Duration::from_secs(120),
                        limit: Duration::from_secs(110),
                        grace: Duration::from_secs(10),
                        lock_held: 75,
                    },
                    listen: service(WORKER_LISTEN),
                    watch: service(crate::supervisor::table::WORKER_WATCH),
                },
            })
        }

        async fn start_processes(&self, start: &WorkerStart) {
            self.events.lock().unwrap().push("start");
            self.started_with.lock().unwrap().push(path_of(&start.env));
        }

        async fn pause_runs(&self, env: &BTreeMap<String, String>, _grace: Duration) {
            self.events.lock().unwrap().push("pause");
            self.paused_with.lock().unwrap().push(path_of(env));
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
        Hangs,
    }

    struct FakeInit {
        events: Events,
        pairs: Arc<StdMutex<Vec<Pair>>>,
        init_with: Arc<StdMutex<Vec<String>>>,
        behaviour: Init,
    }

    impl InitRunner for FakeInit {
        async fn run_init(
            &self,
            _url: &str,
            pair: Pair,
            env: &BTreeMap<String, String>,
        ) -> Result<InitReport, String> {
            self.events.lock().unwrap().push("init");
            self.pairs.lock().unwrap().push(pair);
            self.init_with.lock().unwrap().push(path_of(env));
            match &self.behaviour {
                Init::Accepts => Ok(InitReport {
                    machine: Some("tnk_one".into()),
                }),
                Init::Refuses => Err("refused".into()),
                Init::Hangs => std::future::pending().await,
            }
        }
    }

    struct Rig {
        control: WorkerControl<FakeHost, FakeInit>,
        events: Events,
        pairs: Arc<StdMutex<Vec<Pair>>>,
        units_readable: Arc<AtomicBool>,
        paused_with: Arc<StdMutex<Vec<String>>>,
        started_with: Arc<StdMutex<Vec<String>>>,
        init_with: Arc<StdMutex<Vec<String>>>,
        board: Arc<StatusBoard>,
        store: Arc<dyn SecretStore>,
        activity: Arc<Activity>,
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
        let pairs = Arc::default();
        let units_readable = Arc::new(AtomicBool::new(true));
        let paused_with = Arc::<StdMutex<Vec<String>>>::default();
        let started_with = Arc::<StdMutex<Vec<String>>>::default();
        let init_with = Arc::<StdMutex<Vec<String>>>::default();
        let activity = Arc::new(Activity::default());
        let board = Arc::new(StatusBoard::default());
        board.set(PROXY, Status::Running { port: 4785 });
        let control = WorkerControl::with_parts(
            FakeHost {
                events: events.clone(),
                web_json_url,
                units_readable: units_readable.clone(),
                reads: AtomicUsize::new(0),
                paused_with: paused_with.clone(),
                started_with: started_with.clone(),
            },
            FakeInit {
                events: events.clone(),
                pairs: Arc::clone(&pairs),
                init_with: init_with.clone(),
                behaviour,
            },
            store.clone(),
            board.clone(),
            activity.clone(),
        );
        Rig {
            control,
            events,
            pairs,
            units_readable,
            paused_with,
            started_with,
            init_with,
            board,
            store,
            activity,
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
    async fn an_accepted_reconnect_checks_first_then_stops_pauses_and_starts_with_the_new_pair() {
        let rig = rig(Init::Accepts, enrolled_store());
        started(&rig).await;

        rig.control.enrol("abm_new", "abk1_new").await.unwrap();

        assert_eq!(seen(&rig), ["units", "init", "stop", "pause", "start"]);
        assert_eq!(*rig.pairs.lock().unwrap(), [Pair::Pending]);
        assert_eq!(
            rig.store.get(TOKEN_ACCOUNT).unwrap().as_deref(),
            Some("abm_new")
        );
        assert!(!pending_present(rig.store.as_ref()).unwrap());
    }

    #[tokio::test]
    async fn an_accepted_enrolment_reports_the_machine_the_board_named() {
        let rig = rig(Init::Accepts, enrolled_store());
        started(&rig).await;

        let done = rig.control.enrol("abm_new", "abk1_new").await.unwrap();

        assert_eq!(done.machine.as_deref(), Some("tnk_one"));
    }

    #[tokio::test]
    async fn a_refused_reconnect_runs_only_the_check_and_leaves_the_worker_and_its_pair_alone() {
        let rig = rig(Init::Refuses, enrolled_store());
        started(&rig).await;
        let before = rig.board.get(WORKER_LISTEN);

        rig.control.enrol("abm_new", "abk1_new").await.unwrap_err();

        assert_eq!(seen(&rig), ["units", "init"]);
        assert_eq!(rig.board.get(WORKER_LISTEN), before);
        assert_eq!(
            rig.store.get(TOKEN_ACCOUNT).unwrap().as_deref(),
            Some("abm_old")
        );
        assert!(!pending_present(rig.store.as_ref()).unwrap());
    }

    #[tokio::test]
    async fn a_first_enrolment_pauses_before_it_starts() {
        let rig = rig(Init::Accepts, Arc::new(MemoryStore::default()));
        rig.control.proxy_ready().await;

        rig.control.enrol("abm_new", "abk1_new").await.unwrap();

        assert_eq!(seen(&rig), ["units", "init", "pause", "start"]);
    }

    #[tokio::test]
    async fn restarting_a_started_worker_stops_and_starts_it_without_pausing_runs() {
        let rig = rig(Init::Accepts, enrolled_store());
        started(&rig).await;

        rig.control.restart().await.unwrap();

        assert_eq!(seen(&rig), ["stop", "units", "start"]);
    }

    #[tokio::test]
    async fn restarting_a_blocked_worker_with_an_enrolled_pair_goes_through_connect_again() {
        let rig = rig_with_web_json(Init::Refuses, enrolled_store(), "http://127.0.0.1:4790");
        rig.control.proxy_ready().await;
        rig.events.lock().unwrap().clear();

        rig.control.restart().await.unwrap();

        assert_eq!(seen(&rig), ["units", "pause", "init"]);
        assert_eq!(*rig.pairs.lock().unwrap(), [Pair::Enrolled, Pair::Enrolled]);
    }

    #[tokio::test]
    async fn restarting_a_worker_that_was_never_enrolled_changes_nothing() {
        let rig = rig(Init::Accepts, Arc::new(MemoryStore::default()));
        rig.control.proxy_ready().await;

        rig.control.restart().await.unwrap_err();

        assert!(seen(&rig).is_empty());
    }

    #[tokio::test]
    async fn disconnecting_stops_the_worker_pauses_its_runs_and_forgets_the_pair() {
        let rig = rig(Init::Accepts, enrolled_store());
        started(&rig).await;

        rig.control.disconnect().await.unwrap();

        assert_eq!(seen(&rig), ["stop", "pause"]);
        assert_eq!(rig.store.get(TOKEN_ACCOUNT).unwrap(), None);
        assert_eq!(rig.store.get(DATA_KEY_ACCOUNT).unwrap(), None);
        assert_eq!(rig.board.get(WORKER_LISTEN), Some(Status::NotConnected));
    }

    #[tokio::test]
    async fn after_a_disconnect_the_next_enrolment_is_a_first_one() {
        let rig = rig(Init::Accepts, enrolled_store());
        started(&rig).await;
        rig.control.disconnect().await.unwrap();
        rig.events.lock().unwrap().clear();

        rig.control.enrol("abm_new", "abk1_new").await.unwrap();

        assert_eq!(seen(&rig), ["units", "init", "pause", "start"]);
    }

    #[tokio::test]
    async fn an_action_is_refused_while_another_activity_holds_the_gate() {
        let rig = rig(Init::Accepts, enrolled_store());
        started(&rig).await;
        let _backup = rig.activity.begin(Kind::Backup).unwrap();

        assert!(matches!(
            rig.control.restart().await,
            Err(ActionError::Busy(Kind::Backup))
        ));
        assert!(matches!(
            rig.control.disconnect().await,
            Err(ActionError::Busy(Kind::Backup))
        ));
        assert!(rig.control.enrol("abm_new", "abk1_new").await.is_err());
        assert!(seen(&rig).is_empty());
    }

    #[tokio::test]
    async fn the_gate_is_free_again_once_an_action_has_finished() {
        let rig = rig(Init::Accepts, enrolled_store());
        started(&rig).await;

        rig.control.restart().await.unwrap();

        assert!(rig.activity.begin(Kind::Backup).is_ok());
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

    fn failing_store(old_pair: bool) -> Arc<FailsAtCall> {
        let store = Arc::new(FailsAtCall::default());
        if old_pair {
            store.set(TOKEN_ACCOUNT, "abm_old").unwrap();
            store.set(DATA_KEY_ACCOUNT, "abk1_old").unwrap();
        }
        store
    }

    fn enrolled_pair(rig: &Rig) -> (Option<String>, Option<String>) {
        (
            rig.store.get(TOKEN_ACCOUNT).unwrap(),
            rig.store.get(DATA_KEY_ACCOUNT).unwrap(),
        )
    }

    fn old_pair() -> (Option<String>, Option<String>) {
        (Some("abm_old".into()), Some("abk1_old".into()))
    }

    #[tokio::test]
    async fn a_failed_token_write_discards_the_pending_pair_and_restarts_the_worker_on_the_old_one()
    {
        let store = failing_store(true);
        let rig = rig(Init::Accepts, store.clone());
        started(&rig).await;
        store.count_from_here(&[3], &[]);

        rig.control.enrol("abm_new", "abk1_new").await.unwrap_err();

        assert_eq!(seen(&rig), ["units", "init", "stop", "pause", "start"]);
        assert_eq!(enrolled_pair(&rig), old_pair());
        assert!(!pending_present(rig.store.as_ref()).unwrap());
    }

    #[tokio::test]
    async fn a_failed_token_write_with_no_worker_running_starts_nothing() {
        let store = failing_store(false);
        let rig = rig(Init::Accepts, store.clone());
        rig.control.proxy_ready().await;
        rig.events.lock().unwrap().clear();
        store.count_from_here(&[3], &[]);

        rig.control.enrol("abm_new", "abk1_new").await.unwrap_err();

        assert_eq!(seen(&rig), ["units", "init", "pause"]);
        assert_eq!(enrolled_pair(&rig), (None, None));
        assert!(!pending_present(rig.store.as_ref()).unwrap());
    }

    #[tokio::test]
    async fn a_failed_data_key_write_restores_the_old_pair_discards_the_pending_one_and_restarts() {
        let store = failing_store(true);
        let rig = rig(Init::Accepts, store.clone());
        started(&rig).await;
        store.count_from_here(&[4], &[]);

        rig.control.enrol("abm_new", "abk1_new").await.unwrap_err();

        assert_eq!(seen(&rig), ["units", "init", "stop", "pause", "start"]);
        assert_eq!(enrolled_pair(&rig), old_pair());
        assert!(!pending_present(rig.store.as_ref()).unwrap());
    }

    #[tokio::test]
    async fn a_failed_data_key_write_with_no_old_pair_forgets_the_partial_one_and_shows_not_connected(
    ) {
        let store = failing_store(false);
        let rig = rig(Init::Accepts, store.clone());
        rig.control.proxy_ready().await;
        rig.events.lock().unwrap().clear();
        store.count_from_here(&[4], &[]);

        rig.control.enrol("abm_new", "abk1_new").await.unwrap_err();

        assert_eq!(seen(&rig), ["units", "init", "pause"]);
        assert_eq!(enrolled_pair(&rig), (None, None));
        assert!(!pending_present(rig.store.as_ref()).unwrap());
        assert_eq!(rig.board.get(WORKER_LISTEN), Some(Status::NotConnected));
    }

    fn blocked_reason(rig: &Rig) -> String {
        let Some(Status::Blocked { reason }) = rig.board.get(WORKER_LISTEN) else {
            panic!("the worker should be blocked");
        };
        reason
    }

    #[tokio::test]
    async fn a_failed_restore_blocks_the_worker_and_removes_the_mixed_pair_so_nothing_starts_on_it()
    {
        let store = failing_store(true);
        let rig = rig(Init::Accepts, store.clone());
        started(&rig).await;
        store.count_from_here(&[4, 5], &[]);

        rig.control.enrol("abm_new", "abk1_new").await.unwrap_err();

        assert_eq!(seen(&rig), ["units", "init", "stop", "pause"]);
        blocked_reason(&rig);
        assert_eq!(enrolled_pair(&rig), (None, None));
        assert!(!enrolled(rig.store.as_ref()).unwrap());
        assert!(!pending_present(rig.store.as_ref()).unwrap());

        rig.control.restart().await.unwrap_err();
        assert_eq!(seen(&rig), ["units", "init", "stop", "pause"]);
    }

    #[tokio::test]
    async fn a_failed_forget_after_a_partial_write_blocks_the_worker_and_leaves_it_stopped() {
        let store = failing_store(false);
        let rig = rig(Init::Accepts, store.clone());
        rig.control.proxy_ready().await;
        rig.events.lock().unwrap().clear();
        store.count_from_here(&[4], &[3]);

        rig.control.enrol("abm_new", "abk1_new").await.unwrap_err();

        assert_eq!(seen(&rig), ["units", "init", "pause"]);
        blocked_reason(&rig);
        assert!(!enrolled(rig.store.as_ref()).unwrap());
        assert!(!pending_present(rig.store.as_ref()).unwrap());
    }

    #[tokio::test]
    async fn a_failed_cleanup_after_a_failed_restore_puts_both_errors_in_the_blocked_reason() {
        let store = failing_store(true);
        let rig = rig(Init::Accepts, store.clone());
        started(&rig).await;
        store.count_from_here(&[4, 5], &[3, 5]);

        rig.control.enrol("abm_new", "abk1_new").await.unwrap_err();

        assert_eq!(seen(&rig), ["units", "init", "stop", "pause"]);
        let reason = blocked_reason(&rig);
        assert!(reason.contains("could not be restored"));
        assert!(reason.contains("could not be removed"));
        assert!(!enrolled(rig.store.as_ref()).unwrap());
    }

    #[tokio::test]
    async fn a_failed_pending_discard_still_starts_the_worker_and_reports_the_enrolment() {
        let store = failing_store(true);
        let rig = rig(Init::Accepts, store.clone());
        started(&rig).await;
        store.count_from_here(&[], &[3]);

        let done = rig.control.enrol("abm_new", "abk1_new").await.unwrap();

        assert_eq!(done.machine.as_deref(), Some("tnk_one"));
        assert_eq!(seen(&rig), ["units", "init", "stop", "pause", "start"]);
        assert_eq!(
            enrolled_pair(&rig),
            (Some("abm_new".into()), Some("abk1_new".into()))
        );
    }

    #[tokio::test]
    async fn an_unreadable_old_pair_fails_the_enrolment_before_anything_is_stopped() {
        struct UnreadableEnrolled(MemoryStore);
        impl SecretStore for UnreadableEnrolled {
            fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
                if account == TOKEN_ACCOUNT {
                    return Err(SecretError("locked".into()));
                }
                self.0.get(account)
            }
            fn set(&self, account: &str, value: &str) -> Result<(), SecretError> {
                self.0.set(account, value)
            }
            fn delete(&self, account: &str) -> Result<(), SecretError> {
                self.0.delete(account)
            }
        }
        let rig = rig(
            Init::Accepts,
            Arc::new(UnreadableEnrolled(MemoryStore::default())),
        );
        rig.board.set(WORKER_LISTEN, Status::NotConnected);

        rig.control.enrol("abm_new", "abk1_new").await.unwrap_err();

        assert_eq!(seen(&rig), ["units", "init"]);
    }

    #[test]
    fn prepare_discards_a_leftover_pending_pair_even_when_the_layout_cannot_be_written() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(root.path());
        std::fs::create_dir_all(paths.worker_bin_dir().parent().unwrap()).unwrap();
        let worker_dir = paths.worker_bin_dir().parent().unwrap().to_path_buf();
        std::fs::set_permissions(&worker_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let store = Arc::new(MemoryStore::default());
        write_pending(
            store.as_ref(),
            &super::super::credentials::Credentials {
                token: "abm_new".into(),
                data_key: "abk1_new".into(),
            },
        )
        .unwrap();
        let control = WorkerControl::new(
            WorkerSetup {
                paths,
                board: PathBuf::new(),
                base_env: BTreeMap::new(),
                worker_path: Vec::new(),
                app_log: LogFile::new(PathBuf::new()),
            },
            store.clone(),
            Arc::new(StatusBoard::default()),
            Arc::new(Activity::default()),
        );

        let result = control.prepare(Path::new("/board"), Path::new("/app"));

        std::fs::set_permissions(&worker_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err());
        assert!(!pending_present(store.as_ref()).unwrap());
    }

    #[tokio::test]
    async fn a_moved_proxy_port_reruns_board_init_before_the_worker_starts() {
        let rig = rig_with_web_json(Init::Accepts, enrolled_store(), "http://127.0.0.1:4790");

        rig.control.proxy_ready().await;

        assert_eq!(seen(&rig), ["units", "pause", "init", "start"]);
    }

    #[tokio::test]
    async fn a_web_json_naming_the_running_proxy_starts_the_worker_without_board_init() {
        let rig = rig(Init::Accepts, enrolled_store());

        rig.control.proxy_ready().await;

        assert_eq!(seen(&rig), ["units", "pause", "start"]);
    }

    #[tokio::test]
    async fn a_refused_reinit_at_start_leaves_the_worker_blocked_and_stopped() {
        let rig = rig_with_web_json(Init::Refuses, enrolled_store(), "http://127.0.0.1:4790");

        rig.control.proxy_ready().await;

        assert_eq!(seen(&rig), ["units", "pause", "init"]);
        let Some(Status::Blocked { reason }) = rig.board.get(WORKER_LISTEN) else {
            panic!("the worker should be blocked");
        };
        assert!(reason.starts_with("board init:"));
    }

    fn blocked_on_units(rig: &Rig) {
        assert!(blocked_reason(rig).starts_with("worker units:"));
    }

    #[tokio::test]
    async fn a_start_reads_the_units_once_and_its_environment_serves_the_pause_the_init_and_the_processes(
    ) {
        let rig = rig_with_web_json(Init::Accepts, enrolled_store(), "http://127.0.0.1:4790");

        rig.control.proxy_ready().await;

        assert_eq!(seen(&rig), ["units", "pause", "init", "start"]);
        for used in [&rig.paused_with, &rig.init_with, &rig.started_with] {
            assert_eq!(*used.lock().unwrap(), ["/read-1"]);
        }
    }

    #[tokio::test]
    async fn an_unreadable_unit_list_at_connect_leaves_the_worker_stopped_and_quit_pauses_nothing()
    {
        let rig = rig(Init::Accepts, enrolled_store());
        rig.units_readable.store(false, Ordering::SeqCst);

        rig.control.proxy_ready().await;

        assert_eq!(seen(&rig), ["units"]);
        blocked_on_units(&rig);
        rig.control.stop().await;
        assert_eq!(seen(&rig), ["units", "stop", "kill"]);
    }

    #[tokio::test]
    async fn an_unreadable_unit_list_fails_the_enrolment_before_anything_is_checked_stopped_or_promoted(
    ) {
        let rig = rig(Init::Accepts, enrolled_store());
        started(&rig).await;
        rig.units_readable.store(false, Ordering::SeqCst);

        let error = rig.control.enrol("abm_new", "abk1_new").await.unwrap_err();

        assert!(error.to_string().starts_with("worker units:"));
        assert_eq!(seen(&rig), ["units"]);
        assert_eq!(
            rig.store.get(TOKEN_ACCOUNT).unwrap().as_deref(),
            Some("abm_old")
        );
        assert!(!pending_present(rig.store.as_ref()).unwrap());
    }

    #[tokio::test]
    async fn a_restart_that_cannot_read_the_units_leaves_the_worker_stopped_and_quit_pauses_with_the_previous_environment(
    ) {
        let rig = rig(Init::Accepts, enrolled_store());
        started(&rig).await;
        rig.units_readable.store(false, Ordering::SeqCst);

        rig.control.restart().await.unwrap();

        assert_eq!(seen(&rig), ["stop", "units"]);
        blocked_on_units(&rig);
        rig.events.lock().unwrap().clear();
        rig.paused_with.lock().unwrap().clear();
        rig.control.stop().await;
        assert_eq!(seen(&rig), ["stop", "pause", "kill"]);
        assert_eq!(*rig.paused_with.lock().unwrap(), ["/read-1"]);
    }

    #[tokio::test]
    async fn a_restart_after_a_failed_read_connects_again_with_a_fresh_read() {
        let rig = rig(Init::Accepts, enrolled_store());
        started(&rig).await;
        rig.units_readable.store(false, Ordering::SeqCst);
        rig.control.restart().await.unwrap();
        rig.units_readable.store(true, Ordering::SeqCst);
        rig.events.lock().unwrap().clear();

        rig.control.restart().await.unwrap();

        assert_eq!(seen(&rig), ["units", "pause", "start"]);
        assert_eq!(rig.started_with.lock().unwrap().last().unwrap(), "/read-2");
    }

    fn host_over(root: &Path) -> ProcessHost {
        let setup = Arc::new(WorkerSetup {
            paths: AppPaths::new(root),
            board: PathBuf::new(),
            base_env: BTreeMap::new(),
            worker_path: Vec::new(),
            app_log: LogFile::new(PathBuf::new()),
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
    const FAILING_UNITS: &str =
        "#!/bin/sh\nif [ \"$1\" = internal ]; then echo broken >&2; exit 1; fi\necho '[]'\n";

    fn control_over_a_script(root: &Path) -> WorkerControl {
        use std::os::unix::fs::PermissionsExt;

        let paths = AppPaths::new(root);
        let script = root.join("board");
        std::fs::write(&script, FAILING_UNITS).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        WorkerControl::new(
            WorkerSetup {
                app_log: LogFile::new(root.join("app.log")),
                paths,
                board: script,
                base_env: BTreeMap::new(),
                worker_path: Vec::new(),
            },
            Arc::new(MemoryStore::default()),
            Arc::new(StatusBoard::default()),
            Arc::new(Activity::default()),
        )
    }

    #[tokio::test]
    async fn the_check_runs_with_the_environment_of_the_last_start() {
        let root = tempfile::tempdir().unwrap();
        let control = control_over_a_script(root.path());
        control.state.lock().await.env = Some(Arc::new(BTreeMap::from([(
            "PATH".to_owned(),
            "/started/bin".to_owned(),
        )])));

        control.check().await.unwrap();

        let log = std::fs::read_to_string(control.check_log()).unwrap();
        assert!(log.contains("(/started/bin)"), "{log}");
    }

    #[tokio::test]
    async fn the_check_of_a_worker_that_never_started_reads_the_units_and_counts_a_failure_when_it_cannot(
    ) {
        let root = tempfile::tempdir().unwrap();
        let control = control_over_a_script(root.path());

        let problems = control.check().await.unwrap();

        assert_eq!(problems, 1);
        let log = std::fs::read_to_string(control.check_log()).unwrap();
        assert!(log.contains("fail worker units:"), "{log}");
        assert!(log.contains("broken"), "{log}");
    }
}
