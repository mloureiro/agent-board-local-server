use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch, Mutex};
use tokio::task::JoinHandle;
use tokio::time::sleep;

use super::report::worker_note;
use super::units::{ServiceUnit, TickUnit, WorkerUnits};
use crate::applog::LogFile;
use crate::localtime::clock;
use crate::paths::AppPaths;
use crate::supervisor::process::stop_requested;
use crate::supervisor::table::{DISPATCH, WORKER};
use crate::supervisor::{
    supervise, Launcher, NoReadiness, ProcessSpec, RunningProcess, Spawned, Status, StatusBoard,
};

const STOP_GRACE: Duration = Duration::from_secs(10);

pub struct WorkerSetup {
    pub paths: AppPaths,
    pub board: PathBuf,
    pub base_env: BTreeMap<String, String>,
    pub worker_path: Vec<PathBuf>,
    pub app_log: LogFile,
}

#[derive(Debug, Clone)]
pub struct WorkerStart {
    pub env: Arc<BTreeMap<String, String>>,
    pub units: WorkerUnits,
}

fn spec(
    name: &'static str,
    program: &str,
    args: &[String],
    env: &Arc<BTreeMap<String, String>>,
) -> ProcessSpec {
    ProcessSpec {
        name,
        program: PathBuf::from(program),
        args: args.iter().map(OsString::from).collect(),
        env: env.as_ref().clone(),
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
    pub fn classify(
        code: Option<i32>,
        signal: Option<i32>,
        timed_out: bool,
        lock_held: i32,
    ) -> Self {
        match (timed_out, code, signal) {
            (true, _, _) => Self::TimedOut,
            (false, Some(0), _) => Self::Ok,
            (false, Some(code), _) if code == lock_held => Self::LockHeld,
            (false, Some(code), _) => Self::Failed(code),
            (false, None, Some(signal)) => Self::Killed(signal),
            (false, None, None) => Self::Failed(-1),
        }
    }

    fn label(&self, at: &str, limit: Duration) -> String {
        match self {
            Self::Ok => format!("ok at {at}"),
            Self::LockHeld => format!("lock held by another tick at {at}"),
            Self::Failed(code) => format!("failed with exit code {code} at {at}"),
            Self::Killed(signal) => format!("killed by signal {signal} at {at}"),
            Self::TimedOut => format!("stopped after {} s at {at}", limit.as_secs()),
        }
    }
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

    pub async fn start(&self, start: &WorkerStart) {
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
                &start.units.listen,
                &start.env,
                stopped.clone(),
            ),
            supervise_worker(
                &self.setup,
                &self.board,
                &start.units.watch,
                &start.env,
                stopped.clone(),
            ),
            tokio::spawn(dispatch_timer(
                self.setup.clone(),
                self.board.clone(),
                start.units.dispatch.clone(),
                start.env.clone(),
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
    unit: ServiceUnit,
    env: Arc<BTreeMap<String, String>>,
}

impl Launcher for WorkerLauncher {
    type Process = Spawned;

    fn launch(&mut self) -> Result<Spawned, String> {
        let spec = spec(
            self.unit.row,
            &self.unit.program,
            &self.unit.args,
            &self.env,
        );
        RunningProcess::spawn(&spec, &self.setup.paths)
            .map(|process| Spawned::new(process, Status::Note("running".into())))
            .map_err(|error| format!("cannot start: {error}"))
    }
}

fn supervise_worker(
    setup: &Arc<WorkerSetup>,
    board: &Arc<StatusBoard>,
    unit: &ServiceUnit,
    env: &Arc<BTreeMap<String, String>>,
    stop: watch::Receiver<bool>,
) -> JoinHandle<()> {
    let (_commands, received) = mpsc::channel(1);
    tokio::spawn(supervise(
        unit.row,
        WorkerLauncher {
            setup: setup.clone(),
            unit: unit.clone(),
            env: env.clone(),
        },
        None::<NoReadiness>,
        board.clone(),
        setup.paths.log_file(unit.row),
        stop,
        received,
    ))
}

enum Tick {
    Done(TickResult),
    Stopped,
}

async fn run_tick(
    setup: &WorkerSetup,
    tick: &TickUnit,
    env: &Arc<BTreeMap<String, String>>,
    stop: &mut watch::Receiver<bool>,
) -> Result<Tick, String> {
    use std::os::unix::process::ExitStatusExt;
    let mut process = RunningProcess::spawn(&tick_spec(tick, env), &setup.paths)
        .map_err(|error| format!("cannot start: {error}"))?;
    Ok(tokio::select! {
        status = process.wait() => match status {
            Ok(status) => Tick::Done(TickResult::classify(status.code(), status.signal(), false, tick.lock_held)),
            Err(error) => return Err(format!("could not be waited on: {error}")),
        },
        () = sleep(tick.limit) => {
            let _ = process.stop(tick.grace).await;
            Tick::Done(TickResult::TimedOut)
        }
        () = stop_requested(stop) => {
            let _ = process.stop(STOP_GRACE).await;
            Tick::Stopped
        }
    })
}

fn tick_spec(tick: &TickUnit, env: &Arc<BTreeMap<String, String>>) -> ProcessSpec {
    spec(tick.row, &tick.program, &tick.args, env)
}

async fn dispatch_timer(
    setup: Arc<WorkerSetup>,
    board: Arc<StatusBoard>,
    tick: TickUnit,
    env: Arc<BTreeMap<String, String>>,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        match run_tick(&setup, &tick, &env, &mut stop).await {
            Ok(Tick::Stopped) => return,
            Ok(Tick::Done(result)) => {
                board.set(DISPATCH, Status::Note(result.label(&clock(), tick.limit)));
                if let Some(status) = worker_note(&result, &setup.paths.log_file(DISPATCH)) {
                    board.set(WORKER, status);
                }
            }
            Err(reason) => board.set(DISPATCH, Status::Note(format!("{reason} at {}", clock()))),
        }
        tokio::select! {
            () = sleep(tick.every) => {}
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
                TickResult::classify(code, signal, timed_out, 75),
                expected,
                "code {code:?} signal {signal:?} timed_out {timed_out}"
            );
        }
        assert_eq!(
            TickResult::classify(Some(75), None, false, 11),
            TickResult::Failed(75)
        );
        assert_eq!(
            TickResult::classify(Some(11), None, false, 11),
            TickResult::LockHeld
        );
    }

    #[test]
    fn the_tick_runs_the_program_and_arguments_the_units_name() {
        let tick = TickUnit {
            row: DISPATCH,
            program: "/usr/bin/lockf".into(),
            args: [
                "-s",
                "-t",
                "0",
                "/run/dispatch.lock",
                "/cli/board",
                "internal",
                "dispatch",
            ]
            .map(String::from)
            .to_vec(),
            every: Duration::from_secs(120),
            limit: Duration::from_secs(110),
            grace: Duration::from_secs(10),
            lock_held: 75,
        };
        let env = Arc::new(BTreeMap::from([("PATH".to_owned(), "/bin".to_owned())]));

        let spec = tick_spec(&tick, &env);

        assert_eq!(spec.name, DISPATCH);
        assert_eq!(spec.program, PathBuf::from("/usr/bin/lockf"));
        let args: Vec<_> = spec.args.iter().map(|arg| arg.to_string_lossy()).collect();
        assert_eq!(
            args.join(" "),
            "-s -t 0 /run/dispatch.lock /cli/board internal dispatch"
        );
        assert_eq!(spec.env, *env);
    }
}
