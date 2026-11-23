use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::process::Command;

use super::enrol::scrub;
use crate::localtime::timestamp;

const DROPPED: &str = include_str!("../../../worker-check/dropped-checks.json");
const TOOLS: [&str; 2] = ["claude", "gh"];
const DOCTOR_LIMIT: Duration = Duration::from_secs(60);
const OUTPUT_EXCERPT: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Fail,
    Warn,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Check {
    pub name: String,
    pub level: Level,
    pub message: String,
    pub hint: Option<String>,
}

impl Check {
    fn new(name: &str, level: Level, message: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            level,
            message: message.into(),
            hint: None,
        }
    }

    fn is_problem(&self) -> bool {
        self.level != Level::Ok
    }

    fn line(&self) -> String {
        let level = match self.level {
            Level::Ok => "ok",
            Level::Fail => "fail",
            Level::Warn => "warn",
        };
        let hint = self
            .hint
            .as_deref()
            .map(|hint| format!(" ({hint})"))
            .unwrap_or_default();
        scrub(
            &format!("{level} {}: {}{hint}", self.name, self.message),
            &[],
        )
    }
}

pub fn dropped() -> Vec<String> {
    serde_json::from_str(DROPPED).expect("dropped-checks.json is a list of names")
}

pub fn is_dropped(name: &str, dropped: &[String]) -> bool {
    dropped.iter().any(|entry| match entry.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == entry,
    })
}

pub fn parse(output: &str) -> Result<Vec<Check>, String> {
    serde_json::from_str(output).map_err(|error| error.to_string())
}

pub fn find_tool(tool: &str, path: &str) -> Result<PathBuf, String> {
    let mut found_unexecutable = None;
    for dir in path.split(':').filter(|dir| !dir.is_empty()) {
        let candidate = Path::new(dir).join(tool);
        let Ok(meta) = fs::metadata(&candidate) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        if meta.permissions().mode() & 0o111 != 0 {
            return Ok(candidate);
        }
        found_unexecutable.get_or_insert(candidate);
    }
    Err(match found_unexecutable {
        Some(file) => format!("{} is not executable", file.display()),
        None => format!("not found on the worker's PATH ({path})"),
    })
}

fn tool_checks(path: &str) -> Vec<Check> {
    TOOLS
        .iter()
        .map(|tool| {
            let name = format!("tool {tool}");
            match find_tool(tool, path) {
                Ok(found) => Check::new(&name, Level::Ok, found.display().to_string()),
                Err(reason) => Check::new(&name, Level::Fail, reason),
            }
        })
        .collect()
}

pub struct Report {
    pub lines: Vec<String>,
    pub problems: usize,
}

pub fn report(doctor: Vec<Check>, dropped: &[String], worker_path: &str) -> Report {
    let checks: Vec<Check> = doctor
        .into_iter()
        .filter(|check| !is_dropped(&check.name, dropped))
        .chain(tool_checks(worker_path))
        .collect();
    Report {
        problems: checks.iter().filter(|check| check.is_problem()).count(),
        lines: checks.iter().map(Check::line).collect(),
    }
}

async fn run_doctor(board: &Path, env: &BTreeMap<String, String>) -> Result<Vec<Check>, String> {
    let child = Command::new(board)
        .args(["doctor", "--json"])
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(DOCTOR_LIMIT, child)
        .await
        .map_err(|_| "board doctor did not finish within 60 s".to_owned())?
        .map_err(|error| format!("board doctor cannot be run: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse(&stdout).map_err(|error| {
        let seen = if stdout.trim().is_empty() {
            String::from_utf8_lossy(&output.stderr).into_owned()
        } else {
            stdout.into_owned()
        };
        let excerpt: String = seen.trim().chars().take(OUTPUT_EXCERPT).collect();
        format!("board doctor --json did not print a list of checks ({error}): {excerpt}")
    })
}

pub async fn run(board: &Path, env: &BTreeMap<String, String>, log: &Path) -> usize {
    let worker_path = env.get("PATH").map_or("", String::as_str);
    let Report {
        mut lines,
        problems,
    } = match run_doctor(board, env).await {
        Ok(checks) => report(checks, &dropped(), worker_path),
        Err(reason) => {
            let failure = Check::new("doctor", Level::Fail, reason);
            Report {
                lines: vec![failure.line()],
                problems: 1,
            }
        }
    };
    lines.insert(0, format!("worker check {}", timestamp()));
    if let Some(parent) = log.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(log, lines.join("\n") + "\n");
    problems
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::OpenOptionsExt;

    use super::*;

    fn check(name: &str, level: Level) -> Check {
        Check::new(name, level, "m")
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    fn file_with_mode(dir: &Path, name: &str, mode: u32) {
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(dir.join(name))
            .unwrap();
    }

    #[test]
    fn a_dropped_name_matches_exactly_or_by_prefix_and_an_unknown_name_is_kept() {
        let dropped = names(&["launchd agents", "tool *"]);

        assert!(is_dropped("launchd agents", &dropped));
        assert!(is_dropped("tool claude", &dropped));
        assert!(is_dropped("tool ", &dropped));
        assert!(!is_dropped("launchd agents x", &dropped));
        assert!(!is_dropped("tools", &dropped));
        assert!(!is_dropped("web.json", &dropped));
    }

    #[test]
    fn the_shipped_list_names_the_checks_that_do_not_apply_to_a_supervised_worker() {
        let dropped = dropped();

        assert!(is_dropped("launchd agents", &dropped));
        assert!(is_dropped(
            "launchd de.codeplant.agent-board.dispatch.plist",
            &dropped
        ));
        assert!(is_dropped("tool lockf", &dropped));
        assert!(!is_dropped("web.json", &dropped));
        assert!(!is_dropped("env", &dropped));
    }

    #[test]
    fn a_tool_is_found_in_the_first_directory_that_holds_it_executable() {
        let plain = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        file_with_mode(first.path(), "claude", 0o644);
        file_with_mode(second.path(), "claude", 0o755);
        let path = format!(
            "{}::/no/such:{}:{}",
            plain.path().display(),
            first.path().display(),
            second.path().display()
        );

        assert_eq!(find_tool("claude", &path), Ok(second.path().join("claude")));
    }

    #[test]
    fn a_missing_tool_and_one_that_is_present_but_not_executable_are_told_apart() {
        let dir = tempfile::tempdir().unwrap();
        file_with_mode(dir.path(), "gh", 0o644);
        std::fs::create_dir(dir.path().join("claude")).unwrap();
        let path = dir.path().display().to_string();

        assert!(find_tool("gh", &path)
            .unwrap_err()
            .ends_with("gh is not executable"));
        assert!(find_tool("claude", &path)
            .unwrap_err()
            .starts_with("not found"));
    }

    #[test]
    fn the_problem_count_is_the_filtered_results_that_are_not_ok_plus_the_missing_tools() {
        let dir = tempfile::tempdir().unwrap();
        file_with_mode(dir.path(), "claude", 0o755);
        let path = dir.path().display().to_string();
        let doctor = vec![
            check("web.json", Level::Fail),
            check("config.json", Level::Warn),
            check("env", Level::Ok),
            check("tool gh", Level::Fail),
            check("launchd agents", Level::Warn),
        ];

        let report = report(doctor, &names(&["tool *", "launchd agents"]), &path);

        assert_eq!(report.problems, 3);
        assert_eq!(
            report
                .lines
                .iter()
                .map(|line| line.split(':').next().unwrap())
                .collect::<Vec<_>>(),
            [
                "fail web.json",
                "warn config.json",
                "ok env",
                "ok tool claude",
                "fail tool gh"
            ]
        );
    }

    #[test]
    fn a_line_holding_a_token_shaped_word_is_written_as_withheld() {
        let leaking = Check {
            hint: Some("run with abm_0123456789abcdef".into()),
            ..check("env", Level::Fail)
        };

        let line = leaking.line();

        assert!(!line.contains("abm_0123"));
        assert!(line.contains("(withheld)"));
    }

    #[test]
    fn parsing_takes_the_core_shape_and_refuses_anything_else() {
        let parsed = parse(
            r#"[{"name":"env","level":"fail","message":"missing","hint":"run init"},{"name":"x","level":"ok","message":"y"}]"#,
        )
        .unwrap();

        assert_eq!(parsed[0].hint.as_deref(), Some("run init"));
        assert_eq!(parsed[1].level, Level::Ok);
        for text in [
            "",
            "not json",
            "{}",
            r#"[{"name":"x"}]"#,
            r#"[{"name":"x","level":"bad","message":""}]"#,
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
    }

    fn stub_board(dir: &Path, body: &str) -> PathBuf {
        let board = dir.join("board");
        fs::write(&board, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&board, fs::Permissions::from_mode(0o755)).unwrap();
        board
    }

    #[tokio::test]
    async fn the_log_holds_the_filtered_results_and_the_count_is_returned() {
        let dir = tempfile::tempdir().unwrap();
        let board = stub_board(
            dir.path(),
            r#"printf '[{"name":"web.json","level":"fail","message":"missing"},{"name":"tool claude","level":"ok","message":"/x/claude"}]'; exit 1"#,
        );
        let log = dir.path().join("logs/worker-check.log");
        let env = BTreeMap::from([("PATH".to_owned(), "/no/such".to_owned())]);

        let problems = run(&board, &env, &log).await;

        let written = fs::read_to_string(&log).unwrap();
        assert_eq!(problems, 3);
        assert!(written.contains("fail web.json: missing"));
        assert!(!written.contains("/x/claude"));
        assert!(written.contains("fail tool claude: not found"));
        assert!(written.contains("fail tool gh: not found"));
    }

    #[tokio::test]
    async fn output_that_is_not_json_is_one_problem() {
        let dir = tempfile::tempdir().unwrap();
        let board = stub_board(dir.path(), "echo 'board: unknown command'");
        let log = dir.path().join("worker-check.log");

        let problems = run(&board, &BTreeMap::new(), &log).await;

        assert_eq!(problems, 1);
        let written = fs::read_to_string(&log).unwrap();
        assert!(written.contains("fail doctor: board doctor --json did not print a list of checks"));
        assert!(written.contains("unknown command"));
    }

    #[tokio::test]
    async fn a_board_that_cannot_be_run_is_one_problem() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("worker-check.log");

        let problems = run(&dir.path().join("missing"), &BTreeMap::new(), &log).await;

        assert_eq!(problems, 1);
        assert!(fs::read_to_string(&log)
            .unwrap()
            .contains("board doctor cannot be run"));
    }
}
