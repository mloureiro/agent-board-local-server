use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::paths::AppPaths;

const INHERITED: [&str; 6] = ["HOME", "USER", "LOGNAME", "SHELL", "TMPDIR", "LANG"];

/// The whole environment of every worker process, built from scratch. The `PATH` is written out here because an app
/// started from Finder has only `/usr/bin:/bin:/usr/sbin:/sbin`, where neither `claude` nor `gh` is found.
pub fn environment(
    paths: &AppPaths,
    credentials_command: &Path,
    worker_path: &[PathBuf],
    app_env: &dyn Fn(&str) -> Option<String>,
) -> BTreeMap<String, String> {
    let home = app_env("HOME");
    let mut path = vec![paths.worker_bin_dir().to_string_lossy().into_owned()];
    path.extend(
        worker_path
            .iter()
            .map(|entry| entry.to_string_lossy().into_owned()),
    );
    if let Some(home) = &home {
        path.push(format!("{home}/.local/bin"));
    }
    path.extend(
        [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
        ]
        .map(String::from),
    );

    let mut env = BTreeMap::from([
        ("PATH".to_owned(), path.join(":")),
        (
            "AGENT_BOARD_CONFIG_DIR".to_owned(),
            paths.worker_config_dir().to_string_lossy().into_owned(),
        ),
        (
            "AGENT_BOARD_STATE_DIR".to_owned(),
            paths.worker_state_dir().to_string_lossy().into_owned(),
        ),
        (
            "AGENT_BOARD_RUN_DIR".to_owned(),
            paths.worker_run_dir().to_string_lossy().into_owned(),
        ),
        (
            "AGENT_BOARD_CREDENTIALS_COMMAND".to_owned(),
            credentials_command.to_string_lossy().into_owned(),
        ),
    ]);
    for name in INHERITED.into_iter().chain(["SSH_AUTH_SOCK"]) {
        if let Some(value) = app_env(name) {
            env.insert(name.to_owned(), value);
        }
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worker_environment_is_exactly_this() {
        let paths = AppPaths::new("/Users/ines/Library/Application Support/app");
        let host: BTreeMap<&str, &str> = BTreeMap::from([
            ("HOME", "/Users/ines"),
            ("USER", "ines"),
            ("LOGNAME", "ines"),
            ("SHELL", "/bin/zsh"),
            ("TMPDIR", "/var/folders/xx/T/"),
            ("LANG", "en_GB.UTF-8"),
            ("SSH_AUTH_SOCK", "/private/tmp/agent.sock"),
            ("AGENT_BOARD_TOKEN", "abm_leaked"),
            ("AGENT_BOARD_DATA_KEY", "abk1_leaked"),
            ("AGENT_BOARD_CONFIG_DIR", "/Users/ines/.config/agent-board"),
            ("ANTHROPIC_API_KEY", "sk-leaked"),
        ]);
        let app = PathBuf::from("/App/Contents/MacOS/agent-board-local-server");

        let extra = [
            PathBuf::from("/opt/tools/bin"),
            PathBuf::from("/Users/ines/bin"),
        ];

        let env = environment(&paths, &app, &extra, &|name| {
            host.get(name).map(|v| (*v).to_owned())
        });

        let root = "/Users/ines/Library/Application Support/app/worker";
        assert_eq!(
            env,
            BTreeMap::from(
                [
                    (
                        "PATH",
                        format!(
                            "{root}/bin:/opt/tools/bin:/Users/ines/bin:/Users/ines/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
                        )
                    ),
                    ("AGENT_BOARD_CONFIG_DIR", format!("{root}/config")),
                    ("AGENT_BOARD_STATE_DIR", format!("{root}/state")),
                    ("AGENT_BOARD_RUN_DIR", format!("{root}/run")),
                    (
                        "AGENT_BOARD_CREDENTIALS_COMMAND",
                        "/App/Contents/MacOS/agent-board-local-server".to_owned()
                    ),
                    ("HOME", "/Users/ines".to_owned()),
                    ("USER", "ines".to_owned()),
                    ("LOGNAME", "ines".to_owned()),
                    ("SHELL", "/bin/zsh".to_owned()),
                    ("TMPDIR", "/var/folders/xx/T/".to_owned()),
                    ("LANG", "en_GB.UTF-8".to_owned()),
                    ("SSH_AUTH_SOCK", "/private/tmp/agent.sock".to_owned()),
                ]
                .map(|(name, value)| (name.to_owned(), value))
            )
        );
    }
}
