use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;
use tokio::time::timeout;

use crate::applog::LogFile;
use crate::supervisor::table::{DISPATCH, WORKER_LISTEN, WORKER_WATCH};

const KNOWN: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../core.worker-units.json"
));
const READ_LIMIT: Duration = Duration::from_secs(30);
const EXCERPT: usize = 120;
const READ_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

#[derive(Deserialize)]
struct Known {
    schemas: Vec<u32>,
    units: Vec<String>,
}

fn known() -> Known {
    serde_json::from_str(KNOWN).expect("core.worker-units.json lists the schemas and the units")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickUnit {
    pub row: &'static str,
    pub program: String,
    pub args: Vec<String>,
    pub every: Duration,
    pub limit: Duration,
    pub grace: Duration,
    pub lock_held: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceUnit {
    pub row: &'static str,
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerUnits {
    pub path: String,
    pub dispatch: TickUnit,
    pub listen: ServiceUnit,
    pub watch: ServiceUnit,
}

pub fn row(name: &str) -> Option<&'static str> {
    ROWS.iter()
        .find(|(unit, _)| *unit == name)
        .map(|(_, row)| *row)
}

const ROWS: [(&str, &str); 3] = [
    ("dispatch", DISPATCH),
    ("listen", WORKER_LISTEN),
    ("watch", WORKER_WATCH),
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTick {
    program: String,
    args: Vec<String>,
    every_seconds: u64,
    limit_seconds: u64,
    grace_seconds: u64,
    lock_held_exit_code: i32,
}

#[derive(Deserialize)]
struct RawService {
    program: String,
    args: Vec<String>,
}

fn excerpt(text: &str) -> String {
    text.trim().chars().take(EXCERPT).collect()
}

fn unit_of<'a>(units: &'a [Value], name: &str) -> Result<&'a Value, String> {
    units
        .iter()
        .find(|unit| unit.get("name").and_then(Value::as_str) == Some(name))
        .ok_or_else(|| format!("the unit {name} is missing"))
}

fn kind_of<'a>(unit: &'a Value, name: &str, kind: &str) -> Result<&'a Value, String> {
    match unit.get("kind").and_then(Value::as_str) {
        Some(found) if found == kind => Ok(unit),
        Some(found) => Err(format!("the unit {name} is a {found}, not a {kind}")),
        None => Err(format!("the unit {name} has no kind")),
    }
}

fn fields<T: for<'de> Deserialize<'de>>(unit: &Value, name: &str) -> Result<T, String> {
    serde_json::from_value(unit.clone()).map_err(|error| format!("the unit {name}: {error}"))
}

fn service(units: &[Value], name: &str) -> Result<ServiceUnit, String> {
    let unit = kind_of(unit_of(units, name)?, name, "service")?;
    let RawService { program, args } = fields(unit, name)?;
    Ok(ServiceUnit {
        row: row(name).ok_or_else(|| format!("the unit {name} is not known"))?,
        program,
        args,
    })
}

fn tick(units: &[Value], name: &str) -> Result<TickUnit, String> {
    let unit = kind_of(unit_of(units, name)?, name, "tick")?;
    let raw: RawTick = fields(unit, name)?;
    Ok(TickUnit {
        row: row(name).ok_or_else(|| format!("the unit {name} is not known"))?,
        program: raw.program,
        args: raw.args,
        every: Duration::from_secs(raw.every_seconds),
        limit: Duration::from_secs(raw.limit_seconds),
        grace: Duration::from_secs(raw.grace_seconds),
        lock_held: raw.lock_held_exit_code,
    })
}

pub fn parse(stdout: &str) -> Result<WorkerUnits, String> {
    let known = known();
    let output: Value = serde_json::from_str(stdout).map_err(|error| {
        format!(
            "board internal units did not print JSON ({error}): {}",
            excerpt(stdout)
        )
    })?;
    let schema = output.get("schema").and_then(Value::as_u64);
    if !schema.is_some_and(|schema| {
        known
            .schemas
            .iter()
            .any(|known| u64::from(*known) == schema)
    }) {
        let seen = output
            .get("schema")
            .map_or("none".to_owned(), Value::to_string);
        let wanted: Vec<String> = known.schemas.iter().map(u32::to_string).collect();
        return Err(format!(
            "board internal units printed schema version {seen}; this app knows version {}",
            wanted.join(", ")
        ));
    }
    let path = output
        .get("path")
        .and_then(Value::as_str)
        .ok_or("board internal units printed no path")?
        .to_owned();
    let units = output
        .get("units")
        .and_then(Value::as_array)
        .ok_or("board internal units printed no units list")?;
    for unit in units {
        match unit.get("name") {
            Some(Value::String(name)) if known.units.contains(name) => {}
            Some(Value::String(name)) => {
                return Err(format!(
                    "board internal units printed the worker unit {name:?}, which this app does not know"
                ))
            }
            Some(other) => {
                return Err(format!(
                    "board internal units printed a unit whose name is not a string ({other})"
                ))
            }
            None => return Err("board internal units printed a unit without a name".into()),
        }
    }
    Ok(WorkerUnits {
        path,
        dispatch: tick(units, "dispatch")?,
        listen: service(units, "listen")?,
        watch: service(units, "watch")?,
    })
}

fn copy_to_log(log: &LogFile, stdout: &str, stderr: &str) {
    for (label, text) in [("stdout", stdout), ("stderr", stderr)] {
        if !text.trim().is_empty() {
            log.write(&format!("board internal units {label}:\n{}", text.trim()));
        }
    }
}

pub async fn read(
    board: &Path,
    base_env: &BTreeMap<String, String>,
    log: &LogFile,
) -> Result<WorkerUnits, String> {
    let run = Command::new(board)
        .args(["internal", "units"])
        .env_clear()
        .envs(base_env)
        .env("PATH", READ_PATH)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let output = match timeout(READ_LIMIT, run).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            let reason = format!("board internal units could not run: {error}");
            log.write(&reason);
            return Err(reason);
        }
        Err(_) => {
            let reason = "board internal units did not finish in time".to_owned();
            log.write(&reason);
            return Err(reason);
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let result = if output.status.success() {
        parse(&stdout)
    } else {
        let seen = if stderr.trim().is_empty() {
            &stdout
        } else {
            &stderr
        };
        Err(format!(
            "board internal units failed ({}): {}",
            output.status,
            excerpt(seen)
        ))
    };
    if result.is_err() {
        copy_to_log(log, &stdout, &stderr);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output() -> Value {
        serde_json::json!({
            "schema": 1,
            "platform": "darwin",
            "path": "/Users/me/.local/bin:/opt/homebrew/bin:/usr/bin:/bin",
            "passedVariables": ["AGENT_BOARD_CONFIG_DIR"],
            "units": [
                {
                    "name": "dispatch",
                    "kind": "tick",
                    "program": "/usr/bin/lockf",
                    "args": ["-s", "-t", "0", "/run/agent-board-dispatch.lock", "/cli/board", "internal", "dispatch"],
                    "everySeconds": 120,
                    "limitSeconds": 110,
                    "graceSeconds": 10,
                    "lockHeldExitCode": 75
                },
                { "name": "listen", "kind": "service", "program": "/cli/board", "args": ["internal", "listen"], "restartDelaySeconds": 10, "stopTimeoutSeconds": 130 },
                { "name": "watch", "kind": "service", "program": "/cli/board", "args": ["internal", "watch"], "restartDelaySeconds": 5 }
            ]
        })
    }

    fn without_unit(name: &str) -> Value {
        let mut value = output();
        value["units"]
            .as_array_mut()
            .unwrap()
            .retain(|unit| unit["name"] != name);
        value
    }

    fn with(edit: impl FnOnce(&mut Value)) -> Value {
        let mut value = output();
        edit(&mut value);
        value
    }

    fn parsed(value: &Value) -> Result<WorkerUnits, String> {
        parse(&value.to_string())
    }

    #[test]
    fn a_schema_one_output_gives_the_three_units() {
        let units = parsed(&output()).unwrap();

        assert_eq!(
            units.path,
            "/Users/me/.local/bin:/opt/homebrew/bin:/usr/bin:/bin"
        );
        assert_eq!(
            units.dispatch,
            TickUnit {
                row: DISPATCH,
                program: "/usr/bin/lockf".into(),
                args: [
                    "-s",
                    "-t",
                    "0",
                    "/run/agent-board-dispatch.lock",
                    "/cli/board",
                    "internal",
                    "dispatch"
                ]
                .map(String::from)
                .to_vec(),
                every: Duration::from_secs(120),
                limit: Duration::from_secs(110),
                grace: Duration::from_secs(10),
                lock_held: 75,
            }
        );
        assert_eq!(
            units.listen,
            ServiceUnit {
                row: WORKER_LISTEN,
                program: "/cli/board".into(),
                args: vec!["internal".into(), "listen".into()],
            }
        );
        assert_eq!(units.watch.row, WORKER_WATCH);
        assert_eq!(units.watch.args, ["internal", "watch"]);
    }

    #[test]
    fn an_output_the_app_cannot_run_is_refused_with_the_reason_named() {
        let table = [
            (with(|value| value["schema"] = 2.into()), "schema version 2"),
            (
                with(|value| value["schema"] = Value::Null),
                "schema version null",
            ),
            (
                with(|value| {
                    value["units"]
                        .as_array_mut()
                        .unwrap()
                        .push(serde_json::json!({
                            "name": "reap", "kind": "tick"
                        }))
                }),
                "\"reap\"",
            ),
            (without_unit("watch"), "unit watch is missing"),
            (
                with(|value| value["units"][1]["kind"] = "tick".into()),
                "unit listen is a tick, not a service",
            ),
            (
                with(|value| value["units"][0]["kind"] = "service".into()),
                "unit dispatch is a service, not a tick",
            ),
            (
                with(|value| {
                    value["units"][2].as_object_mut().unwrap().remove("name");
                }),
                "a unit without a name",
            ),
            (
                with(|value| value["units"][2]["name"] = 7.into()),
                "name is not a string",
            ),
            (
                with(|value| {
                    value["units"][0]
                        .as_object_mut()
                        .unwrap()
                        .remove("everySeconds");
                }),
                "the unit dispatch",
            ),
            (
                with(|value| {
                    value.as_object_mut().unwrap().remove("path");
                }),
                "no path",
            ),
        ];
        for (value, expected) in table {
            let error = parsed(&value).unwrap_err();
            assert!(error.contains(expected), "{expected} not in {error}");
        }
    }

    #[test]
    fn output_that_is_not_json_is_refused_with_an_excerpt() {
        let error = parse("Error: cannot find module 'main.mts'").unwrap_err();

        assert!(error.contains("did not print JSON"), "{error}");
        assert!(error.contains("cannot find module"), "{error}");
    }

    #[test]
    fn the_tray_rows_cover_exactly_the_units_the_shared_file_lists() {
        let mut rowed: Vec<&str> = ROWS.iter().map(|(name, _)| *name).collect();
        let known = known();
        let mut listed: Vec<&str> = known.units.iter().map(String::as_str).collect();
        rowed.sort_unstable();
        listed.sort_unstable();

        assert_eq!(rowed, listed);
    }
    fn script(dir: &Path, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("board");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[tokio::test]
    async fn the_command_runs_with_the_base_environment_and_the_system_path_only() {
        let dir = tempfile::tempdir().unwrap();
        let log = LogFile::new(dir.path().join("app.log"));
        let printed = output().to_string().replace('\'', "");
        let board = script(
            dir.path(),
            &format!(
                "[ \"$1 $2\" = \"internal units\" ] && [ \"$PATH\" = {READ_PATH} ] && [ \"$KEPT\" = yes ] && [ -z \"$HOME\" ] || exit 9\necho '{printed}'"
            ),
        );
        let env = BTreeMap::from([("KEPT".to_owned(), "yes".to_owned())]);

        let units = read(&board, &env, &log).await.unwrap();

        assert_eq!(units.listen.args, ["internal", "listen"]);
    }

    #[tokio::test]
    async fn a_failing_command_is_one_reason_and_its_output_goes_to_the_app_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = LogFile::new(dir.path().join("app.log"));
        let board = script(dir.path(), "echo out-text\necho err-text >&2\nexit 1");

        let reason = read(&board, &BTreeMap::new(), &log).await.unwrap_err();

        assert!(reason.contains("err-text"), "{reason}");
        let logged = std::fs::read_to_string(log.path()).unwrap();
        assert!(
            logged.contains("out-text") && logged.contains("err-text"),
            "{logged}"
        );
    }
}
