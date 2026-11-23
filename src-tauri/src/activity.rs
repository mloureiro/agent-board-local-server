use std::fmt;
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Enrolment,
    Backup,
    Recovery,
    Quit,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Enrolment => "a worker enrolment",
            Self::Backup => "a backup",
            Self::Recovery => "another recovery action",
            Self::Quit => "Quit",
        })
    }
}

pub struct Activity {
    current: Mutex<Option<Kind>>,
    changes: watch::Sender<Option<Kind>>,
}

impl Default for Activity {
    fn default() -> Self {
        Self {
            current: Mutex::new(None),
            changes: watch::channel(None).0,
        }
    }
}

pub struct Guard {
    activity: Arc<Activity>,
    kind: Kind,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let mut current = self.activity.current.lock().expect("activity lock");
        if *current == Some(self.kind) {
            *current = None;
            self.activity.changes.send_replace(None);
        }
    }
}

impl Activity {
    pub fn begin(self: &Arc<Self>, kind: Kind) -> Result<Guard, Kind> {
        let mut current = self.current.lock().expect("activity lock");
        if let Some(running) = *current {
            return Err(running);
        }
        *current = Some(kind);
        self.changes.send_replace(Some(kind));
        Ok(Guard {
            activity: self.clone(),
            kind,
        })
    }

    pub fn quit(&self) {
        *self.current.lock().expect("activity lock") = Some(Kind::Quit);
        self.changes.send_replace(Some(Kind::Quit));
    }

    pub fn subscribe(&self) -> watch::Receiver<Option<Kind>> {
        self.changes.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_begin_names_what_is_running_and_dropping_the_guard_frees_the_gate() {
        let activity = Arc::new(Activity::default());

        let guard = activity.begin(Kind::Enrolment).unwrap();
        assert_eq!(activity.begin(Kind::Recovery).err(), Some(Kind::Enrolment));
        assert_eq!(activity.begin(Kind::Backup).err(), Some(Kind::Enrolment));

        drop(guard);
        assert!(activity.begin(Kind::Recovery).is_ok());
    }

    #[test]
    fn after_quit_every_begin_is_refused_even_when_a_guard_is_dropped() {
        let activity = Arc::new(Activity::default());
        let guard = activity.begin(Kind::Backup).unwrap();

        activity.quit();
        drop(guard);

        for kind in [Kind::Enrolment, Kind::Backup, Kind::Recovery, Kind::Quit] {
            assert_eq!(activity.begin(kind).err(), Some(Kind::Quit));
        }
    }

    #[test]
    fn subscribers_see_the_gate_taken_and_freed() {
        let activity = Arc::new(Activity::default());
        let seen = activity.subscribe();

        let guard = activity.begin(Kind::Recovery).unwrap();
        assert_eq!(*seen.borrow(), Some(Kind::Recovery));

        drop(guard);
        assert_eq!(*seen.borrow(), None);
    }
}
