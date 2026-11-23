use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::activity::{Activity, Guard, Kind};
use crate::applog::LogFile;
use crate::backend::{self, Healthz};
use crate::backup;
use crate::core_build::CoreBuild;
use crate::localtime;
use crate::paths::{AppPaths, Resources, Sidecars};
use crate::proxy::{Proxy, ProxyConfig};
use crate::secrets::{proxy_secret, SecretError, SecretStore};
use crate::settings::{Settings, SettingsError};
use crate::supervisor::orphans;
use crate::supervisor::process::stop_requested;
use crate::supervisor::table::{changed_labels, BACKEND, BACKUP, PROXY, WORKER, WORKER_LISTEN};
use crate::supervisor::{
    supervise, Command, Control, Launcher, ProcessSpec, Readiness, RunningProcess, Spawned, Status,
    StatusBoard,
};
use crate::worker::check::summary;
use crate::worker::control::{ActionError, Enrolled};
use crate::worker::enrol::EnrolError;
use crate::worker::processes::WorkerSetup;
use crate::worker::{env as worker_env, WorkerControl};

const ORPHAN_GRACE: Duration = Duration::from_secs(5);

pub struct Config {
    pub paths: AppPaths,
    pub sidecars: Sidecars,
    pub resources: Resources,
    pub login_name: String,
    pub port_override: Option<u16>,
    /// Serve the proxy in front of this local port and start no backend; for tests that compare the proxy to another one.
    pub upstream_override: Option<u16>,
    /// The worker's credentials and the executable that prints them for the CLI. Ignored with `upstream_override`.
    pub worker: Option<WorkerConfig>,
}

pub struct WorkerConfig {
    pub secrets: Arc<dyn SecretStore>,
    /// The executable that serves the credentials, reached by the CLI through a link in the worker directory.
    pub app_executable: PathBuf,
}

#[derive(Debug)]
pub enum StartError {
    Settings(SettingsError),
    Secret(SecretError),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Settings(error) => error.fmt(f),
            Self::Secret(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for StartError {}

pub type Running = Arc<tokio::sync::Mutex<Option<Handle>>>;

pub struct Handle {
    stop_worker: watch::Sender<bool>,
    stop_proxy: watch::Sender<bool>,
    backend: Control,
    stop_status: watch::Sender<bool>,
    status_task: JoinHandle<()>,
    worker_task: JoinHandle<()>,
    proxy_task: JoinHandle<()>,
    backend_task: JoinHandle<()>,
    worker: Option<Arc<WorkerControl>>,
    settings: Settings,
    backups_dir: PathBuf,
    app_log: LogFile,
    proxy_setup: ProxySetup,
    board: Arc<StatusBoard>,
    activity: Arc<Activity>,
}

impl Handle {
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn worker(&self) -> Option<Arc<WorkerControl>> {
        self.worker.clone()
    }

    pub async fn enrol(&self, token: &str, data_key: &str) -> Result<Enrolled, EnrolError> {
        match &self.worker {
            Some(worker) => worker.enrol(token, data_key).await,
            None => Err(EnrolError::new("this run has no worker")),
        }
    }

    pub async fn back_up_now(&self) -> Result<PathBuf, ActionError> {
        let _gate = begin(&self.activity, Kind::Backup)?;
        let no_backend = || ActionError::Failed("the backend is not running".into());
        if !matches!(self.board.get(BACKEND), Some(Status::Running { .. })) {
            return Err(no_backend());
        }
        let (held, acknowledged) = oneshot::channel();
        let hold = Command::Hold {
            note: "backing up".into(),
            held,
        };
        self.backend
            .commands
            .send(hold)
            .await
            .map_err(|_| no_backend())?;
        acknowledged.await.map_err(|_| no_backend())?;

        let data_dir = self.settings.data_dir.clone();
        let backups_dir = self.backups_dir.clone();
        let log = self.app_log.clone();
        let copied = tokio::task::spawn_blocking(move || {
            backup::manual(&data_dir, &backups_dir, &localtime::compact(), &log)
        })
        .await
        .unwrap_or_else(|error| Err(error.to_string()));
        let _ = self.backend.commands.send(Command::Resume).await;

        match copied {
            Ok(path) => {
                self.board.set(BACKUP, Status::Note(localtime::clock()));
                Ok(path)
            }
            Err(reason) => {
                self.board
                    .set(BACKUP, Status::Note(format!("failed ({reason})")));
                Err(ActionError::Failed(reason))
            }
        }
    }

    pub async fn restart_server(&mut self) -> Result<(), ActionError> {
        let _gate = begin(&self.activity, Kind::Recovery)?;
        self.backend
            .commands
            .send(Command::Restart)
            .await
            .map_err(|_| ActionError::Failed("this run has no backend to restart".into()))?;
        let proxy = self.board.get(PROXY);
        if proxy_needs_restart(proxy.as_ref(), self.proxy_task.is_finished()) {
            self.proxy_task = tokio::spawn(serve_proxy(
                self.proxy_setup.clone(),
                self.board.clone(),
                self.stop_proxy.subscribe(),
            ));
        }
        Ok(())
    }

    /// Stops in reverse order of the start, so that the runs can report as paused while the board is still up:
    /// worker, proxy, backend. Returns once all of it has stopped.
    pub async fn stop(self) {
        let _ = self.stop_worker.send(true);
        let _ = self.worker_task.await;
        let _ = self.stop_proxy.send(true);
        let _ = self.proxy_task.await;
        self.backend.stop();
        let _ = self.backend_task.await;
        let _ = self.stop_status.send(true);
        let _ = self.status_task.await;
    }
}

fn begin(activity: &Arc<Activity>, kind: Kind) -> Result<Guard, ActionError> {
    activity.begin(kind).map_err(ActionError::Busy)
}

async fn worker_of(running: &Running) -> Result<Arc<WorkerControl>, ActionError> {
    let guard = running.lock().await;
    match guard.as_ref() {
        Some(handle) => handle
            .worker()
            .ok_or_else(|| ActionError::Failed("this run has no worker".into())),
        None => Err(ActionError::Failed("the app is not running".into())),
    }
}

pub async fn restart_server(running: &Running) -> Result<(), ActionError> {
    match running.lock().await.as_mut() {
        Some(handle) => handle.restart_server().await,
        None => Err(ActionError::Failed("the app is not running".into())),
    }
}

pub async fn back_up_now(running: &Running) -> Result<PathBuf, ActionError> {
    match running.lock().await.as_ref() {
        Some(handle) => handle.back_up_now().await,
        None => Err(ActionError::Failed("the app is not running".into())),
    }
}

pub async fn restart_worker(running: &Running) -> Result<(), ActionError> {
    worker_of(running).await?.restart().await
}

pub async fn disconnect_worker(running: &Running) -> Result<(), ActionError> {
    worker_of(running).await?.disconnect().await
}

pub async fn check_worker(running: &Running, board: &StatusBoard) -> Result<PathBuf, ActionError> {
    let worker = worker_of(running).await?;
    let problems = worker.check().await?;
    board.set(WORKER, Status::Note(summary(problems)));
    Ok(worker.check_log())
}

fn proxy_needs_restart(row: Option<&Status>, task_finished: bool) -> bool {
    task_finished && !matches!(row, Some(Status::Running { .. }))
}

pub async fn start(
    config: Config,
    secrets: Arc<dyn SecretStore>,
    board: Arc<StatusBoard>,
    activity: Arc<Activity>,
) -> Result<Handle, StartError> {
    let blocked = |error: StartError| {
        board.set(
            BACKEND,
            Status::Blocked {
                reason: error.to_string(),
            },
        );
        error
    };

    let mut settings = Settings::load_or_create(&config.paths, &config.login_name)
        .map_err(StartError::Settings)
        .map_err(blocked)?;
    if let Some(port) = config.port_override {
        settings.port = port;
    }
    orphans::sweep(&config.paths.run_dir(), ORPHAN_GRACE).await;
    let secret = proxy_secret(secrets.as_ref())
        .map_err(StartError::Secret)
        .map_err(blocked)?;

    let (stop_status, status_stopped) = watch::channel(false);
    let status_task = tokio::spawn(log_status_changes(
        LogFile::new(config.paths.log_file("app")),
        board.clone(),
        status_stopped,
    ));

    let worker = match (&config.worker, config.upstream_override) {
        (Some(worker), None) => {
            prepare_worker(&config, worker, &settings.worker_path, &board, &activity)
        }
        _ => None,
    };

    let (stop_worker, worker_stopped) = watch::channel(false);
    let (stop_proxy, proxy_stopped) = watch::channel(false);
    let (backend, backend_stopped, backend_commands) = Control::new();
    let proxy_setup = ProxySetup {
        port: settings.port,
        user_name: settings.user_name.clone(),
        secret: secret.clone(),
        frontend: config.resources.frontend(),
        log: LogFile::new(config.paths.log_file(PROXY)),
        upstream_override: config.upstream_override,
    };
    let proxy_task = tokio::spawn(serve_proxy(
        proxy_setup.clone(),
        board.clone(),
        proxy_stopped,
    ));
    let worker_task = match &worker {
        Some(worker) => tokio::spawn(run_worker(worker.clone(), board.clone(), worker_stopped)),
        None => tokio::spawn(async {}),
    };
    let backups_dir = settings.backups_dir(&config.paths);
    let app_log_path = config.paths.log_file("app");
    let backend_task = if config.upstream_override.is_some() {
        tokio::spawn(async {})
    } else {
        let log = config.paths.log_file(BACKEND);
        let upgrade = PreUpgrade {
            data_dir: settings.data_dir.clone(),
            backups_dir: backups_dir.clone(),
            bundled: CoreBuild::read(&config.resources.core_build()),
            log: LogFile::new(config.paths.log_file("app")),
        };
        tokio::spawn(run_backend(
            BackendLauncher {
                config,
                settings: settings.clone(),
                secret,
                port: None,
            },
            Some(Healthz),
            board.clone(),
            log,
            upgrade,
            backend_stopped,
            backend_commands,
        ))
    };
    Ok(Handle {
        stop_status,
        status_task,
        stop_worker,
        stop_proxy,
        backend,
        worker_task,
        proxy_task,
        backend_task,
        worker,
        settings,
        backups_dir,
        app_log: LogFile::new(app_log_path),
        proxy_setup,
        board,
        activity,
    })
}

fn prepare_worker(
    config: &Config,
    worker: &WorkerConfig,
    worker_path: &[PathBuf],
    board: &Arc<StatusBoard>,
    activity: &Arc<Activity>,
) -> Option<Arc<WorkerControl>> {
    let setup = WorkerSetup {
        paths: config.paths.clone(),
        board: config.paths.worker_board(),
        env: worker_env::environment(
            &config.paths,
            &config.paths.worker_credentials(),
            worker_path,
            &|name| std::env::var(name).ok(),
        ),
        lockf: PathBuf::from("/usr/bin/lockf"),
    };
    let control = Arc::new(WorkerControl::new(
        setup,
        worker.secrets.clone(),
        board.clone(),
        activity.clone(),
    ));
    match control.prepare(&config.sidecars.board, &worker.app_executable) {
        Ok(()) => Some(control),
        Err(reason) => {
            board.set(WORKER_LISTEN, Status::Blocked { reason });
            None
        }
    }
}

async fn run_worker(
    worker: Arc<WorkerControl>,
    board: Arc<StatusBoard>,
    mut stop: watch::Receiver<bool>,
) {
    let mut changes = board.subscribe();
    let proxy_runs = loop {
        if matches!(board.get(PROXY), Some(Status::Running { .. })) {
            break true;
        }
        tokio::select! {
            changed = changes.changed() => if changed.is_err() { break false },
            () = stop_requested(&mut stop) => break false,
        }
    };
    if proxy_runs {
        worker.proxy_ready().await;
        stop_requested(&mut stop).await;
    }
    worker.stop().await;
}

async fn log_status_changes(
    log: LogFile,
    board: Arc<StatusBoard>,
    mut stop: watch::Receiver<bool>,
) {
    let mut changes = board.subscribe();
    let mut seen = BTreeMap::new();
    loop {
        for line in changed_labels(&board, &mut seen) {
            log.write(&line);
        }
        tokio::select! {
            changed = changes.changed() => if changed.is_err() { break },
            () = stop_requested(&mut stop) => break,
        }
    }
    for line in changed_labels(&board, &mut seen) {
        log.write(&line);
    }
}

#[derive(Clone)]
struct ProxySetup {
    port: u16,
    user_name: String,
    secret: String,
    frontend: std::path::PathBuf,
    log: LogFile,
    upstream_override: Option<u16>,
}

fn running_port(board: &StatusBoard) -> Option<u16> {
    match board.get(BACKEND) {
        Some(Status::Running { port }) => Some(port),
        _ => None,
    }
}

/// The proxy opens once the backend has been healthy for the first time and stays up through its restarts,
/// answering 502 while there is nothing to forward to. A port that cannot be bound leaves the app up without it.
async fn serve_proxy(setup: ProxySetup, board: Arc<StatusBoard>, mut stop: watch::Receiver<bool>) {
    let mut changes = board.subscribe();
    loop {
        if setup.upstream_override.is_some() || running_port(&board).is_some() {
            break;
        }
        tokio::select! {
            changed = changes.changed() => if changed.is_err() { return },
            () = stop_requested(&mut stop) => return,
        }
    }

    board.set(PROXY, Status::Starting);
    let lookup = board.clone();
    let started = Proxy::start(ProxyConfig {
        port: setup.port,
        user_name: setup.user_name,
        secret: setup.secret,
        frontend: setup.frontend,
        log: setup.log,
        upstream_port: Arc::new(move || setup.upstream_override.or_else(|| running_port(&lookup))),
    })
    .await;
    match started {
        Ok(proxy) => {
            board.set(PROXY, Status::Running { port: proxy.port() });
            stop_requested(&mut stop).await;
            proxy.stop().await;
            board.set(PROXY, Status::Stopped);
        }
        Err(error) => board.set(
            PROXY,
            Status::Blocked {
                reason: error.to_string(),
            },
        ),
    }
}

#[derive(Clone)]
struct PreUpgrade {
    data_dir: PathBuf,
    backups_dir: PathBuf,
    bundled: Option<CoreBuild>,
    log: LogFile,
}

impl PreUpgrade {
    async fn run(&self) -> Result<(), String> {
        let this = self.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            backup::pre_upgrade(
                &this.data_dir,
                &this.backups_dir,
                this.bundled.as_ref(),
                &localtime::compact(),
            )
        })
        .await
        .unwrap_or_else(|error| Err(error.to_string()))?;
        if let Some(written) = outcome {
            self.log.write(&format!(
                "backup before the upgrade written to {}",
                written.display()
            ));
        }
        Ok(())
    }
}

async fn run_backend<L: Launcher, R: Readiness>(
    launcher: L,
    readiness: Option<R>,
    board: Arc<StatusBoard>,
    log: PathBuf,
    upgrade: PreUpgrade,
    mut stop: watch::Receiver<bool>,
    mut commands: mpsc::Receiver<Command>,
) {
    while let Err(reason) = upgrade.run().await {
        board.set(BACKEND, Status::Blocked { reason });
        if !restart_requested(&mut stop, &mut commands).await {
            board.set(BACKEND, Status::Stopped);
            return;
        }
    }
    if *stop.borrow() {
        board.set(BACKEND, Status::Stopped);
        return;
    }
    supervise(BACKEND, launcher, readiness, board, log, stop, commands).await;
}

async fn restart_requested(
    stop: &mut watch::Receiver<bool>,
    commands: &mut mpsc::Receiver<Command>,
) -> bool {
    loop {
        tokio::select! {
            () = stop_requested(stop) => return false,
            command = commands.recv() => match command {
                Some(Command::Restart) => return true,
                Some(_) => {}
                None => return false,
            },
        }
    }
}

struct BackendLauncher {
    config: Config,
    settings: Settings,
    secret: String,
    port: Option<u16>,
}

impl Launcher for BackendLauncher {
    type Process = Spawned;

    fn launch(&mut self) -> Result<Spawned, String> {
        let port = backend::free_port(self.port).map_err(|error| error.to_string())?;
        self.port = Some(port);
        let spec = ProcessSpec {
            name: BACKEND,
            program: self.config.sidecars.board_server.clone(),
            args: Vec::new(),
            env: backend::environment(&self.settings, &self.secret, port, &self.config.resources),
        };
        RunningProcess::spawn(&spec, &self.config.paths)
            .map(|process| Spawned::new(process, Status::Running { port }))
            .map_err(|error| format!("cannot start: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use std::future::pending;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::supervisor::{NoReadiness, Supervised};

    use super::*;

    struct Idle;

    impl Supervised for Idle {
        async fn wait(&mut self) -> String {
            pending().await
        }

        async fn stop(&mut self, _grace: Duration) {}

        fn running_status(&self) -> Status {
            Status::Running { port: 4000 }
        }
    }

    struct Counting(Arc<AtomicUsize>);

    impl Launcher for Counting {
        type Process = Idle;

        fn launch(&mut self) -> Result<Idle, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Idle)
        }
    }

    async fn until(board: &StatusBoard, wanted: impl Fn(Option<Status>) -> bool) {
        let mut changes = board.subscribe();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !wanted(board.get(BACKEND)) {
                changes.changed().await.unwrap();
            }
        })
        .await
        .expect("the backend row did not reach the wanted state");
    }

    #[tokio::test]
    async fn a_failed_pre_upgrade_backup_blocks_the_backend_and_a_restart_retries_it() {
        let data = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(data.path().join("pglite")).unwrap();
        std::fs::write(data.path().join("pglite/PG_VERSION"), "16").unwrap();
        backup::record(data.path(), "aaaaaaa").unwrap();
        let backups = tempfile::tempdir().unwrap();
        let logs = tempfile::tempdir().unwrap();
        std::fs::set_permissions(backups.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let launches = Arc::new(AtomicUsize::new(0));
        let board = Arc::new(StatusBoard::default());
        let (control, stopped, commands) = Control::new();
        let upgrade = PreUpgrade {
            data_dir: data.path().to_path_buf(),
            backups_dir: backups.path().to_path_buf(),
            bundled: Some(CoreBuild {
                commit: "bbbbbbb1234".into(),
                built_at: "2026-11-20T08:15:00.000Z".into(),
            }),
            log: LogFile::new(logs.path().join("app.log")),
        };
        let task = tokio::spawn(run_backend(
            Counting(launches.clone()),
            None::<NoReadiness>,
            board.clone(),
            logs.path().join("backend.log"),
            upgrade,
            stopped,
            commands,
        ));

        until(&board, |row| matches!(row, Some(Status::Blocked { .. }))).await;
        assert_eq!(launches.load(Ordering::SeqCst), 0);
        assert_eq!(
            backup::read_recorded(data.path()).as_deref(),
            Some("aaaaaaa")
        );

        std::fs::set_permissions(backups.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        control.commands.send(Command::Restart).await.unwrap();
        until(&board, |row| matches!(row, Some(Status::Running { .. }))).await;

        assert_eq!(launches.load(Ordering::SeqCst), 1);
        assert_eq!(
            backup::read_recorded(data.path()).as_deref(),
            Some("bbbbbbb1234")
        );
        assert!(backups.path().read_dir().unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("pre-upgrade-bbbbbbb-")));
        control.stop();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn a_stop_while_the_backend_is_blocked_ends_the_task_without_starting_it() {
        let data = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(data.path().join("pglite")).unwrap();
        let logs = tempfile::tempdir().unwrap();
        let launches = Arc::new(AtomicUsize::new(0));
        let board = Arc::new(StatusBoard::default());
        let (control, stopped, commands) = Control::new();
        let upgrade = PreUpgrade {
            data_dir: data.path().to_path_buf(),
            backups_dir: logs.path().join("backups"),
            bundled: None,
            log: LogFile::new(logs.path().join("app.log")),
        };
        let task = tokio::spawn(run_backend(
            Counting(launches.clone()),
            None::<NoReadiness>,
            board.clone(),
            logs.path().join("backend.log"),
            upgrade,
            stopped,
            commands,
        ));

        until(&board, |row| matches!(row, Some(Status::Blocked { .. }))).await;
        control.stop();
        task.await.unwrap();

        assert_eq!(launches.load(Ordering::SeqCst), 0);
        assert_eq!(board.get(BACKEND), Some(Status::Stopped));
    }

    #[test]
    fn the_proxy_is_started_again_only_when_its_task_ended_without_it_running() {
        let blocked = Status::Blocked {
            reason: "port in use".into(),
        };

        assert!(proxy_needs_restart(Some(&blocked), true));
        assert!(proxy_needs_restart(Some(&Status::Stopped), true));
        assert!(proxy_needs_restart(None, true));
        assert!(!proxy_needs_restart(
            Some(&Status::Running { port: 4785 }),
            true
        ));
        assert!(!proxy_needs_restart(Some(&blocked), false));
        assert!(!proxy_needs_restart(None, false));
    }
}
