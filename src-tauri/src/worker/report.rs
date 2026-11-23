use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::enrol::scrub;
use super::processes::TickResult;
use crate::supervisor::{Status, WorkerFailure};

const TAIL_BYTES: u64 = 8 * 1024;
const REASON_LIMIT: usize = 160;
const PREFIXES: [&str; 2] = ["agent-board:", "board:"];
const REFUSAL_WORDS: [&str; 7] = [
    "refused",
    "403",
    "401",
    "forbidden",
    "unauthorized",
    "revoked",
    "belongs",
];

fn read_tail(path: &Path) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let length = file.metadata().map_or(0, |meta| meta.len());
    let _ = file.seek(SeekFrom::Start(length.saturating_sub(TAIL_BYTES)));
    let mut bytes = Vec::new();
    let _ = file.read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A failed `internal dispatch` ends with the CLI's own message, `agent-board: <reason>`.
fn failure_reason(log: &str) -> Option<String> {
    let lines = || log.lines().map(str::trim).filter(|line| !line.is_empty());
    let line = lines()
        .rev()
        .find(|line| PREFIXES.iter().any(|prefix| line.starts_with(prefix)))
        .or_else(|| lines().next_back())?;
    let reason = PREFIXES
        .iter()
        .find_map(|prefix| line.strip_prefix(prefix))
        .unwrap_or(line)
        .trim();
    Some(scrub(reason, &[]).chars().take(REASON_LIMIT).collect())
}

fn is_refusal(reason: &str) -> bool {
    let lower = reason.to_lowercase();
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| REFUSAL_WORDS.contains(&word))
}

pub fn worker_note(result: &TickResult, log: &Path) -> Option<Status> {
    let failing = |kind, reason: &str| {
        Some(Status::WorkerFailing {
            kind,
            reason: reason.into(),
        })
    };
    match result {
        TickResult::Ok => Some(Status::Note("ok".into())),
        TickResult::Failed(_) => {
            let reason = failure_reason(&read_tail(log))
                .unwrap_or_else(|| "the dispatch tick failed".into());
            let kind = if is_refusal(&reason) {
                WorkerFailure::Refused
            } else {
                WorkerFailure::Error
            };
            failing(kind, &reason)
        }
        TickResult::TimedOut => failing(
            WorkerFailure::Error,
            "the dispatch tick did not finish in time",
        ),
        TickResult::LockHeld | TickResult::Killed(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(result: &TickResult, log: &str) -> Option<Status> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dispatch.log");
        std::fs::write(&path, log).unwrap();
        worker_note(result, &path)
    }

    fn failing(kind: WorkerFailure, reason: &str) -> Option<Status> {
        Some(Status::WorkerFailing {
            kind,
            reason: reason.into(),
        })
    }

    #[test]
    fn a_claim_refused_by_the_board_is_shown_with_its_reason() {
        let log =
            "STARTED t1 prompt lane=main\nagent-board: this token belongs to tnk_one, not mac\n";

        assert_eq!(
            note(&TickResult::Failed(1), log),
            failing(
                WorkerFailure::Refused,
                "this token belongs to tnk_one, not mac"
            )
        );
    }

    #[test]
    fn another_failure_is_an_error_and_the_cli_prefix_is_dropped() {
        let log = "agent-board: fetch failed: connect ECONNREFUSED 127.0.0.1:4785\n";

        assert_eq!(
            note(&TickResult::Failed(1), log),
            failing(
                WorkerFailure::Error,
                "fetch failed: connect ECONNREFUSED 127.0.0.1:4785"
            )
        );
    }

    #[test]
    fn the_cli_message_wins_over_a_later_line_without_the_prefix() {
        let log = "agent-board: the token was refused (401)\n    at something\n";

        assert_eq!(
            note(&TickResult::Failed(1), log),
            failing(WorkerFailure::Refused, "the token was refused (401)")
        );
    }

    #[test]
    fn a_failed_tick_with_an_empty_log_still_says_so() {
        assert_eq!(
            note(&TickResult::Failed(1), ""),
            failing(WorkerFailure::Error, "the dispatch tick failed")
        );
    }

    #[test]
    fn a_good_tick_clears_the_error_and_a_tick_that_told_nothing_leaves_it() {
        assert_eq!(note(&TickResult::Ok, ""), Some(Status::Note("ok".into())));
        assert_eq!(note(&TickResult::LockHeld, "agent-board: x"), None);
        assert_eq!(note(&TickResult::Killed(9), "agent-board: x"), None);
        assert_eq!(
            note(&TickResult::TimedOut, ""),
            failing(
                WorkerFailure::Error,
                "the dispatch tick did not finish in time"
            )
        );
    }

    #[test]
    fn a_secret_in_a_log_line_is_withheld() {
        let log = "agent-board: refused abm_abcdefghijklmnop\n";

        let Some(Status::WorkerFailing { reason, .. }) = note(&TickResult::Failed(1), log) else {
            panic!("expected a failing worker");
        };

        assert!(!reason.contains("abcdefgh"));
    }

    #[test]
    fn only_the_end_of_a_long_log_is_read() {
        let mut log = "x".repeat(20_000);
        log.push_str("\nagent-board: 403 forbidden\n");

        assert_eq!(
            note(&TickResult::Failed(1), &log),
            failing(WorkerFailure::Refused, "403 forbidden")
        );
    }
}
