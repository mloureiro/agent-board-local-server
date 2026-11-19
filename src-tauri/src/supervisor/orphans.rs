use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use tokio::process::Command;

use super::process::{signal_group, Pidfile};

const POLL: Duration = Duration::from_millis(100);

pub struct Observed {
    pub state: String,
    pub command: String,
}

pub async fn observe(pid: u32) -> Option<Observed> {
    let output = Command::new("/bin/ps")
        .args(["-ww", "-o", "stat=,command=", "-p", &pid.to_string()])
        .output()
        .await
        .ok()?;
    let line = String::from_utf8_lossy(&output.stdout);
    let (state, command) = line.trim().split_once(char::is_whitespace)?;
    Some(Observed {
        state: state.to_owned(),
        command: command.trim().to_owned(),
    })
}

/// A recorded pid is only trusted while the live process still runs the recorded program,
/// because the number may have been handed to something unrelated since.
async fn is_ours(pidfile: &Pidfile) -> bool {
    match observe(pidfile.pid).await {
        Some(seen) => !seen.state.starts_with('Z') && seen.command.starts_with(&pidfile.argv0),
        None => false,
    }
}

/// Stops what an earlier run of the app left behind, then removes every pidfile in `run_dir`.
pub async fn sweep(run_dir: &Path, grace: Duration) {
    let Ok(entries) = fs::read_dir(run_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("pid") {
            continue;
        }
        if let Some(pidfile) = Pidfile::read(&path) {
            if is_ours(&pidfile).await {
                terminate(&pidfile, grace).await;
            }
        }
        let _ = fs::remove_file(&path);
    }
}

async fn terminate(pidfile: &Pidfile, grace: Duration) {
    signal_group(pidfile.pid, libc::SIGTERM);
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if !is_ours(pidfile).await {
            return;
        }
        tokio::time::sleep(POLL).await;
    }
    signal_group(pidfile.pid, libc::SIGKILL);
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

    use super::*;

    fn sleeper() -> Child {
        Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap()
    }

    fn record(dir: &Path, name: &str, child: &Child, argv0: &str) {
        Pidfile {
            pid: child.id(),
            started_at: 0,
            argv0: argv0.into(),
        }
        .write(&dir.join(format!("{name}.pid")))
        .unwrap();
    }

    #[tokio::test]
    async fn a_process_running_another_program_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut other = sleeper();
        record(
            dir.path(),
            "backend",
            &other,
            "/Applications/Agent Board Server.app/Contents/MacOS/board-server",
        );

        sweep(dir.path(), Duration::from_millis(300)).await;

        assert!(
            other.try_wait().unwrap().is_none(),
            "the unrelated process was signalled"
        );
        assert!(!dir.path().join("backend.pid").exists());
        other.kill().unwrap();
        other.wait().unwrap();
    }

    #[tokio::test]
    async fn a_process_running_the_recorded_program_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let mut orphan = sleeper();
        record(dir.path(), "backend", &orphan, "/bin/sleep");

        sweep(dir.path(), Duration::from_secs(5)).await;

        assert!(
            orphan.wait().unwrap().code().is_none(),
            "expected a signal exit"
        );
        assert!(!dir.path().join("backend.pid").exists());
    }
}
