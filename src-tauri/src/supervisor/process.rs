use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::process::{Child, Command};

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
    pub started_at: u64,
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
            started_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs()),
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
    use super::*;

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
