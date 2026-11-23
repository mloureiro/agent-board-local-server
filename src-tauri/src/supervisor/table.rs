use std::collections::BTreeMap;

use super::state::{Status, StatusBoard};

pub const BACKEND: &str = "backend";
pub const PROXY: &str = "proxy";
pub const WORKER: &str = "worker";
pub const WORKER_LISTEN: &str = "worker-listen";
pub const WORKER_WATCH: &str = "worker-watch";
pub const DISPATCH: &str = "dispatch";

pub struct Row {
    pub name: &'static str,
    pub title: &'static str,
    pub initial: &'static str,
}

pub const ROWS: [Row; 6] = [
    Row {
        name: BACKEND,
        title: "Backend",
        initial: "Backend: starting",
    },
    Row {
        name: PROXY,
        title: "Proxy",
        initial: "Proxy: waiting",
    },
    Row {
        name: WORKER,
        title: "Worker",
        initial: "Worker: not connected",
    },
    Row {
        name: WORKER_LISTEN,
        title: "Worker listener",
        initial: "Worker listener: waiting",
    },
    Row {
        name: WORKER_WATCH,
        title: "Worker watch",
        initial: "Worker watch: waiting",
    },
    Row {
        name: DISPATCH,
        title: "Last dispatch tick",
        initial: "Last dispatch tick: none yet",
    },
];

pub fn changed_labels(
    board: &StatusBoard,
    seen: &mut BTreeMap<&'static str, String>,
) -> Vec<String> {
    let mut changed = Vec::new();
    for row in &ROWS {
        let Some(status) = board.get(row.name) else {
            continue;
        };
        let line = match status {
            Status::Running { port } => format!("{} running on 127.0.0.1:{port}", row.title),
            other => other.label(row.title),
        };
        if seen.insert(row.name, line.clone()).as_deref() != Some(line.as_str()) {
            changed.push(line);
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_reported_once_and_again_only_when_it_changes() {
        let board = StatusBoard::default();
        let mut seen = BTreeMap::new();
        board.set(BACKEND, Status::Running { port: 5000 });
        board.set(WORKER, Status::Note("ok".into()));

        assert_eq!(
            changed_labels(&board, &mut seen),
            ["Backend running on 127.0.0.1:5000", "Worker: ok"]
        );
        assert!(changed_labels(&board, &mut seen).is_empty());

        board.set(WORKER, Status::Note("ok".into()));
        board.set(PROXY, Status::Starting);
        assert_eq!(changed_labels(&board, &mut seen), ["Proxy: starting"]);
    }
}
