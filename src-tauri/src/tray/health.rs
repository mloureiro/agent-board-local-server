use tauri::tray::TrayIcon;
use tauri::Wry;

use crate::supervisor::table::ROWS;
use crate::supervisor::StatusBoard;

const IDLE_TOOLTIP: &str = "agent-board local server";
const FAILING_MARK: &str = "!";

pub fn first_failing(board: &StatusBoard) -> Option<String> {
    ROWS.iter().find_map(|row| {
        board
            .get(row.name)
            .filter(|status| status.is_failing())
            .map(|status| status.label(row.title))
    })
}

pub fn show(icon: &TrayIcon<Wry>, failing: Option<&str>) {
    let _ = icon.set_title(failing.map(|_| FAILING_MARK));
    let _ = icon.set_tooltip(Some(failing.unwrap_or(IDLE_TOOLTIP)));
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::supervisor::table::{BACKEND, DISPATCH, PROXY, WORKER};
    use crate::supervisor::{Status, WorkerFailure};

    fn blocked(reason: &str) -> Status {
        Status::Blocked {
            reason: reason.into(),
        }
    }

    #[test]
    fn nothing_is_reported_while_every_row_is_well_or_restarting() {
        let board = StatusBoard::default();
        board.set(BACKEND, Status::Running { port: 1 });
        board.set(
            PROXY,
            Status::Crashed {
                reason: "exit 1".into(),
                restart_in: Some(Duration::from_secs(2)),
            },
        );

        assert_eq!(first_failing(&board), None);
    }

    #[test]
    fn the_earlier_row_of_the_table_wins_whatever_order_they_failed_in() {
        let board = StatusBoard::default();
        board.set(
            WORKER,
            Status::WorkerFailing {
                kind: WorkerFailure::Refused,
                reason: "403".into(),
            },
        );
        board.set(BACKEND, blocked("no port"));

        assert_eq!(
            first_failing(&board).as_deref(),
            Some("Backend: not started (no port)")
        );
    }

    #[test]
    fn a_recovered_row_is_no_longer_reported() {
        let board = StatusBoard::default();
        board.set(DISPATCH, Status::Note("ok".into()));
        board.set(
            WORKER,
            Status::WorkerFailing {
                kind: WorkerFailure::Error,
                reason: "timed out".into(),
            },
        );
        assert_eq!(
            first_failing(&board).as_deref(),
            Some("Worker: error — timed out")
        );

        board.set(WORKER, Status::Note("ok".into()));

        assert_eq!(first_failing(&board), None);
    }
}
