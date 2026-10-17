//! API keys live in the OS keychain (macOS Keychain, Windows Credential Manager,
//! Secret Service on Linux), never in the config file.
//!
//! Keys are kept in memory after the first read, so the OS asks for keychain access
//! at most once per account per run instead of on every request.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

const SERVICE: &str = "io.github.Admiralkheir.Cue";

static CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn cache() -> MutexGuard<'static, Option<HashMap<String, String>>> {
    CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, thiserror::Error)]
#[error("keychain error: {0}")]
pub struct SecretError(#[from] keyring::Error);

/// The key stored for `account_id`, if any.
pub fn api_key(account_id: &str) -> Result<Option<String>, SecretError> {
    if let Some(key) = cache().as_ref().and_then(|keys| keys.get(account_id)) {
        return Ok(Some(key.clone()));
    }
    match keyring::Entry::new(SERVICE, account_id)?.get_password() {
        Ok(key) => {
            cache()
                .get_or_insert_default()
                .insert(account_id.to_owned(), key.clone());
            Ok(Some(key))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

pub fn set_api_key(account_id: &str, key: &str) -> Result<(), SecretError> {
    keyring::Entry::new(SERVICE, account_id)?.set_password(key)?;
    cache()
        .get_or_insert_default()
        .insert(account_id.to_owned(), key.to_owned());
    Ok(())
}

/// Removes the key; succeeds if there was none.
pub fn delete_api_key(account_id: &str) -> Result<(), SecretError> {
    if let Some(keys) = cache().as_mut() {
        keys.remove(account_id);
    }
    match keyring::Entry::new(SERVICE, account_id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(err.into()),
    }
}
