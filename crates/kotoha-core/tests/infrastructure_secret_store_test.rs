//! Tests for `src/infrastructure/secret_store.rs`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{HashMap, HashSet};

use kotoha_core::domain::{
    CoreError, ProviderSecretKind, Secret, SecretRef, SecretStore, SecretWriter,
};
use kotoha_core::infrastructure::{EnvSecretStore, InMemorySecretStore, ENV_SECRET_PREFIX};

#[test]
fn in_memory_store_reads_writes_and_reports_missing_without_leaking_others() {
    let deepl = SecretRef::deepl_api_key();
    let store = InMemorySecretStore::new()
        .with(&deepl, "deepl-secret-value")
        .with(&SecretRef::google_cloud_api_key(), "   ");

    assert_eq!(
        store.resolve(&deepl).unwrap().expose(),
        "deepl-secret-value"
    );
    assert!(store.is_configured(&deepl));
    assert!(!store.is_configured(&SecretRef::google_cloud_api_key()));

    let err = store.resolve(&SecretRef::openai_api_key()).unwrap_err();
    assert!(
        matches!(err, CoreError::SecretMissing(ref n) if n == "translation.openai_compatible.api_key")
    );
    assert!(!err.to_string().contains("deepl-secret-value"));
    assert!(!format!("{store:?}").contains("deepl-secret-value"));

    assert!(store
        .resolve_if_set(&SecretRef::openai_api_key())
        .unwrap()
        .is_none());
    assert!(store.resolve_if_set(&deepl).unwrap().is_some());

    store
        .set(&SecretRef::openai_api_key(), Secret::new("sk-new"))
        .unwrap();
    assert!(store.is_configured(&SecretRef::openai_api_key()));
    store.delete(&SecretRef::openai_api_key()).unwrap();
    store.delete(&SecretRef::openai_api_key()).unwrap();
    assert!(!store.is_configured(&SecretRef::openai_api_key()));
}

fn env_store(vars: HashMap<String, String>) -> EnvSecretStore {
    EnvSecretStore::with_lookup(ENV_SECRET_PREFIX, move |n| vars.get(n).cloned()).unwrap()
}

#[test]
fn env_store_resolves_values_and_errors_do_not_leak() {
    let vars = HashMap::from([
        (
            "KOTOHA_SECRET_TRANSLATION__DEEPL__API_KEY".to_owned(),
            "env-deepl-value".to_owned(),
        ),
        (
            "KOTOHA_SECRET_TRANSLATION__GOOGLE_CLOUD__API_KEY".to_owned(),
            String::new(),
        ),
        (
            "KOTOHA_SECRET_TRANSLATION__OPENAI_COMPATIBLE__API_KEY".to_owned(),
            "other-secret-value".to_owned(),
        ),
    ]);
    let store = env_store(vars);

    assert_eq!(
        store.resolve(&SecretRef::deepl_api_key()).unwrap().expose(),
        "env-deepl-value"
    );
    for missing in [
        SecretRef::google_cloud_api_key(),
        SecretRef::gas_endpoint_url(),
    ] {
        let err = store.resolve(&missing).unwrap_err();
        assert!(matches!(err, CoreError::SecretMissing(_)));
        let msg = err.to_string();
        assert!(
            msg.contains("KOTOHA_SECRET_"),
            "missing-secret error must name the env variable"
        );
        assert!(
            !msg.contains("env-deepl-value"),
            "missing-secret error must not contain another secret"
        );
        assert!(
            !msg.contains("other-secret-value"),
            "missing-secret error must not contain another secret"
        );
    }
    assert!(!format!("{store:?}").contains("env-deepl-value"));
}

#[test]
fn env_var_mapping_is_injective_and_stays_under_the_prefix() {
    let store = EnvSecretStore::new();
    let mut seen = HashSet::new();
    let mut names: Vec<String> = ProviderSecretKind::ALL
        .iter()
        .map(|k| k.secret_ref().to_string())
        .collect();
    // Names that used to collide under a naive "non-alphanumeric becomes _" mapping.
    names.extend(
        [
            "a.b_c", "a_b.c", "a.b.c", "a_b_c", "ab.c", "a.bc", "a1.b", "a.1b",
        ]
        .map(str::to_owned),
    );
    for name in &names {
        let var = store.env_var_name(&SecretRef::new(name).unwrap());
        assert!(var.starts_with(ENV_SECRET_PREFIX), "{var}");
        assert!(seen.insert(var.clone()), "collision on {var} for {name}");
    }
    assert_eq!(
        store.env_var_name(&SecretRef::google_cloud_api_key()),
        "KOTOHA_SECRET_TRANSLATION__GOOGLE_CLOUD__API_KEY"
    );
}

#[test]
fn env_store_rejects_unsafe_prefixes() {
    for bad in ["", "_", "kotoha_", "KOTOHA", "KOTOHA-", "KOTOHA SECRET_"] {
        assert!(
            matches!(
                EnvSecretStore::with_prefix(bad),
                Err(CoreError::SecretStore(_))
            ),
            "{bad:?}"
        );
    }
    assert!(EnvSecretStore::with_prefix("MYAPP_SECRET_").is_ok());
}

#[test]
fn env_store_reads_real_process_environment_only_under_its_prefix() {
    // cargo sets CARGO_PKG_NAME for test processes; the ref grammar cannot escape the prefix.
    let store = EnvSecretStore::with_prefix("CARGO_PKG_").unwrap();
    assert!(store.is_configured(&SecretRef::new("name").unwrap()));
    assert!(!store.is_configured(&SecretRef::new("surely.not.set").unwrap()));
    // `PATH` is unreachable from the default prefix.
    assert!(!EnvSecretStore::new().is_configured(&SecretRef::new("path").unwrap()));
}
