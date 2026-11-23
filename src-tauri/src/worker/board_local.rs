use crate::paths::AppPaths;

const REFUSED: [&str; 4] = ["setup", "self-update", "restart", "init"];

fn quoted(value: &std::path::Path) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', r"'\''"))
}

pub fn script(paths: &AppPaths) -> String {
    let refused = REFUSED.join("|");
    let config = quoted(&paths.worker_config_dir());
    let state = quoted(&paths.worker_state_dir());
    let run = quoted(&paths.worker_run_dir());
    let credentials = quoted(&paths.worker_credentials());
    let board = quoted(&paths.worker_board());
    format!(
        r#"#!/bin/sh
case "$1" in
  {refused})
    printf 'board-local: %s is not available here, the app manages the worker\n' "$1" >&2
    exit 2
    ;;
esac
AGENT_BOARD_CONFIG_DIR={config}
AGENT_BOARD_STATE_DIR={state}
AGENT_BOARD_RUN_DIR={run}
AGENT_BOARD_CREDENTIALS_COMMAND={credentials}
export AGENT_BOARD_CONFIG_DIR AGENT_BOARD_STATE_DIR AGENT_BOARD_RUN_DIR AGENT_BOARD_CREDENTIALS_COMMAND
exec {board} "$@"
"#
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::{Command, Output};

    use super::*;

    const STUB: &str = r#"#!/bin/sh
printf 'PATH=%s\n' "$PATH"
printf 'CONFIG=%s\n' "$AGENT_BOARD_CONFIG_DIR"
printf 'STATE=%s\n' "$AGENT_BOARD_STATE_DIR"
printf 'RUN=%s\n' "$AGENT_BOARD_RUN_DIR"
printf 'CREDENTIALS=%s\n' "$AGENT_BOARD_CREDENTIALS_COMMAND"
for argument in "$@"; do printf 'ARG=%s\n' "$argument"; done
"#;

    fn run(root: &Path, arguments: &[&str]) -> Output {
        let paths = AppPaths::new(root);
        fs::create_dir_all(paths.worker_bin_dir()).unwrap();
        fs::write(paths.worker_board(), STUB).unwrap();
        fs::set_permissions(paths.worker_board(), fs::Permissions::from_mode(0o755)).unwrap();
        let script_file = root.join("board-local");
        fs::write(&script_file, script(&paths)).unwrap();
        Command::new("/bin/sh")
            .arg(&script_file)
            .args(arguments)
            .env("PATH", "/terminal/bin:/usr/bin:/bin")
            .output()
            .unwrap()
    }

    fn awkward_root() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("App Support's $HOME `x`");
        (dir, root)
    }

    #[test]
    fn the_script_runs_the_bundled_cli_on_the_worker_directories_through_a_root_that_needs_quoting()
    {
        let (_guard, root) = awkward_root();

        let output = run(&root, &["dirs", "add", "two words"]);

        assert!(output.status.success());
        let worker = root.join("worker");
        let lines: Vec<String> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            lines,
            [
                "PATH=/terminal/bin:/usr/bin:/bin".to_owned(),
                format!("CONFIG={}", worker.join("config").display()),
                format!("STATE={}", worker.join("state").display()),
                format!("RUN={}", worker.join("run").display()),
                format!(
                    "CREDENTIALS={}",
                    worker.join("worker-credentials").display()
                ),
                "ARG=dirs".to_owned(),
                "ARG=add".to_owned(),
                "ARG=two words".to_owned(),
            ]
        );
    }

    #[test]
    fn the_commands_that_would_reconfigure_the_worker_are_refused_without_running_anything() {
        let (_guard, root) = awkward_root();

        for command in ["setup", "self-update", "restart", "init"] {
            let output = run(&root, &[command, "--force"]);

            assert_eq!(output.status.code(), Some(2), "{command}");
            assert!(output.stdout.is_empty(), "{command}");
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(command),
                "{command}"
            );
        }
    }

    #[test]
    fn a_command_that_only_resembles_a_refused_one_is_passed_through() {
        let (_guard, root) = awkward_root();

        let output = run(&root, &["initial"]);

        assert!(output.status.success());
        assert!(String::from_utf8(output.stdout)
            .unwrap()
            .contains("ARG=initial"));
    }
}
