use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

use crate::paths::APP_IDENTIFIER;

pub const PROXY_SECRET: &str = "proxy-secret";

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

/// The secret the backend trusts the proxy's headers with; created once per install.
pub fn proxy_secret(store: &dyn SecretStore) -> Result<String, SecretError> {
    if let Some(existing) = store.get(PROXY_SECRET)? {
        return Ok(existing);
    }
    let bytes: [u8; 32] = rand::random();
    let secret: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    store.set(PROXY_SECRET, &secret)?;
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_proxy_secret_is_created_once_and_then_reused() {
        let store = MemoryStore::default();
        let first = proxy_secret(&store).unwrap();
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(proxy_secret(&store).unwrap(), first);
    }
}
