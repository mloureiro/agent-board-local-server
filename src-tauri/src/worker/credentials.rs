use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::Path;

use crate::secrets::{SecretError, SecretStore};

pub const TOKEN_ACCOUNT: &str = "worker-token";
pub const DATA_KEY_ACCOUNT: &str = "worker-data-key";
const PENDING_TOKEN_ACCOUNT: &str = "worker-token-pending";
const PENDING_DATA_KEY_ACCOUNT: &str = "worker-data-key-pending";

const TOKEN_NAME: &str = "AGENT_BOARD_TOKEN";
const DATA_KEY_NAME: &str = "AGENT_BOARD_DATA_KEY";

pub const HELPER_FLAG: &str = "--worker-credentials";
/// The CLI runs its credentials command without arguments, so the app is reached through a link of this name.
pub const HELPER_LINK: &str = "worker-credentials";

#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub token: String,
    pub data_key: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials(..)")
    }
}

fn read_pair(
    store: &dyn SecretStore,
    token: &str,
    data_key: &str,
) -> Result<Option<Credentials>, SecretError> {
    match (store.get(token)?, store.get(data_key)?) {
        (Some(token), Some(data_key)) => Ok(Some(Credentials { token, data_key })),
        _ => Ok(None),
    }
}

pub fn enrolled(store: &dyn SecretStore) -> Result<bool, SecretError> {
    Ok(read_pair(store, TOKEN_ACCOUNT, DATA_KEY_ACCOUNT)?.is_some())
}

pub fn write_pending(
    store: &dyn SecretStore,
    credentials: &Credentials,
) -> Result<(), SecretError> {
    store.set(PENDING_TOKEN_ACCOUNT, &credentials.token)?;
    store.set(PENDING_DATA_KEY_ACCOUNT, &credentials.data_key)
}

pub fn promote_pending(store: &dyn SecretStore) -> Result<(), SecretError> {
    let Some(pending) = read_pair(store, PENDING_TOKEN_ACCOUNT, PENDING_DATA_KEY_ACCOUNT)? else {
        return Ok(());
    };
    store.set(TOKEN_ACCOUNT, &pending.token)?;
    store.set(DATA_KEY_ACCOUNT, &pending.data_key)?;
    discard_pending(store)
}

pub fn discard_pending(store: &dyn SecretStore) -> Result<(), SecretError> {
    store.delete(PENDING_TOKEN_ACCOUNT)?;
    store.delete(PENDING_DATA_KEY_ACCOUNT)
}

#[derive(Debug)]
pub enum HelperError {
    NotEnrolled,
    Store(SecretError),
}

impl fmt::Display for HelperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotEnrolled => f.write_str("not enrolled"),
            Self::Store(error) => error.fmt(f),
        }
    }
}

/// What the CLI reads through `AGENT_BOARD_CREDENTIALS_COMMAND`. Pending credentials win while an enrolment
/// is running, because `board init` validates them through this very command before they are promoted.
pub fn helper_output(store: &dyn SecretStore) -> Result<String, HelperError> {
    let pending = read_pair(store, PENDING_TOKEN_ACCOUNT, PENDING_DATA_KEY_ACCOUNT)
        .map_err(HelperError::Store)?;
    let current = match pending {
        Some(pending) => pending,
        None => read_pair(store, TOKEN_ACCOUNT, DATA_KEY_ACCOUNT)
            .map_err(HelperError::Store)?
            .ok_or(HelperError::NotEnrolled)?,
    };
    Ok(format!(
        "{TOKEN_NAME}={}\n{DATA_KEY_NAME}={}\n",
        current.token, current.data_key
    ))
}

pub fn run_helper_if_requested(
    args: impl IntoIterator<Item = OsString>,
    store: &dyn SecretStore,
) -> Option<i32> {
    let mut args = args.into_iter();
    let program = args.next()?;
    let named_as_helper = Path::new(&program).file_name() == Some(OsStr::new(HELPER_LINK));
    let first = args.next();
    let requested = named_as_helper || first.as_deref() == Some(OsStr::new(HELPER_FLAG));
    if !requested {
        return None;
    }
    let extra = if named_as_helper {
        first.is_some()
    } else {
        args.next().is_some()
    };
    if extra {
        eprintln!("{HELPER_FLAG} takes no other argument");
        return Some(2);
    }
    match helper_output(store) {
        Ok(lines) => {
            print!("{lines}");
            Some(0)
        }
        Err(error) => {
            eprintln!("{error}");
            Some(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::secrets::MemoryStore;

    use super::*;

    fn pair(token: &str, data_key: &str) -> Credentials {
        Credentials {
            token: token.into(),
            data_key: data_key.into(),
        }
    }

    #[test]
    fn the_helper_prints_the_two_lines_of_an_enrolled_store_and_refuses_an_empty_one() {
        let store = MemoryStore::default();
        assert!(matches!(
            helper_output(&store),
            Err(HelperError::NotEnrolled)
        ));

        store.set(TOKEN_ACCOUNT, "abm_one").unwrap();
        assert!(matches!(
            helper_output(&store),
            Err(HelperError::NotEnrolled)
        ));

        store.set(DATA_KEY_ACCOUNT, "abk1_one").unwrap();
        assert_eq!(
            helper_output(&store).unwrap(),
            "AGENT_BOARD_TOKEN=abm_one\nAGENT_BOARD_DATA_KEY=abk1_one\n"
        );
    }

    #[test]
    fn pending_credentials_are_visible_to_the_helper_until_they_are_promoted_or_discarded() {
        let store = MemoryStore::default();
        store.set(TOKEN_ACCOUNT, "abm_old").unwrap();
        store.set(DATA_KEY_ACCOUNT, "abk1_old").unwrap();
        write_pending(&store, &pair("abm_new", "abk1_new")).unwrap();

        assert!(helper_output(&store).unwrap().contains("abm_new"));

        discard_pending(&store).unwrap();
        assert!(helper_output(&store).unwrap().contains("abm_old"));

        write_pending(&store, &pair("abm_new", "abk1_new")).unwrap();
        promote_pending(&store).unwrap();
        assert_eq!(
            helper_output(&store).unwrap(),
            "AGENT_BOARD_TOKEN=abm_new\nAGENT_BOARD_DATA_KEY=abk1_new\n"
        );
        assert_eq!(store.get(PENDING_TOKEN_ACCOUNT).unwrap(), None);
        assert_eq!(store.get(PENDING_DATA_KEY_ACCOUNT).unwrap(), None);
    }

    #[test]
    fn the_helper_mode_needs_exactly_its_flag() {
        let store = MemoryStore::default();
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();

        assert_eq!(run_helper_if_requested(args(&[]), &store), None);
        assert_eq!(run_helper_if_requested(args(&["app"]), &store), None);
        assert_eq!(
            run_helper_if_requested(args(&["app", "--other"]), &store),
            None
        );
        assert_eq!(
            run_helper_if_requested(args(&["app", HELPER_FLAG, "extra"]), &store),
            Some(2)
        );
        assert_eq!(
            run_helper_if_requested(args(&["app", HELPER_FLAG]), &store),
            Some(1)
        );
    }

    #[test]
    fn the_helper_mode_is_also_entered_by_the_name_the_cli_runs_it_under() {
        let store = MemoryStore::default();
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();

        assert_eq!(
            run_helper_if_requested(args(&["/app/worker/worker-credentials"]), &store),
            Some(1)
        );
        assert_eq!(
            run_helper_if_requested(args(&["/app/worker/worker-credentials", "x"]), &store),
            Some(2)
        );
        assert_eq!(
            run_helper_if_requested(args(&["/app/worker/other"]), &store),
            None
        );
    }
}
