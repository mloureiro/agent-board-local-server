use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch, Mutex};
use tokio::task::JoinHandle;
use tokio::time::sleep;

use super::report::worker_note;
use crate::paths::AppPaths;
use crate::supervisor::process::stop_requested;
use crate::supervisor::table::{DISPATCH, WORKER, WORKER_LISTEN, WORKER_WATCH};
use crate::supervisor::{
    supervise, Launcher, NoReadiness, ProcessSpec, RunningProcess, Spawned, Status, StatusBoard,
};

const TICK_EVERY: Duration = Duration::from_secs(120);
const TICK_LIMIT: Duration = Duration::from_secs(110);
const TICK_GRACE: Duration = Duration::from_secs(10);
const STOP_GRACE: Duration = Duration::from_secs(10);
const LOCK_FILE: &str = "agent-board-dispatch.lock";
/// `lockf -s -t 0` exits with EX_TEMPFAIL when another tick holds the lock.
const LOCK_HELD: i32 = 75;

pub struct WorkerSetup {
    pub paths: AppPaths,
    pub board: PathBuf,
    pub env: BTreeMap<String, String>,
    pub lockf: PathBuf,
}

impl WorkerSetup {
    pub fn spec(&self, name: &'static str, args: &[&str]) -> ProcessSpec {
        ProcessSpec {
            name,
            program: self.board.clone(),
            args: args.iter().map(OsString::from).collect(),
            env: self.env.clone(),
        }
    }

    fn tick_spec(&self) -> ProcessSpec {
        let lock = self.paths.worker_run_dir().join(LOCK_FILE);
        ProcessSpec {
            name: DISPATCH,
            program: self.lockf.clone(),
            args: vec![
                "-s".into(),
                "-t".into(),
                "0".into(),
                lock.into_os_string(),
                self.board.clone().into_os_string(),
                "internal".into(),
                "dispatch".into(),
            ],
            env: self.env.clone(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum TickResult {
    Ok,
    LockHeld,
    Failed(i32),
    Killed(i32),
    TimedOut,
}

impl TickResult {
    /// An exit with the lock-held code is another tick at work, never a failure.
    pub fn classify(code: Option<i32>, signal: Option<i32>, timed_out: bool) -> Self {
        match (timed_out, code, signal) {
            (true, _, _) => Self::TimedOut,
            (false, Some(0), _) => Self::Ok,
            (false, Some(LOCK_HELD), _) => Self::LockHeld,
            (false, Some(code), _) => Self::Failed(code),
            (false, None, Some(signal)) => Self::Killed(signal),
            (false, None, None) => Self::Failed(-1),
        }
    }

    fn label(&self, at: &str) -> String {
        match self {
            Self::Ok => format!("ok at {at}"),
            Self::LockHeld => format!("lock held by another tick at {at}"),
            Self::Failed(code) => format!("failed with exit code {code} at {at}"),
            Self::Killed(signal) => format!("killed by signal {signal} at {at}"),
            Self::TimedOut => format!("stopped after {} s at {at}", TICK_LIMIT.as_secs()),
        }
    }
}

fn clock() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs()) as libc::time_t;
    // SAFETY: localtime_r only writes into the struct we pass; both pointers are valid for the call.
    let local = unsafe {
        let mut parts: libc::tm = std::mem::zeroed();
        libc::localtime_r(&seconds, &mut parts);
        parts
    };
    format!("{:02}:{:02}", local.tm_hour, local.tm_min)
}

struct Running {
    stop: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

pub struct Processes {
    setup: Arc<WorkerSetup>,
    board: Arc<StatusBoard>,
    running: Mutex<Option<Running>>,
}

impl Processes {
    pub fn new(setup: Arc<WorkerSetup>, board: Arc<StatusBoard>) -> Self {
        Self {
            setup,
            board,
            running: Mutex::new(None),
        }
    }

    pub async fn start(&self) {
        let mut running = self.running.lock().await;
        if running.is_some() {
            return;
        }
        let (stop, stopped) = watch::channel(false);
        self.board.set(
            WORKER,
            Status::Note("waiting for the first dispatch tick".into()),
        );
        let tasks = vec![
            supervise_worker(
                &self.setup,
                &self.board,
                WORKER_LISTEN,
                &["internal", "listen"],
                stopped.clone(),
            ),
            supervise_worker(
                &self.setup,
                &self.board,
                WORKER_WATCH,
                &["internal", "watch"],
                stopped.clone(),
            ),
            tokio::spawn(dispatch_timer(
                self.setup.clone(),
                self.board.clone(),
                stopped,
            )),
        ];
        *running = Some(Running { stop, tasks });
    }

    pub async fn stop(&self) {
        let Some(running) = self.running.lock().await.take() else {
            return;
        };
        let _ = running.stop.send(true);
        for task in running.tasks {
            let _ = task.await;
        }
    }
}

struct WorkerLauncher {
    setup: Arc<WorkerSetup>,
    name: &'static str,
    args: &'static [&'static str],
}

impl Launcher for WorkerLauncher {
    type Process = Spawned;

    fn launch(&mut self) -> Result<Spawned, String> {
        RunningProcess::spawn(&self.setup.spec(self.name, self.args), &self.setup.paths)
            .map(|process| Spawned::new(process, Status::Note("running".into())))
            .map_err(|error| format!("cannot start: {error}"))
    }
}

fn supervise_worker(
    setup: &Arc<WorkerSetup>,
    board: &Arc<StatusBoard>,
    name: &'static str,
    args: &'static [&'static str],
    stop: watch::Receiver<bool>,
) -> JoinHandle<()> {
    let (_commands, received) = mpsc::channel(1);
    tokio::spawn(supervise(
        name,
        WorkerLauncher {
            setup: setup.clone(),
            name,
            args,
        },
        None::<NoReadiness>,
        board.clone(),
        setup.paths.log_file(name),
        stop,
        received,
    ))
}

enum Tick {
    Done(TickResult),
    Stopped,
}

async fn run_tick(setup: &WorkerSetup, stop: &mut watch::Receiver<bool>) -> Result<Tick, String> {
    use std::os::unix::process::ExitStatusExt;
    let mut process = RunningProcess::spawn(&setup.tick_spec(), &setup.paths)
        .map_err(|error| format!("cannot start: {error}"))?;
    Ok(tokio::select! {
        status = process.wait() => match status {
            Ok(status) => Tick::Done(TickResult::classify(status.code(), status.signal(), false)),
            Err(error) => return Err(format!("could not be waited on: {error}")),
        },
        () = sleep(TICK_LIMIT) => {
            let _ = process.stop(TICK_GRACE).await;
            Tick::Done(TickResult::TimedOut)
        }
        () = stop_requested(stop) => {
            let _ = process.stop(STOP_GRACE).await;
            Tick::Stopped
        }
    })
}

async fn dispatch_timer(
    setup: Arc<WorkerSetup>,
    board: Arc<StatusBoard>,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        match run_tick(&setup, &mut stop).await {
            Ok(Tick::Stopped) => return,
            Ok(Tick::Done(result)) => {
                board.set(DISPATCH, Status::Note(result.label(&clock())));
                if let Some(status) = worker_note(&result, &setup.paths.log_file(DISPATCH)) {
                    board.set(WORKER, status);
                }
            }
            Err(reason) => board.set(DISPATCH, Status::Note(format!("{reason} at {}", clock()))),
        }
        tokio::select! {
            () = sleep(TICK_EVERY) => {}
            () = stop_requested(&mut stop) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tick_exit_is_classified_and_only_the_lock_code_is_not_a_failure() {
        let table = [
            (Some(0), None, false, TickResult::Ok),
            (Some(75), None, false, TickResult::LockHeld),
            (Some(1), None, false, TickResult::Failed(1)),
            (Some(74), None, false, TickResult::Failed(74)),
            (None, Some(9), false, TickResult::Killed(9)),
            (None, Some(15), true, TickResult::TimedOut),
            (Some(75), None, true, TickResult::TimedOut),
        ];
        for (code, signal, timed_out, expected) in table {
            assert_eq!(
                TickResult::classify(code, signal, timed_out),
                expected,
                "code {code:?} signal {signal:?} timed_out {timed_out}"
            );
        }
    }

    #[test]
    fn the_tick_runs_the_dispatcher_under_the_lock_in_the_worker_run_directory() {
        let setup = WorkerSetup {
            paths: AppPaths::new("/app"),
            board: PathBuf::from("/app/worker/bin/board"),
            env: BTreeMap::new(),
            lockf: PathBuf::from("/usr/bin/lockf"),
        };
        let spec = setup.tick_spec();
        let line: Vec<String> = std::iter::once(spec.program.to_string_lossy().into_owned())
            .chain(
                spec.args
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned()),
            )
            .collect();
        assert_eq!(
            line.join(" "),
            "/usr/bin/lockf -s -t 0 /app/worker/run/agent-board-dispatch.lock /app/worker/bin/board internal dispatch"
        );
    }
}
