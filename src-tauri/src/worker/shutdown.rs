use std::fs;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use tokio::time::timeout;

use super::processes::WorkerSetup;
use crate::supervisor::orphans::{observe, Observed};
use crate::supervisor::process::{signal_group, RunningProcess};

const SHUTDOWN: &str = "worker-shutdown";
const REPORT_MARGIN: Duration = Duration::from_secs(15);

/// The backend and the proxy must still be up: each worker reports its run as paused to the board before it exits.
pub async fn pause_runs(setup: &WorkerSetup, grace: Duration) {
    let seconds = grace.as_secs().to_string();
    let spec = setup.spec(SHUTDOWN, &["internal", "shutdown", "--grace", &seconds]);
    let Ok(mut process) = RunningProcess::spawn(&spec, &setup.paths) else {
        return;
    };
    if timeout(grace + REPORT_MARGIN, process.wait())
        .await
        .is_err()
    {
        let _ = process.stop(Duration::from_secs(2)).await;
    }
}

#[derive(Deserialize)]
struct WorkerPid {
    pid: u32,
}

fn is_worker_command(command: &str, task_id: &str) -> bool {
    command
        .trim()
        .ends_with(&format!(" internal worker {task_id}"))
}

/// A pid file is trusted only while that process's command line still ends in `internal worker <id>`.
pub async fn kill_stragglers(run_dir: &Path) {
    let Ok(entries) = fs::read_dir(run_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(task_id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".pid"))
            .map(str::to_owned)
        else {
            continue;
        };
        let Some(record) = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<WorkerPid>(&bytes).ok())
        else {
            continue;
        };
        let verified = matches!(
            observe(record.pid).await,
            Some(Observed { state, command })
                if !state.starts_with('Z') && is_worker_command(&command, &task_id)
        );
        if !verified {
            continue;
        }
        let agent = fs::read_to_string(run_dir.join(format!("{task_id}.pgid")))
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
            .filter(|pgid| *pgid > 1);
        for pgid in std::iter::once(record.pid).chain(agent) {
            signal_group(pgid, libc::SIGKILL);
        }
        // SAFETY: a plain signal to the verified worker, covering the case where it is not its group's leader.
        if let Ok(pid) = i32::try_from(record.pid) {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

    use super::*;

    fn spawn_worker_like(dir: &Path, task_id: &str, argv_tail: &[&str]) -> Child {
        let script = dir.join("fake-board");
        fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let child = Command::new(&script)
            .args(argv_tail)
            .process_group(0)
            .spawn()
            .unwrap();
        fs::write(
            dir.join(format!("{task_id}.pid")),
            format!(
                "{{\"pid\":{},\"startedAt\":\"2026-11-19T00:00:00.000Z\"}}",
                child.id()
            ),
        )
        .unwrap();
        child
    }

    fn gone(child: &mut Child) -> bool {
        for _ in 0..50 {
            if child.try_wait().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    #[test]
    fn only_a_process_still_running_the_worker_command_for_that_task_is_killed() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut worker =
            spawn_worker_like(dir.path(), "abc12345", &["internal", "worker", "abc12345"]);
        let other_dir = tempfile::tempdir().unwrap();
        let mut reused = spawn_worker_like(other_dir.path(), "abc12345", &["internal", "listen"]);

        std::thread::sleep(Duration::from_millis(300));
        runtime.block_on(kill_stragglers(dir.path()));
        runtime.block_on(kill_stragglers(other_dir.path()));

        assert!(gone(&mut worker), "the verified worker survived");
        assert!(
            reused.try_wait().unwrap().is_none(),
            "a process with another command line was killed"
        );
        reused.kill().unwrap();
        reused.wait().unwrap();
    }
}
