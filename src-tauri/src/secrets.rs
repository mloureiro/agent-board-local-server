use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::Mutex;

use crate::paths::{AppPaths, APP_IDENTIFIER};
use crate::worker::credentials;

pub const ACCOUNTS: [&str; 4] = [
    credentials::TOKEN_ACCOUNT,
    credentials::DATA_KEY_ACCOUNT,
    credentials::PENDING_TOKEN_ACCOUNT,
    credentials::PENDING_DATA_KEY_ACCOUNT,
];

#[derive(Debug)]
pub struct SecretError(pub String);

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "keychain: {}", self.0)
    }
}

impl std::error::Error for SecretError {}

pub trait SecretStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError>;
    fn set(&self, account: &str, value: &str) -> Result<(), SecretError>;
    fn delete(&self, account: &str) -> Result<(), SecretError>;
}

pub struct Keychain {
    service: String,
}

impl Keychain {
    pub fn app() -> Self {
        Self::with_service(APP_IDENTIFIER)
    }

    /// Any root other than the default gets a Keychain service of its own, so a scratch root never reads,
    /// replaces or deletes the installed app's items.
    pub fn for_root(root: &Path) -> Self {
        if AppPaths::default_root().as_deref() == Some(root) {
            Self::app()
        } else {
            Self::with_service(format!("{APP_IDENTIFIER}.root:{}", root.display()))
        }
    }

    pub fn with_service(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, account).map_err(|error| SecretError(error.to_string()))
    }
}

impl SecretStore for Keychain {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        match self.entry(account)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(SecretError(error.to_string())),
        }
    }

    fn set(&self, account: &str, value: &str) -> Result<(), SecretError> {
        self.entry(account)?
            .set_password(value)
            .map_err(|error| SecretError(error.to_string()))
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(SecretError(error.to_string())),
        }
    }
}

#[derive(Default)]
pub struct MemoryStore {
    values: Mutex<HashMap<String, String>>,
}

impl SecretStore for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        Ok(self
            .values
            .lock()
            .expect("store lock")
            .get(account)
            .cloned())
    }

    fn set(&self, account: &str, value: &str) -> Result<(), SecretError> {
        self.values
            .lock()
            .expect("store lock")
            .insert(account.to_owned(), value.to_owned());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        self.values.lock().expect("store lock").remove(account);
        Ok(())
    }
}

#[cfg(test)]
pub mod testing {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{MemoryStore, SecretError, SecretStore};

    #[derive(Default)]
    pub struct DeletesFailWhenArmed {
        inner: MemoryStore,
        armed: AtomicBool,
    }

    impl DeletesFailWhenArmed {
        pub fn arm(&self) {
            self.armed.store(true, Ordering::SeqCst);
        }
    }

    impl SecretStore for DeletesFailWhenArmed {
        fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
            self.inner.get(account)
        }

        fn set(&self, account: &str, value: &str) -> Result<(), SecretError> {
            self.inner.set(account, value)
        }

        fn delete(&self, account: &str) -> Result<(), SecretError> {
            if self.armed.load(Ordering::SeqCst) {
                return Err(SecretError("locked".into()));
            }
            self.inner.delete(account)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_default_root_uses_the_installed_apps_keychain_service() {
        let default = AppPaths::default_root().expect("HOME is set in the test environment");
        assert_eq!(Keychain::for_root(&default).service, APP_IDENTIFIER);
        let scratch = Keychain::for_root(Path::new("/tmp/scratch-root")).service;
        assert_ne!(scratch, APP_IDENTIFIER);
        assert!(scratch.ends_with(":/tmp/scratch-root"));
    }
}
