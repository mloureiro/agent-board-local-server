use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::{Child, Command};

use super::state::Status;
use super::supervise::Supervised;
use crate::paths::AppPaths;

pub const LOG_LIMIT: u64 = 10 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub name: &'static str,
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pidfile {
    pub pid: u32,
    pub argv0: String,
}

impl Pidfile {
    pub fn read(path: &Path) -> Option<Self> {
        serde_json::from_slice(&fs::read(path).ok()?).ok()
    }

    pub fn write(&self, path: &Path) -> io::Result<()> {
        fs::write(path, serde_json::to_vec(self).expect("pidfile serialises"))
    }
}

pub struct RunningProcess {
    child: Child,
    pid: u32,
    pidfile: PathBuf,
}

impl RunningProcess {
    /// The child leads its own process group, so stopping it also stops anything it started.
    pub fn spawn(spec: &ProcessSpec, paths: &AppPaths) -> io::Result<Self> {
        fs::create_dir_all(paths.logs_dir())?;
        fs::create_dir_all(paths.run_dir())?;

        let log_path = paths.log_file(spec.name);
        rotate_if_larger(&log_path, LOG_LIMIT)?;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;

        let child = Command::new(&spec.program)
            .args(&spec.args)
            .env_clear()
            .envs(&spec.env)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .process_group(0)
            .spawn()?;
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("the process exited before it could be tracked"))?;

        let pidfile = paths.pidfile(spec.name);
        Pidfile {
            pid,
            argv0: spec.program.to_string_lossy().into_owned(),
        }
        .write(&pidfile)?;

        Ok(Self {
            child,
            pid,
            pidfile,
        })
    }

    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.child.wait().await;
        let _ = fs::remove_file(&self.pidfile);
        status
    }

    /// SIGTERM to the group, then SIGKILL once `grace` has passed.
    pub async fn stop(&mut self, grace: Duration) -> io::Result<ExitStatus> {
        if let Some(status) = self.child.try_wait()? {
            let _ = fs::remove_file(&self.pidfile);
            return Ok(status);
        }
        signal_group(self.pid, libc::SIGTERM);
        if tokio::time::timeout(grace, self.child.wait())
            .await
            .is_err()
        {
            signal_group(self.pid, libc::SIGKILL);
        }
        self.wait().await
    }
}

pub struct Spawned {
    process: RunningProcess,
    status: Status,
}

impl Spawned {
    pub fn new(process: RunningProcess, status: Status) -> Self {
        Self { process, status }
    }
}

impl Supervised for Spawned {
    async fn wait(&mut self) -> String {
        describe_exit(self.process.wait().await)
    }

    async fn stop(&mut self, grace: Duration) {
        let _ = self.process.stop(grace).await;
    }

    fn running_status(&self) -> Status {
        self.status.clone()
    }
}

pub fn signal_group(pgid: u32, signal: i32) {
    let Ok(pgid) = i32::try_from(pgid) else {
        return;
    };
    // SAFETY: killpg only sends a signal; a stale group is reported as an error we ignore.
    unsafe { libc::killpg(pgid, signal) };
}

/// Keeps one previous generation. Copying then truncating is safe for a writer that appends.
pub fn rotate_if_larger(path: &Path, limit: u64) -> io::Result<()> {
    let Ok(meta) = fs::metadata(path) else {
        return Ok(());
    };
    if meta.len() <= limit {
        return Ok(());
    }
    let mut rotated = path.as_os_str().to_owned();
    rotated.push(".1");
    fs::copy(path, &rotated)?;
    OpenOptions::new().write(true).open(path)?.set_len(0)
}

pub fn last_log_line(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let line = text.lines().rev().find(|line| !line.trim().is_empty())?;
    Some(line.chars().take(160).collect())
}

pub fn describe_exit(status: io::Result<ExitStatus>) -> String {
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

pub fn with_log_tail(reason: String, log: &Path) -> String {
    if !reason.starts_with("exited with code") {
        return reason;
    }
    match last_log_line(log) {
        Some(line) => format!("{reason}: {line}"),
        None => reason,
    }
}

pub async fn stop_requested(stop: &mut tokio::sync::watch::Receiver<bool>) {
    while !*stop.borrow() {
        if stop.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::time::Instant;

    use super::*;

    const GRACE: Duration = Duration::from_millis(200);
    const SLACK: Duration = Duration::from_secs(5);
    const LONG_GRACE: Duration = Duration::from_secs(30);
    const FAST: Duration = Duration::from_secs(10);
    const DEADLINE: Duration = Duration::from_secs(5);
    const POLL: Duration = Duration::from_millis(10);

    fn spec(name: &'static str, program: &str, args: &[&str]) -> ProcessSpec {
        ProcessSpec {
            name,
            program: program.into(),
            args: args.iter().map(OsString::from).collect(),
            env: BTreeMap::new(),
        }
    }

    struct KillOnDrop {
        group: Option<u32>,
        pid: Option<i32>,
    }

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            if let Some(group) = self.group {
                signal_group(group, libc::SIGKILL);
            }
            if let Some(pid) = self.pid {
                // SAFETY: kill only sends a signal; a pid that is gone is reported as an error we ignore.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }

    async fn ready_pid(log: &Path) -> i32 {
        let started = Instant::now();
        loop {
            let pid = fs::read_to_string(log).ok().and_then(|text| {
                text.lines()
                    .find_map(|line| line.strip_prefix("ready ")?.trim().parse().ok())
            });
            if let Some(pid) = pid {
                return pid;
            }
            assert!(
                started.elapsed() < DEADLINE,
                "the child never reported ready"
            );
            tokio::time::sleep(POLL).await;
        }
    }

    async fn gone(pid: i32) -> bool {
        let started = Instant::now();
        while started.elapsed() < DEADLINE {
            // SAFETY: signal 0 only checks that the pid exists.
            let missing = unsafe { libc::kill(pid, 0) } == -1
                && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            if missing {
                return true;
            }
            tokio::time::sleep(POLL).await;
        }
        false
    }

    #[tokio::test]
    async fn a_child_that_ignores_sigterm_is_killed_with_its_group_after_the_grace() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(dir.path());
        let script = r#"trap "" TERM; /bin/sleep 60 & echo "ready $!"; wait"#;
        let mut process =
            RunningProcess::spawn(&spec("stubborn", "/bin/sh", &["-c", script]), &paths).unwrap();
        let mut guard = KillOnDrop {
            group: Some(process.pid),
            pid: None,
        };
        let grandchild = ready_pid(&paths.log_file("stubborn")).await;
        guard.pid = Some(grandchild);

        let started = Instant::now();
        let stopped = tokio::time::timeout(GRACE + SLACK, process.stop(GRACE)).await;
        guard.group = None;

        assert!(started.elapsed() >= GRACE);
        let Ok(Ok(status)) = stopped else {
            panic!("stop did not finish: {stopped:?}");
        };
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert!(!paths.pidfile("stubborn").exists());
        assert!(gone(grandchild).await);
        guard.pid = None;
    }

    #[tokio::test]
    async fn a_child_that_obeys_sigterm_stops_before_the_grace_runs_out() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(dir.path());
        let mut process =
            RunningProcess::spawn(&spec("obedient", "/bin/sleep", &["60"]), &paths).unwrap();
        let mut guard = KillOnDrop {
            group: Some(process.pid),
            pid: None,
        };

        let stopped = tokio::time::timeout(FAST, process.stop(LONG_GRACE)).await;
        guard.group = None;

        let Ok(Ok(status)) = stopped else {
            panic!("stop did not finish: {stopped:?}");
        };
        assert_eq!(status.signal(), Some(libc::SIGTERM));
        assert!(!paths.pidfile("obedient").exists());
    }

    #[test]
    fn a_log_over_the_limit_moves_to_the_previous_generation() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("backend.log");

        fs::write(&log, "small").unwrap();
        rotate_if_larger(&log, 10).unwrap();
        assert_eq!(fs::read_to_string(&log).unwrap(), "small");

        fs::write(&log, "a line well over ten bytes").unwrap();
        rotate_if_larger(&log, 10).unwrap();
        assert_eq!(fs::read_to_string(&log).unwrap(), "");
        assert_eq!(
            fs::read_to_string(dir.path().join("backend.log.1")).unwrap(),
            "a line well over ten bytes"
        );
    }
}
