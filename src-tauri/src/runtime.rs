use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use crate::backend::{self, HEALTH_LIMIT, HEALTH_POLL};
use crate::paths::{AppPaths, Resources, Sidecars};
use crate::proxy::{Proxy, ProxyConfig};
use crate::secrets::{proxy_secret, SecretError, SecretStore};
use crate::settings::{Settings, SettingsError};
use crate::supervisor::orphans;
use crate::supervisor::process::{
    describe_exit, rotate_if_larger, stop_requested, with_log_tail, LOG_LIMIT,
};
use crate::supervisor::{decide, Decision, ProcessSpec, RunningProcess, Status, StatusBoard};
use crate::worker::control::Enrolled;
use crate::worker::enrol::EnrolError;
use crate::worker::processes::{WorkerSetup, WORKER_LISTEN};
use crate::worker::{env as worker_env, WorkerControl};

pub const BACKEND: &str = "backend";
pub const PROXY: &str = "proxy";
const ORPHAN_GRACE: Duration = Duration::from_secs(5);
const STOP_GRACE: Duration = Duration::from_secs(10);
const LOG_CHECK: Duration = Duration::from_secs(60);

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
    stop_backend: watch::Sender<bool>,
    worker_task: JoinHandle<()>,
    proxy_task: JoinHandle<()>,
    backend_task: JoinHandle<()>,
    worker: Option<Arc<WorkerControl>>,
}

impl Handle {
    pub fn worker(&self) -> Option<Arc<WorkerControl>> {
        self.worker.clone()
    }

    pub async fn enrol(&self, token: &str, data_key: &str) -> Result<Enrolled, EnrolError> {
        match &self.worker {
            Some(worker) => worker.enrol(token, data_key).await,
            None => Err(EnrolError::new("this run has no worker")),
        }
    }

    /// Stops in reverse order of the start, so that the runs can report as paused while the board is still up:
    /// worker, proxy, backend. Returns once all of it has stopped.
    pub async fn stop(self) {
        let _ = self.stop_worker.send(true);
        let _ = self.worker_task.await;
        let _ = self.stop_proxy.send(true);
        let _ = self.proxy_task.await;
        let _ = self.stop_backend.send(true);
        let _ = self.backend_task.await;
    }
}

pub async fn start(
    config: Config,
    secrets: Arc<dyn SecretStore>,
    board: Arc<StatusBoard>,
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

    let worker = match (&config.worker, config.upstream_override) {
        (Some(worker), None) => prepare_worker(&config, worker, &board),
        _ => None,
    };

    let (stop_worker, worker_stopped) = watch::channel(false);
    let (stop_proxy, proxy_stopped) = watch::channel(false);
    let (stop_backend, backend_stopped) = watch::channel(false);
    let proxy_task = tokio::spawn(serve_proxy(
        ProxySetup {
            port: settings.port,
            user_name: settings.user_name.clone(),
            secret: secret.clone(),
            frontend: config.resources.frontend(),
            upstream_override: config.upstream_override,
        },
        board.clone(),
        proxy_stopped,
    ));
    let worker_task = match &worker {
        Some(worker) => tokio::spawn(run_worker(worker.clone(), board.clone(), worker_stopped)),
        None => tokio::spawn(async {}),
    };
    let backend_task = if config.upstream_override.is_some() {
        tokio::spawn(async {})
    } else {
        tokio::spawn(supervise_backend(
            Backend {
                config,
                settings,
                secret,
                board,
            },
            backend_stopped,
        ))
    };
    Ok(Handle {
        stop_worker,
        stop_proxy,
        stop_backend,
        worker_task,
        proxy_task,
        backend_task,
        worker,
    })
}

fn prepare_worker(
    config: &Config,
    worker: &WorkerConfig,
    board: &Arc<StatusBoard>,
) -> Option<Arc<WorkerControl>> {
    let setup = WorkerSetup {
        paths: config.paths.clone(),
        board: config.paths.worker_board(),
        env: worker_env::environment(&config.paths, &config.paths.worker_credentials(), &|name| {
            std::env::var(name).ok()
        }),
        lockf: PathBuf::from("/usr/bin/lockf"),
    };
    let control = Arc::new(WorkerControl::new(
        setup,
        worker.secrets.clone(),
        board.clone(),
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

struct ProxySetup {
    port: u16,
    user_name: String,
    secret: String,
    frontend: std::path::PathBuf,
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

struct Backend {
    config: Config,
    settings: Settings,
    secret: String,
    board: Arc<StatusBoard>,
}

impl Backend {
    fn spec(&self, port: u16) -> ProcessSpec {
        ProcessSpec {
            name: BACKEND,
            program: self.config.sidecars.board_server.clone(),
            args: Vec::new(),
            env: backend::environment(&self.settings, &self.secret, port, &self.config.resources),
        }
    }

    fn crashed(&self, reason: String, restart_in: Option<Duration>) {
        self.board
            .set(BACKEND, Status::Crashed { reason, restart_in });
    }
}

enum Outcome {
    Stop,
    Exited(String),
    NotHealthy,
    Healthy,
}

async fn supervise_backend(backend: Backend, mut stop: watch::Receiver<bool>) {
    let log = backend.config.paths.log_file(BACKEND);
    let mut restarts = 0;
    let mut port = None;

    loop {
        backend.board.set(BACKEND, Status::Starting);
        let started = backend::free_port(port)
            .map_err(|error| error.to_string())
            .and_then(|free| {
                port = Some(free);
                RunningProcess::spawn(&backend.spec(free), &backend.config.paths)
                    .map(|process| (free, process))
                    .map_err(|error| format!("cannot start: {error}"))
            });

        let reason = match started {
            Err(reason) => reason,
            Ok((free, mut process)) => {
                let outcome = tokio::select! {
                    healthy = backend::wait_healthy(free, HEALTH_POLL, HEALTH_LIMIT) => {
                        if healthy { Outcome::Healthy } else { Outcome::NotHealthy }
                    }
                    status = process.wait() => Outcome::Exited(describe_exit(status)),
                    () = stop_requested(&mut stop) => Outcome::Stop,
                };
                let ended = match outcome {
                    Outcome::Stop => {
                        let _ = process.stop(STOP_GRACE).await;
                        backend.board.set(BACKEND, Status::Stopped);
                        return;
                    }
                    Outcome::NotHealthy => {
                        let _ = process.stop(STOP_GRACE).await;
                        Some(format!("not healthy after {} s", HEALTH_LIMIT.as_secs()))
                    }
                    Outcome::Exited(reason) => Some(reason),
                    Outcome::Healthy => None,
                };
                match ended {
                    Some(reason) => reason,
                    None => {
                        backend.board.set(BACKEND, Status::Running { port: free });
                        let exit = loop {
                            tokio::select! {
                                status = process.wait() => break Some(describe_exit(status)),
                                () = stop_requested(&mut stop) => break None,
                                () = sleep(LOG_CHECK) => { let _ = rotate_if_larger(&log, LOG_LIMIT); }
                            }
                        };
                        let Some(reason) = exit else {
                            let _ = process.stop(STOP_GRACE).await;
                            backend.board.set(BACKEND, Status::Stopped);
                            return;
                        };
                        reason
                    }
                }
            }
        };

        let reason = with_log_tail(reason, &log);
        match decide(restarts) {
            Decision::Restart { after } => {
                backend.crashed(reason, Some(after));
                restarts += 1;
                tokio::select! {
                    () = sleep(after) => {}
                    () = stop_requested(&mut stop) => {
                        backend.board.set(BACKEND, Status::Stopped);
                        return;
                    }
                }
            }
            Decision::GiveUp => {
                backend.crashed(reason, None);
                stop_requested(&mut stop).await;
                return;
            }
        }
    }
}
