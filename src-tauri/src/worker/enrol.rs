use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

use super::credentials::{discard_pending, promote_pending, write_pending, Credentials};
use super::machine;
use crate::secrets::{SecretError, SecretStore};

const INIT_LIMIT: Duration = Duration::from_secs(60);
const MESSAGE_LIMIT: usize = 400;

#[derive(Debug)]
pub struct EnrolError(String);

impl fmt::Display for EnrolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EnrolError {}

impl From<SecretError> for EnrolError {
    fn from(error: SecretError) -> Self {
        Self(error.to_string())
    }
}

impl EnrolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// What an accepted enrolment leaves behind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitReport {
    /// The machine name in the worker's config.json, which is the one the board gave the token.
    pub machine: Option<String>,
}

pub trait InitRunner: Send + Sync {
    fn run_init(&self, url: &str) -> impl Future<Output = Result<InitReport, String>> + Send;
}

pub struct BoardInit {
    pub board: PathBuf,
    pub env: BTreeMap<String, String>,
}

impl BoardInit {
    fn config_path(&self) -> Option<PathBuf> {
        self.env
            .get("AGENT_BOARD_CONFIG_DIR")
            .map(|dir| PathBuf::from(dir).join("config.json"))
    }

    async fn run_board_init(&self, url: &str) -> Result<(), String> {
        let run = Command::new(&self.board)
            .args(["init", "--url", url, "--force"])
            .env_clear()
            .envs(&self.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output();
        let output = match timeout(INIT_LIMIT, run).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => return Err(format!("board init could not run: {error}")),
            Err(_) => return Err("board init did not finish in time".into()),
        };
        if output.status.success() {
            return Ok(());
        }
        Err(failure_message(
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
        )
        .unwrap_or_else(|| format!("board init failed ({})", output.status)))
    }
}

impl InitRunner for BoardInit {
    async fn run_init(&self, url: &str) -> Result<InitReport, String> {
        let held = match self.config_path() {
            Some(path) => Some(machine::hold(&path).map_err(|error| {
                format!("the worker's config.json could not be set aside: {error}")
            })?),
            None => None,
        };
        let outcome = self.run_board_init(url).await;
        match (outcome, held) {
            (Ok(()), Some(held)) => held
                .keep_with_board_name()
                .map(|machine| InitReport { machine })
                .map_err(|error| format!("the worker's config.json could not be updated: {error}")),
            (Ok(()), None) => Ok(InitReport::default()),
            (Err(message), Some(held)) => Err(match held.restore() {
                Ok(()) => message,
                Err(error) => {
                    format!("{message}; the worker's config.json could not be put back ({error})")
                }
            }),
            (Err(message), None) => Err(message),
        }
    }
}

/// `board init` ends with its own health check: a refused token shows up as a failed line (✗) with a hint,
/// far from the top of a long report. Errors of its own (a refused token, a data key the board does not hold, an
/// unreachable board, bad arguments) come on stderr, the first line being the message and the rest its usage text.
fn failure_message(stdout: &str, stderr: &str) -> Option<String> {
    if let Some(first) = stderr.lines().find(|line| !line.trim().is_empty()) {
        return Some(first.trim().trim_start_matches("board:").trim().to_owned());
    }
    let mut parts = Vec::new();
    let mut lines = stdout.lines().peekable();
    while let Some(line) = lines.next() {
        if !line.trim_start().starts_with('✗') {
            continue;
        }
        let mut part = line.trim().trim_start_matches('✗').trim().to_owned();
        if let Some(hint) = lines.next_if(|next| next.trim_start().starts_with('→')) {
            let hint = hint.trim().trim_start_matches('→').trim();
            if !hint.contains("board init") {
                part.push_str(" (");
                part.push_str(hint);
                part.push(')');
            }
        }
        parts.push(part);
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

fn bare_value(pasted: &str, name: &str) -> String {
    let trimmed = pasted.trim();
    trimmed
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('='))
        .unwrap_or(trimmed)
        .trim()
        .to_owned()
}

/// A line break inside a value would add a line of its own to what the credentials command prints.
fn is_single_line(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(char::is_control)
}

pub(crate) fn scrub(message: &str, secrets: &[&str]) -> String {
    let mut text = message.to_owned();
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        text = text.replace(secret, "(withheld)");
    }
    let cleaned: Vec<&str> = text
        .split(' ')
        .map(|word| {
            if looks_like_secret(word) {
                "(withheld)"
            } else {
                word
            }
        })
        .collect();
    cleaned.join(" ").chars().take(MESSAGE_LIMIT).collect()
}

/// A prefix alone ("starts with abm_") is advice; a prefix followed by more of a value is a secret.
fn looks_like_secret(word: &str) -> bool {
    ["abm_", "abk1_"].iter().any(|prefix| {
        word.find(prefix).is_some_and(|at| {
            word[at + prefix.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                .count()
                >= 6
        })
    })
}

pub async fn enrol<R: InitRunner>(
    store: &dyn SecretStore,
    runner: &R,
    url: &str,
    token: &str,
    data_key: &str,
) -> Result<InitReport, EnrolError> {
    let credentials = Credentials {
        token: bare_value(token, "AGENT_BOARD_TOKEN"),
        data_key: bare_value(data_key, "AGENT_BOARD_DATA_KEY"),
    };
    if !is_single_line(&credentials.token) || !is_single_line(&credentials.data_key) {
        return Err(EnrolError::new(
            "the token and the data key must each be one line of text",
        ));
    }

    discard_pending(store)?;
    write_pending(store, &credentials)?;
    match runner.run_init(url).await {
        Ok(report) => match promote_pending(store) {
            Ok(()) => Ok(report),
            Err(error) => Err(with_cleanup(EnrolError::from(error), store)),
        },
        Err(message) => Err(with_cleanup(
            EnrolError::new(scrub(
                &message,
                &[&credentials.token, &credentials.data_key],
            )),
            store,
        )),
    }
}

fn with_cleanup(error: EnrolError, store: &dyn SecretStore) -> EnrolError {
    match discard_pending(store) {
        Ok(()) => error,
        Err(cleanup) => EnrolError::new(format!(
            "{error}; the pending credentials could not be removed from the Keychain ({cleanup})"
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::secrets::testing::DeletesFailWhenArmed;
    use crate::secrets::MemoryStore;
    use crate::worker::credentials::{helper_output, TOKEN_ACCOUNT};

    use super::*;

    struct FakeInit {
        outcome: Result<InitReport, String>,
        seen_by_helper: Mutex<Option<String>>,
        store: std::sync::Arc<MemoryStore>,
    }

    impl InitRunner for FakeInit {
        async fn run_init(&self, _url: &str) -> Result<InitReport, String> {
            *self.seen_by_helper.lock().unwrap() = helper_output(self.store.as_ref()).ok();
            self.outcome.clone()
        }
    }

    fn fake(outcome: Result<InitReport, String>, store: &std::sync::Arc<MemoryStore>) -> FakeInit {
        FakeInit {
            outcome,
            seen_by_helper: Mutex::new(None),
            store: store.clone(),
        }
    }

    #[tokio::test]
    async fn a_refused_init_discards_the_pending_credentials_and_keeps_the_working_ones() {
        let store = std::sync::Arc::new(MemoryStore::default());
        store.set(TOKEN_ACCOUNT, "abm_old").unwrap();
        store
            .set(crate::worker::credentials::DATA_KEY_ACCOUNT, "abk1_old")
            .unwrap();
        let runner = fake(
            Err("the token: a machine token starts with abm_ (abm_pasted-wrong)".into()),
            &store,
        );

        let error = enrol(
            store.as_ref(),
            &runner,
            "http://127.0.0.1:4785",
            "AGENT_BOARD_TOKEN=abm_pasted-wrong",
            "abk1_new",
        )
        .await
        .unwrap_err();

        assert!(runner
            .seen_by_helper
            .lock()
            .unwrap()
            .as_deref()
            .is_some_and(|lines| lines.contains("abm_pasted-wrong")));
        assert!(!error.to_string().contains("pasted-wrong"));
        assert!(error.to_string().contains("(withheld)"));
        assert_eq!(
            helper_output(store.as_ref()).unwrap(),
            "AGENT_BOARD_TOKEN=abm_old\nAGENT_BOARD_DATA_KEY=abk1_old\n"
        );
    }

    struct ArmsTheStore(std::sync::Arc<DeletesFailWhenArmed>);

    impl InitRunner for ArmsTheStore {
        async fn run_init(&self, _url: &str) -> Result<InitReport, String> {
            self.0.arm();
            Err("refused".into())
        }
    }

    #[tokio::test]
    async fn a_refused_pair_that_cannot_be_removed_is_reported_and_still_visible_as_pending() {
        let store = std::sync::Arc::new(DeletesFailWhenArmed::default());

        let error = enrol(
            store.as_ref(),
            &ArmsTheStore(store.clone()),
            "http://127.0.0.1:4785",
            "abm_new",
            "abk1_new",
        )
        .await
        .unwrap_err();

        assert!(error.to_string().starts_with("refused;"));
        assert!(error.to_string().contains("could not be removed"));
        assert!(crate::worker::credentials::pending_present(store.as_ref()).unwrap());
    }

    #[tokio::test]
    async fn an_accepted_init_promotes_the_credentials_without_the_pasted_name_prefix() {
        let store = std::sync::Arc::new(MemoryStore::default());
        let runner = fake(Ok(InitReport::default()), &store);

        enrol(
            store.as_ref(),
            &runner,
            "http://127.0.0.1:4785",
            " AGENT_BOARD_TOKEN=abm_fresh \n",
            "AGENT_BOARD_DATA_KEY=abk1_fresh",
        )
        .await
        .unwrap();

        assert_eq!(
            helper_output(store.as_ref()).unwrap(),
            "AGENT_BOARD_TOKEN=abm_fresh\nAGENT_BOARD_DATA_KEY=abk1_fresh\n"
        );
        assert_eq!(
            store.get(TOKEN_ACCOUNT).unwrap().as_deref(),
            Some("abm_fresh")
        );
    }

    #[tokio::test]
    async fn a_value_with_a_line_break_is_refused_before_anything_is_stored() {
        let store = std::sync::Arc::new(MemoryStore::default());
        let runner = fake(Ok(InitReport::default()), &store);

        let error = enrol(
            store.as_ref(),
            &runner,
            "http://127.0.0.1:4785",
            "abm_x\nAGENT_BOARD_DATA_KEY=abk1_injected",
            "abk1_real",
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("one line"));
        assert!(runner.seen_by_helper.lock().unwrap().is_none());
        assert!(matches!(
            helper_output(store.as_ref()),
            Err(crate::worker::credentials::HelperError::NotEnrolled)
        ));
    }

    #[test]
    fn a_failed_health_check_line_is_what_the_owner_is_told() {
        let report = "wrote /x/web.json\n\n✓ token: present\n✗ server: the token was refused (401)\n    → create a new token on the board's Settings page\n✓ tool claude: /bin/claude\n";

        assert_eq!(
            failure_message(report, "").as_deref(),
            Some("server: the token was refused (401) (create a new token on the board's Settings page)")
        );
        assert_eq!(
            failure_message(
                report,
                "board: --url is needed\nUsage: board init [options]\n"
            )
            .as_deref(),
            Some("--url is needed")
        );
        assert_eq!(failure_message("✓ all fine\n", ""), None);
        assert_eq!(
            failure_message(
                "✗ server: refused the token\n    → run \"board init --force\" again\n",
                ""
            )
            .as_deref(),
            Some("server: refused the token")
        );
    }
}
