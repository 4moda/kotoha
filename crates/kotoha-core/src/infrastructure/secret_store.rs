//! [`SecretStore`] adapters: in-memory (app runtime / tests) and environment-variable backed
//! (read-only; CLI / E2E).
//!
//! An OS keychain backed store is a planned follow-up; it will implement the same ports.

use std::collections::HashMap;
use std::sync::RwLock;

use crate::domain::{CoreError, Secret, SecretRef, SecretStore, SecretWriter};

/// Default environment variable prefix used by [`EnvSecretStore`].
pub const ENV_SECRET_PREFIX: &str = "KOTOHA_SECRET_";

/// Thread-safe in-memory [`SecretStore`] + [`SecretWriter`]. Values live only for the process
/// lifetime and are never persisted.
#[derive(Default)]
pub struct InMemorySecretStore {
    entries: RwLock<HashMap<SecretRef, Secret>>,
}

impl std::fmt::Debug for InMemorySecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.entries.read().map_or(0, |e| e.len());
        f.debug_struct("InMemorySecretStore")
            .field("entries", &count)
            .finish()
    }
}

impl InMemorySecretStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style insert of `value` under `secret_ref`.
    #[must_use]
    pub fn with(self, secret_ref: &SecretRef, value: impl Into<Secret>) -> Self {
        if let Ok(mut guard) = self.entries.write() {
            guard.insert(secret_ref.clone(), value.into());
        }
        self
    }
}

fn poisoned() -> CoreError {
    CoreError::SecretStore("in-memory secret store lock poisoned".to_owned())
}

impl SecretStore for InMemorySecretStore {
    fn resolve(&self, secret_ref: &SecretRef) -> Result<Secret, CoreError> {
        self.entries
            .read()
            .map_err(|_| poisoned())?
            .get(secret_ref)
            .cloned()
            .filter(|s| !s.is_blank())
            .ok_or_else(|| CoreError::SecretMissing(secret_ref.to_string()))
    }
}

impl SecretWriter for InMemorySecretStore {
    fn set(&self, secret_ref: &SecretRef, value: Secret) -> Result<(), CoreError> {
        self.entries
            .write()
            .map_err(|_| poisoned())?
            .insert(secret_ref.clone(), value);
        Ok(())
    }

    fn delete(&self, secret_ref: &SecretRef) -> Result<(), CoreError> {
        self.entries
            .write()
            .map_err(|_| poisoned())?
            .remove(secret_ref);
        Ok(())
    }
}

type EnvLookup = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Read-only [`SecretStore`] reading secrets from environment variables.
///
/// The mapping from [`SecretRef`] to variable name is injective: letters and digits are
/// upper-cased, a segment separator `.` becomes `__` and a single `_` stays `_`
/// (`translation.google_cloud.api_key` -> `KOTOHA_SECRET_TRANSLATION__GOOGLE_CLOUD__API_KEY`).
/// Together with the [`SecretRef`] grammar no two refs share a variable. The prefix must be
/// non-empty and end with `_`, so a ref can only ever reach variables under that prefix.
pub struct EnvSecretStore {
    prefix: String,
    lookup: EnvLookup,
}

impl std::fmt::Debug for EnvSecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvSecretStore")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl Default for EnvSecretStore {
    fn default() -> Self {
        Self {
            prefix: ENV_SECRET_PREFIX.to_owned(),
            lookup: Box::new(|name| std::env::var(name).ok()),
        }
    }
}

fn validate_prefix(prefix: &str) -> Result<(), CoreError> {
    let valid = prefix.len() > 1
        && prefix.ends_with('_')
        && prefix
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    if valid {
        Ok(())
    } else {
        Err(CoreError::SecretStore(
            "environment secret prefix must be non-empty, upper-case and end with '_'".to_owned(),
        ))
    }
}

impl EnvSecretStore {
    /// Creates a store using [`ENV_SECRET_PREFIX`] and the process environment.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a store using a custom variable `prefix` and the process environment.
    pub fn with_prefix(prefix: impl Into<String>) -> Result<Self, CoreError> {
        Self::with_lookup(prefix, |name| std::env::var(name).ok())
    }

    /// Creates a store with a custom variable lookup (deterministic tests without mutating the
    /// process environment).
    pub fn with_lookup(
        prefix: impl Into<String>,
        lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Result<Self, CoreError> {
        let prefix = prefix.into();
        validate_prefix(&prefix)?;
        Ok(Self {
            prefix,
            lookup: Box::new(lookup),
        })
    }

    /// Returns the environment variable name for `secret_ref`.
    #[must_use]
    pub fn env_var_name(&self, secret_ref: &SecretRef) -> String {
        let mut name = self.prefix.clone();
        for c in secret_ref.as_str().chars() {
            match c {
                '.' => name.push_str("__"),
                '_' => name.push('_'),
                other => name.push(other.to_ascii_uppercase()),
            }
        }
        name
    }
}

impl SecretStore for EnvSecretStore {
    fn resolve(&self, secret_ref: &SecretRef) -> Result<Secret, CoreError> {
        let var = self.env_var_name(secret_ref);
        (self.lookup)(&var)
            .map(Secret::new)
            .filter(|s| !s.is_blank())
            .ok_or_else(|| {
                CoreError::SecretMissing(format!("{secret_ref} (environment variable {var})"))
            })
    }
}
