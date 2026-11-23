use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::watch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Starting,
    Running {
        port: u16,
    },
    Crashed {
        reason: String,
        restart_in: Option<Duration>,
    },
    Blocked {
        reason: String,
    },
    NotConnected,
    Note(String),
    WorkerFailing {
        kind: WorkerFailure,
        reason: String,
    },
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerFailure {
    Refused,
    Error,
}

impl WorkerFailure {
    fn word(self) -> &'static str {
        match self {
            Self::Refused => "refused",
            Self::Error => "error",
        }
    }
}

impl Status {
    pub fn is_failing(&self) -> bool {
        matches!(
            self,
            Self::Blocked { .. }
                | Self::Crashed {
                    restart_in: None,
                    ..
                }
                | Self::WorkerFailing { .. }
        )
    }

    pub fn label(&self, title: &str) -> String {
        match self {
            Self::Starting => format!("{title}: starting"),
            Self::Running { .. } => format!("{title}: running"),
            Self::Crashed {
                reason,
                restart_in: Some(after),
            } => {
                format!(
                    "{title}: crashed ({reason}), restarting in {} s",
                    after.as_secs()
                )
            }
            Self::Crashed {
                reason,
                restart_in: None,
            } => format!("{title}: crashed ({reason})"),
            Self::Blocked { reason } => format!("{title}: not started ({reason})"),
            Self::NotConnected => "Worker not connected".into(),
            Self::Note(text) => format!("{title}: {text}"),
            Self::WorkerFailing { kind, reason } => format!("{title}: {} — {reason}", kind.word()),
            Self::Stopped => format!("{title}: stopped"),
        }
    }
}

/// What the tray reads. Every change bumps a counter so a reader can wait for the next one.
pub struct StatusBoard {
    statuses: Mutex<BTreeMap<&'static str, Status>>,
    changed: watch::Sender<u64>,
}

impl Default for StatusBoard {
    fn default() -> Self {
        Self {
            statuses: Mutex::default(),
            changed: watch::channel(0).0,
        }
    }
}

impl StatusBoard {
    pub fn set(&self, name: &'static str, status: Status) {
        self.statuses
            .lock()
            .expect("status lock")
            .insert(name, status);
        self.changed.send_modify(|version| *version += 1);
    }

    pub fn get(&self, name: &str) -> Option<Status> {
        self.statuses
            .lock()
            .expect("status lock")
            .get(name)
            .cloned()
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crashed(restart_in: Option<Duration>) -> Status {
        Status::Crashed {
            reason: "exit 1".into(),
            restart_in,
        }
    }

    #[test]
    fn only_a_row_that_will_not_recover_by_itself_is_failing() {
        let failing = [
            Status::Blocked { reason: "x".into() },
            crashed(None),
            Status::WorkerFailing {
                kind: WorkerFailure::Refused,
                reason: "x".into(),
            },
            Status::WorkerFailing {
                kind: WorkerFailure::Error,
                reason: "x".into(),
            },
        ];
        let healthy = [
            Status::Starting,
            Status::Running { port: 1 },
            crashed(Some(Duration::from_secs(2))),
            Status::NotConnected,
            Status::Note("ok".into()),
            Status::Stopped,
        ];

        assert!(failing.iter().all(Status::is_failing));
        assert!(!healthy.iter().any(Status::is_failing));
    }

    #[test]
    fn a_worker_failure_reads_with_its_kind_and_reason() {
        let refused = Status::WorkerFailing {
            kind: WorkerFailure::Refused,
            reason: "403 forbidden".into(),
        };
        let error = Status::WorkerFailing {
            kind: WorkerFailure::Error,
            reason: "timed out".into(),
        };

        assert_eq!(refused.label("Worker"), "Worker: refused — 403 forbidden");
        assert_eq!(error.label("Worker"), "Worker: error — timed out");
    }
}
