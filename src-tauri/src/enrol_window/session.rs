use std::future::Future;
use std::sync::Mutex;

use serde::Serialize;

use crate::worker::control::Enrolled;
use crate::worker::enrol::EnrolError;

pub trait Enroller: Send + Sync {
    fn enrol(
        &self,
        token: &str,
        data_key: &str,
    ) -> impl Future<Output = Result<Enrolled, EnrolError>> + Send;
}

/// What the window shows; it reaches the page as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum Phase {
    Form,
    Connecting,
    Connected { machine: Option<String> },
    Failed { reason: String },
}

pub struct Session<E> {
    enroller: E,
    phase: Mutex<Phase>,
}

impl<E: Enroller> Session<E> {
    pub fn new(enroller: E) -> Self {
        Self {
            enroller,
            phase: Mutex::new(Phase::Form),
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase.lock().expect("phase lock").clone()
    }

    fn set(&self, phase: Phase) -> Phase {
        *self.phase.lock().expect("phase lock") = phase.clone();
        phase
    }

    /// A second submit while one is running, or after a success, changes nothing.
    pub async fn submit(&self, token: &str, data_key: &str) -> Phase {
        {
            let mut phase = self.phase.lock().expect("phase lock");
            if matches!(*phase, Phase::Connecting | Phase::Connected { .. }) {
                return phase.clone();
            }
            *phase = Phase::Connecting;
        }
        for (name, value) in [("machine token", token), ("data key", data_key)] {
            if value.trim().is_empty() {
                return self.set(Phase::Failed {
                    reason: format!("the {name} is empty"),
                });
            }
        }
        match self.enroller.enrol(token, data_key).await {
            Ok(Enrolled { machine }) => self.set(Phase::Connected { machine }),
            Err(error) => self.set(Phase::Failed {
                reason: error.to_string(),
            }),
        }
    }

    /// Closing is refused while the board is being asked: stopping then would leave the worker half enrolled.
    pub fn can_close(&self) -> bool {
        self.phase() != Phase::Connecting
    }

    pub fn reset(&self) {
        self.set(Phase::Form);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use tokio::sync::Notify;

    use super::*;

    struct Scripted {
        outcomes: StdMutex<Vec<Result<Enrolled, EnrolError>>>,
        calls: StdMutex<Vec<(String, String)>>,
    }

    impl Enroller for Scripted {
        async fn enrol(&self, token: &str, data_key: &str) -> Result<Enrolled, EnrolError> {
            self.calls
                .lock()
                .unwrap()
                .push((token.to_owned(), data_key.to_owned()));
            self.outcomes.lock().unwrap().remove(0)
        }
    }

    fn scripted(outcomes: Vec<Result<Enrolled, EnrolError>>) -> Session<Scripted> {
        Session::new(Scripted {
            outcomes: StdMutex::new(outcomes),
            calls: StdMutex::default(),
        })
    }

    fn named(machine: &str) -> Result<Enrolled, EnrolError> {
        Ok(Enrolled {
            machine: Some(machine.into()),
        })
    }

    fn refused(message: &str) -> Result<Enrolled, EnrolError> {
        Err(EnrolError::new(message))
    }

    #[tokio::test]
    async fn a_token_for_another_name_connects_under_the_name_the_board_gives() {
        let session = scripted(vec![named("tnk_one")]);

        let phase = session.submit("abm_x", "abk1_y").await;

        assert_eq!(
            phase,
            Phase::Connected {
                machine: Some("tnk_one".into())
            }
        );
        assert_eq!(session.phase(), phase);
    }

    #[tokio::test]
    async fn each_refusal_stays_on_the_form_with_the_reason_and_a_retry_connects() {
        let reasons = [
            "http://127.0.0.1:4785 refused the token: create a machine token on the board's Settings page and give that one; nothing was written",
            "this machine holds a different data key (id a) than the board's (id b); nothing was written",
            "cannot check the token with http://127.0.0.1:4785: fetch failed; nothing was written",
        ];
        let mut outcomes: Vec<_> = reasons.iter().map(|reason| refused(reason)).collect();
        outcomes.push(named("tnk_one"));
        let session = scripted(outcomes);

        for reason in reasons {
            assert_eq!(
                session.submit("abm_x", "abk1_y").await,
                Phase::Failed {
                    reason: reason.into()
                }
            );
            assert!(session.can_close());
        }
        assert_eq!(
            session.submit("abm_x", "abk1_y").await,
            Phase::Connected {
                machine: Some("tnk_one".into())
            }
        );
    }

    #[tokio::test]
    async fn an_empty_field_is_refused_without_asking_the_board() {
        let session = scripted(vec![]);

        assert_eq!(
            session.submit("  ", "abk1_y").await,
            Phase::Failed {
                reason: "the machine token is empty".into()
            }
        );
        assert_eq!(
            session.submit("abm_x", "").await,
            Phase::Failed {
                reason: "the data key is empty".into()
            }
        );
        assert!(session.enroller.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancelling_before_submitting_stores_and_asks_nothing() {
        let session = scripted(vec![]);

        assert_eq!(session.phase(), Phase::Form);
        assert!(session.can_close());
        assert!(session.enroller.calls.lock().unwrap().is_empty());
    }

    struct Gate(Notify, Notify);

    impl Enroller for Gate {
        async fn enrol(&self, _token: &str, _data_key: &str) -> Result<Enrolled, EnrolError> {
            self.0.notify_one();
            self.1.notified().await;
            Ok(Enrolled { machine: None })
        }
    }

    #[tokio::test]
    async fn while_connecting_the_window_cannot_be_closed_and_a_second_submit_does_nothing() {
        let session = std::sync::Arc::new(Session::new(Gate(Notify::new(), Notify::new())));
        let running = tokio::spawn({
            let session = session.clone();
            async move { session.submit("abm_x", "abk1_y").await }
        });
        session.enroller.0.notified().await;

        assert_eq!(session.phase(), Phase::Connecting);
        assert!(!session.can_close());
        assert_eq!(
            session.submit("abm_other", "abk1_y").await,
            Phase::Connecting
        );

        session.enroller.1.notify_one();
        assert_eq!(running.await.unwrap(), Phase::Connected { machine: None });
        assert!(session.can_close());
    }

    #[tokio::test]
    async fn a_connected_session_ignores_further_submits_until_reset() {
        let session = scripted(vec![named("tnk_one"), named("other")]);
        session.submit("abm_x", "abk1_y").await;

        let again = session.submit("abm_z", "abk1_y").await;
        assert_eq!(
            again,
            Phase::Connected {
                machine: Some("tnk_one".into())
            }
        );

        session.reset();
        assert_eq!(session.phase(), Phase::Form);
    }
}
