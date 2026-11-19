use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use crate::backend::{self, HEALTH_LIMIT, HEALTH_POLL};
use crate::lock::Lock;
use crate::paths::{AppPaths, Resources, Sidecars};
use crate::secrets::{proxy_secret, SecretError, SecretStore};
use crate::settings::{Settings, SettingsError};
use crate::supervisor::orphans;
use crate::supervisor::process::{last_log_line, rotate_if_larger, LOG_LIMIT};
use crate::supervisor::{decide, Decision, ProcessSpec, RunningProcess, Status, StatusBoard};

pub const BACKEND: &str = "backend";
const ORPHAN_GRACE: Duration = Duration::from_secs(5);
const STOP_GRACE: Duration = Duration::from_secs(10);
const LOG_CHECK: Duration = Duration::from_secs(60);

pub struct Config {
    pub paths: AppPaths,
    pub sidecars: Sidecars,
    pub resources: Resources,
    pub login_name: String,
    pub port_override: Option<u16>,
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

pub struct Handle {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
    _lock: Lock,
}

impl Handle {
    /// Returns once everything the runtime started has stopped; the lock is released afterwards.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
    }
}

pub async fn start(
    config: Config,
    lock: Lock,
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

    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(supervise_backend(
        Backend {
            config,
            settings,
            secret,
            board,
        },
        stopped,
    ));
    Ok(Handle {
        stop,
        task,
        _lock: lock,
    })
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

async fn stop_requested(stop: &mut watch::Receiver<bool>) {
    while !*stop.borrow() {
        if stop.changed().await.is_err() {
            return;
        }
    }
}

enum Outcome {
    Stop,
    Exited(String),
    NotHealthy,
    Healthy,
}

fn describe_exit(status: std::io::Result<std::process::ExitStatus>) -> String {
    use std::os::unix::process::ExitStatusExt;
    match status {
        Ok(status) => match (status.code(), status.signal()) {
            (Some(code), _) => format!("exited with code {code}"),
            (None, Some(signal)) => format!("killed by signal {signal}"),
            (None, None) => "exited".into(),
        },
        Err(error) => format!("could not be waited on: {error}"),
    }
}

fn with_log_tail(reason: String, log: &std::path::Path) -> String {
    if !reason.starts_with("exited with code") {
        return reason;
    }
    match last_log_line(log) {
        Some(line) => format!("{reason}: {line}"),
        None => reason,
    }
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
