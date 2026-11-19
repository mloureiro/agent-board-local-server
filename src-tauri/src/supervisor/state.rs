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
    Stopped,
}

impl Status {
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
