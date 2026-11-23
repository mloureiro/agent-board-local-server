use std::future::{pending, Future};
use std::path::PathBuf;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{interval_at, sleep, Instant};

use super::policy::{decide, Decision};
use super::process::{rotate_if_larger, stop_requested, with_log_tail, LOG_LIMIT};
use super::state::{Status, StatusBoard};

const STOP_GRACE: Duration = Duration::from_secs(10);
const STABLE_RUN: Duration = Duration::from_secs(10 * 60);
const TICK: Duration = Duration::from_secs(60);
const MISSES_ALLOWED: u32 = 3;

pub trait Launcher: Send {
    type Process: Supervised;

    fn launch(&mut self) -> Result<Self::Process, String>;
}

pub trait Supervised: Send {
    /// Resolves with a description of how the process ended.
    fn wait(&mut self) -> impl Future<Output = String> + Send;
    fn stop(&mut self, grace: Duration) -> impl Future<Output = ()> + Send;
    /// What the board shows while the process runs.
    fn running_status(&self) -> Status;
}

/// `running` is the process's `running_status`, so a check can find where to look without borrowing the process.
pub trait Readiness: Send + Sync {
    /// True once the process is ready, false when this gives up waiting.
    fn first(&self, running: &Status) -> impl Future<Output = bool> + Send;
    /// One check, bounded by its own timeout.
    fn probe(&self, running: &Status) -> impl Future<Output = bool> + Send;
}

pub struct NoReadiness;

impl Readiness for NoReadiness {
    async fn first(&self, _running: &Status) -> bool {
        true
    }

    async fn probe(&self, _running: &Status) -> bool {
        true
    }
}

pub enum Command {
    /// Starts again at once with the restart count back at zero.
    Restart,
    /// Stops the process and keeps it down; `held` answers once nothing is running.
    Hold {
        note: String,
        held: oneshot::Sender<()>,
    },
    Resume,
}

pub struct Control {
    stop: watch::Sender<bool>,
    pub commands: mpsc::Sender<Command>,
}

impl Control {
    pub fn new() -> (Self, watch::Receiver<bool>, mpsc::Receiver<Command>) {
        let (stop, stopped) = watch::channel(false);
        let (commands, received) = mpsc::channel(4);
        (Self { stop, commands }, stopped, received)
    }

    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }
}

enum Event {
    Stop,
    Restart,
    Resume,
    Elapsed,
    Hold {
        note: String,
        held: oneshot::Sender<()>,
    },
    Failed(String),
}

impl From<Command> for Event {
    fn from(command: Command) -> Self {
        match command {
            Command::Restart => Self::Restart,
            Command::Resume => Self::Resume,
            Command::Hold { note, held } => Self::Hold { note, held },
        }
    }
}

struct Loop {
    name: &'static str,
    board: Arc<StatusBoard>,
    log: PathBuf,
    stop: watch::Receiver<bool>,
    commands: mpsc::Receiver<Command>,
}

impl Loop {
    async fn command(commands: &mut mpsc::Receiver<Command>) -> Command {
        match commands.recv().await {
            Some(command) => command,
            None => pending().await,
        }
    }

    /// Launches, waits for readiness and then watches the running process until something ends it.
    async fn run<L: Launcher, R: Readiness>(
        &mut self,
        launcher: &mut L,
        readiness: Option<&R>,
        restarts: &mut usize,
    ) -> Event {
        self.board.set(self.name, Status::Starting);
        let mut process = match launcher.launch() {
            Ok(process) => process,
            Err(reason) => return Event::Failed(reason),
        };
        let running = process.running_status();

        if let Some(readiness) = readiness {
            let waiting = Instant::now();
            let ready = tokio::select! {
                ready = readiness.first(&running) => ready,
                exit = process.wait() => return Event::Failed(exit),
                () = stop_requested(&mut self.stop) => {
                    process.stop(STOP_GRACE).await;
                    return Event::Stop;
                }
            };
            if !ready {
                process.stop(STOP_GRACE).await;
                return Event::Failed(format!(
                    "not healthy after {} s",
                    waiting.elapsed().as_secs()
                ));
            }
        }

        self.board.set(self.name, running.clone());
        let mut stable = pin!(sleep(STABLE_RUN));
        let mut counting = true;
        let mut tick = interval_at(Instant::now() + TICK, TICK);
        let mut misses = 0;
        loop {
            tokio::select! {
                exit = process.wait() => return Event::Failed(exit),
                () = stop_requested(&mut self.stop) => {
                    process.stop(STOP_GRACE).await;
                    return Event::Stop;
                }
                command = Self::command(&mut self.commands) => match Event::from(command) {
                    Event::Resume => {}
                    event => {
                        process.stop(STOP_GRACE).await;
                        return event;
                    }
                },
                () = &mut stable, if counting => {
                    *restarts = 0;
                    counting = false;
                }
                _ = tick.tick() => {
                    let _ = rotate_if_larger(&self.log, LOG_LIMIT);
                    let Some(readiness) = readiness else { continue };
                    if readiness.probe(&running).await {
                        misses = 0;
                        continue;
                    }
                    misses += 1;
                    if misses >= MISSES_ALLOWED {
                        process.stop(STOP_GRACE).await;
                        return Event::Failed(format!(
                            "did not answer /healthz {MISSES_ALLOWED} times in a row"
                        ));
                    }
                }
            }
        }
    }

    /// Sits out a backoff (or, with `None`, waits for a command) and says what ended the wait.
    async fn wait_out(&mut self, after: Option<Duration>) -> Event {
        let mut timer = pin!(async {
            match after {
                Some(after) => sleep(after).await,
                None => pending().await,
            }
        });
        loop {
            tokio::select! {
                () = &mut timer => return Event::Elapsed,
                () = stop_requested(&mut self.stop) => return Event::Stop,
                command = Self::command(&mut self.commands) => match Event::from(command) {
                    Event::Resume => {}
                    event => return event,
                },
            }
        }
    }

    /// Returns true when resumed, false when the app is stopping.
    async fn hold(&mut self, note: String, held: oneshot::Sender<()>) -> bool {
        let (mut note, mut held) = (note, held);
        loop {
            self.board.set(self.name, Status::Note(note));
            let _ = held.send(());
            loop {
                tokio::select! {
                    () = stop_requested(&mut self.stop) => return false,
                    command = Self::command(&mut self.commands) => match command {
                        Command::Resume => return true,
                        Command::Restart => {}
                        Command::Hold { note: next, held: acknowledge } => {
                            (note, held) = (next, acknowledge);
                            break;
                        }
                    },
                }
            }
        }
    }
}

pub async fn supervise<L: Launcher, R: Readiness>(
    name: &'static str,
    mut launcher: L,
    readiness: Option<R>,
    board: Arc<StatusBoard>,
    log: PathBuf,
    stop: watch::Receiver<bool>,
    commands: mpsc::Receiver<Command>,
) {
    let mut this = Loop {
        name,
        board,
        log,
        stop,
        commands,
    };
    let mut restarts = 0;
    loop {
        let mut event = this
            .run(&mut launcher, readiness.as_ref(), &mut restarts)
            .await;
        loop {
            event = match event {
                Event::Stop => {
                    this.board.set(name, Status::Stopped);
                    return;
                }
                Event::Elapsed | Event::Resume => break,
                Event::Restart => {
                    restarts = 0;
                    break;
                }
                Event::Hold { note, held } => {
                    if this.hold(note, held).await {
                        break;
                    }
                    Event::Stop
                }
                Event::Failed(reason) => {
                    let reason = with_log_tail(reason, &this.log);
                    match decide(restarts) {
                        Decision::Restart { after } => {
                            this.board.set(
                                name,
                                Status::Crashed {
                                    reason,
                                    restart_in: Some(after),
                                },
                            );
                            restarts += 1;
                            this.wait_out(Some(after)).await
                        }
                        Decision::GiveUp => {
                            this.board.set(
                                name,
                                Status::Crashed {
                                    reason,
                                    restart_in: None,
                                },
                            );
                            this.wait_out(None).await
                        }
                    }
                }
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use tokio::task::JoinHandle;
    use tokio::time::advance;

    use super::*;

    const NAME: &str = "fake";

    #[derive(Clone, Default)]
    struct Script {
        exits: Arc<Mutex<Vec<Option<oneshot::Sender<String>>>>>,
        stops: Arc<AtomicUsize>,
        probes: Arc<AtomicUsize>,
        answers: Arc<Mutex<VecDeque<bool>>>,
    }

    impl Script {
        fn launches(&self) -> usize {
            self.exits.lock().unwrap().len()
        }

        fn stops(&self) -> usize {
            self.stops.load(Ordering::SeqCst)
        }

        fn probes(&self) -> usize {
            self.probes.load(Ordering::SeqCst)
        }

        fn exit_latest(&self) {
            let sender = self.exits.lock().unwrap().last_mut().and_then(Option::take);
            sender
                .expect("a running process")
                .send("gone".into())
                .unwrap();
        }

        fn answer(&self, answers: &[bool]) {
            self.answers.lock().unwrap().extend(answers);
        }
    }

    struct FakeProcess {
        exit: oneshot::Receiver<String>,
        stops: Arc<AtomicUsize>,
    }

    impl Supervised for FakeProcess {
        async fn wait(&mut self) -> String {
            match (&mut self.exit).await {
                Ok(reason) => reason,
                Err(_) => pending().await,
            }
        }

        async fn stop(&mut self, _grace: Duration) {
            self.stops.fetch_add(1, Ordering::SeqCst);
        }

        fn running_status(&self) -> Status {
            Status::Running { port: 1 }
        }
    }

    struct FakeLauncher(Script);

    impl Launcher for FakeLauncher {
        type Process = FakeProcess;

        fn launch(&mut self) -> Result<FakeProcess, String> {
            let (sender, exit) = oneshot::channel();
            self.0.exits.lock().unwrap().push(Some(sender));
            Ok(FakeProcess {
                exit,
                stops: self.0.stops.clone(),
            })
        }
    }

    struct FakeReadiness {
        script: Script,
        first_after: Duration,
        first_result: bool,
    }

    impl Readiness for FakeReadiness {
        async fn first(&self, _running: &Status) -> bool {
            sleep(self.first_after).await;
            self.first_result
        }

        async fn probe(&self, _running: &Status) -> bool {
            self.script.probes.fetch_add(1, Ordering::SeqCst);
            self.script
                .answers
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(true)
        }
    }

    struct Rig {
        script: Script,
        board: Arc<StatusBoard>,
        control: Control,
        task: JoinHandle<()>,
    }

    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    async fn elapse(total: Duration) {
        for _ in 0..total.as_secs() {
            advance(Duration::from_secs(1)).await;
            settle().await;
        }
    }

    async fn start(readiness: Option<FakeReadiness>) -> Rig {
        let script = readiness
            .as_ref()
            .map_or_else(Script::default, |readiness| readiness.script.clone());
        start_with(script, readiness).await
    }

    async fn start_with(script: Script, readiness: Option<FakeReadiness>) -> Rig {
        let board = Arc::new(StatusBoard::default());
        let (control, stopped, commands) = Control::new();
        let launcher = FakeLauncher(script.clone());
        let log = PathBuf::from("/nonexistent/fake.log");
        let task = match readiness {
            Some(readiness) => tokio::spawn(supervise(
                NAME,
                launcher,
                Some(readiness),
                board.clone(),
                log,
                stopped,
                commands,
            )),
            None => tokio::spawn(supervise(
                NAME,
                launcher,
                None::<NoReadiness>,
                board.clone(),
                log,
                stopped,
                commands,
            )),
        };
        settle().await;
        Rig {
            script,
            board,
            control,
            task,
        }
    }

    fn ready_after(script: &Script, first_after: Duration, first_result: bool) -> FakeReadiness {
        FakeReadiness {
            script: script.clone(),
            first_after,
            first_result,
        }
    }

    impl Rig {
        fn status(&self) -> Status {
            self.board.get(NAME).expect("a status")
        }

        fn backoff(&self) -> Option<Duration> {
            match self.status() {
                Status::Crashed { restart_in, .. } => restart_in,
                other => panic!("expected a crash, found {other:?}"),
            }
        }

        async fn crash(&self) {
            self.script.exit_latest();
            settle().await;
        }

        async fn send(&self, command: Command) {
            self.control.commands.send(command).await.unwrap();
            settle().await;
        }

        async fn give_up(&self) {
            loop {
                self.crash().await;
                match self.backoff() {
                    Some(after) => elapse(after).await,
                    None => break,
                }
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn exits_back_off_longer_each_time_and_the_fourth_gives_up() {
        let rig = start(None).await;

        let mut waits = Vec::new();
        for launches in 1..=3 {
            assert_eq!(rig.script.launches(), launches);
            rig.crash().await;
            let after = rig.backoff().expect("a restart is scheduled");
            elapse(after - Duration::from_secs(1)).await;
            assert_eq!(rig.script.launches(), launches);
            elapse(Duration::from_secs(1)).await;
            waits.push(after);
        }
        assert!(waits.windows(2).all(|pair| pair[0] < pair[1]));

        rig.crash().await;
        assert_eq!(rig.backoff(), None);
        elapse(Duration::from_secs(3600)).await;
        assert_eq!(rig.script.launches(), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_of_ten_minutes_resets_the_count_and_one_second_less_does_not() {
        let rig = start(None).await;
        rig.crash().await;
        let first = rig.backoff().unwrap();
        elapse(first).await;

        elapse(STABLE_RUN - Duration::from_secs(1)).await;
        rig.crash().await;
        let second = rig.backoff().unwrap();
        assert!(second > first);
        elapse(second).await;

        elapse(STABLE_RUN).await;
        rig.crash().await;
        assert_eq!(rig.backoff(), Some(first));
    }

    #[tokio::test(start_paused = true)]
    async fn with_readiness_the_stable_run_counts_from_ready_not_from_spawn() {
        let script = Script::default();
        let wait = Duration::from_secs(120);
        let rig = start_with(script.clone(), Some(ready_after(&script, wait, true))).await;
        elapse(wait).await;
        rig.crash().await;
        let first = rig.backoff().unwrap();
        elapse(first).await;

        elapse(wait + STABLE_RUN - Duration::from_secs(1)).await;
        rig.crash().await;

        assert!(rig.backoff().unwrap() > first);
    }

    async fn probed(answers: &[bool]) -> Rig {
        let script = Script::default();
        script.answer(answers);
        start_with(
            script.clone(),
            Some(ready_after(&script, Duration::ZERO, true)),
        )
        .await
    }

    #[tokio::test(start_paused = true)]
    async fn three_misses_in_a_row_fail_the_process() {
        let rig = probed(&[false, false, false]).await;

        elapse(TICK * 3).await;

        assert_eq!(rig.script.stops(), 1);
        match rig.status() {
            Status::Crashed { reason, .. } => assert!(reason.contains("3 times in a row")),
            other => panic!("expected a crash, found {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_success_between_misses_keeps_the_process_running() {
        let rig = probed(&[false, false, true, false, false]).await;

        elapse(TICK * 6).await;

        assert_eq!(rig.script.probes(), 6);
        assert_eq!(rig.script.stops(), 0);
        assert_eq!(rig.status(), Status::Running { port: 1 });
    }

    #[tokio::test(start_paused = true)]
    async fn a_process_that_is_never_ready_fails_as_not_healthy() {
        let script = Script::default();
        let limit = Duration::from_secs(180);
        let rig = start_with(script.clone(), Some(ready_after(&script, limit, false))).await;

        assert_eq!(rig.status(), Status::Starting);
        elapse(limit).await;

        assert_eq!(rig.script.stops(), 1);
        match rig.status() {
            Status::Crashed { reason, .. } => assert!(reason.starts_with("not healthy after")),
            other => panic!("expected a crash, found {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stop_ends_in_stopped_from_every_state() {
        for phase in ["waiting", "running", "backoff", "gave up", "held"] {
            let script = Script::default();
            let rig = if phase == "waiting" {
                let readiness = ready_after(&script, Duration::from_secs(1000), true);
                start_with(script.clone(), Some(readiness)).await
            } else {
                start_with(script, None).await
            };
            match phase {
                "backoff" => {
                    rig.crash().await;
                }
                "gave up" => rig.give_up().await,
                "held" => {
                    let (held, _) = oneshot::channel();
                    rig.send(Command::Hold {
                        note: "held".into(),
                        held,
                    })
                    .await;
                }
                _ => {}
            }

            rig.control.stop();
            settle().await;

            assert!(rig.task.is_finished(), "{phase} did not finish");
            assert_eq!(rig.status(), Status::Stopped, "{phase}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn restart_starts_at_once_with_the_count_back_at_zero() {
        for phase in ["running", "backoff", "gave up"] {
            let rig = start(None).await;
            rig.crash().await;
            let first = rig.backoff().unwrap();
            match phase {
                "running" => elapse(first).await,
                "gave up" => {
                    elapse(first).await;
                    rig.give_up().await;
                }
                _ => {}
            }
            let launches = rig.script.launches();

            rig.send(Command::Restart).await;

            assert_eq!(rig.script.launches(), launches + 1, "{phase}");
            assert_eq!(rig.status(), Status::Running { port: 1 }, "{phase}");
            rig.crash().await;
            assert_eq!(rig.backoff(), Some(first), "{phase}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn hold_stops_the_process_answers_and_keeps_it_down_until_resumed() {
        let script = Script::default();
        let rig = start_with(
            script.clone(),
            Some(ready_after(&script, Duration::ZERO, true)),
        )
        .await;
        let (held, answered) = oneshot::channel();

        rig.send(Command::Hold {
            note: "backing up".into(),
            held,
        })
        .await;

        assert!(answered.await.is_ok());
        assert_eq!(rig.script.stops(), 1);
        assert_eq!(rig.status(), Status::Note("backing up".into()));

        rig.send(Command::Restart).await;
        elapse(STABLE_RUN).await;
        assert_eq!(rig.script.launches(), 1);
        assert_eq!(rig.script.probes(), 0);

        rig.send(Command::Resume).await;
        assert_eq!(rig.script.launches(), 2);
        assert_eq!(rig.status(), Status::Running { port: 1 });
    }
}
