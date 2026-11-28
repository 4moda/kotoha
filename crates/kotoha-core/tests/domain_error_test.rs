//! Unit tests for `kotoha-core` error classification (`src/domain/error.rs`).

use kotoha_core::domain::{CoreError, ErrorScope};
use kotoha_protocol::ProtocolError;

#[test]
fn domain_error_scope_classifies_each_variant() {
    let cases = [
        (CoreError::InvalidAudio("x".into()), ErrorScope::Chunk),
        (CoreError::Translation("x".into()), ErrorScope::Utterance),
        (CoreError::NotConfigured("x".into()), ErrorScope::Utterance),
        (
            CoreError::UnsupportedInPhase1("x".into()),
            ErrorScope::Utterance,
        ),
        (
            CoreError::UnknownModelProfile("x".into()),
            ErrorScope::Utterance,
        ),
        (
            CoreError::InvalidLanguageCode("x".into()),
            ErrorScope::Utterance,
        ),
        (CoreError::Glossary("x".into()), ErrorScope::Utterance),
        (CoreError::Sink("x".into()), ErrorScope::Utterance),
        (
            CoreError::InvalidSecretRef("x".into()),
            ErrorScope::Utterance,
        ),
        (CoreError::SecretStore("x".into()), ErrorScope::Utterance),
        (CoreError::SecretMissing("x".into()), ErrorScope::Utterance),
        (CoreError::Stt("x".into()), ErrorScope::Speaker),
        (CoreError::Ipc("x".into()), ErrorScope::Fatal),
        (
            CoreError::Protocol(ProtocolError::InvalidMagic(0, 0)),
            ErrorScope::Fatal,
        ),
        (CoreError::ChannelClosed("x"), ErrorScope::Fatal),
    ];
    for (index, (err, expected)) in cases.into_iter().enumerate() {
        assert_eq!(err.scope(), expected, "unexpected scope for case #{index}");
        assert_eq!(err.scope().is_fatal(), expected == ErrorScope::Fatal);
    }
}

#[test]
fn domain_error_scope_is_ordered_and_clamped_by_call_site() {
    assert!(ErrorScope::Chunk < ErrorScope::Utterance);
    assert!(ErrorScope::Utterance < ErrorScope::Speaker);
    assert!(ErrorScope::Speaker < ErrorScope::Fatal);

    assert_eq!(
        ErrorScope::Fatal.at_most(ErrorScope::Utterance),
        ErrorScope::Utterance
    );
    assert_eq!(
        ErrorScope::Speaker.at_most(ErrorScope::Speaker),
        ErrorScope::Speaker
    );
    assert_eq!(
        ErrorScope::Chunk.at_most(ErrorScope::Speaker),
        ErrorScope::Chunk
    );
}

#[test]
fn domain_error_translation_failure_reason_never_leaks_message() {
    let secret = "sk-very-secret body={\"error\":\"quota\"}";
    let errors = [
        CoreError::Translation(secret.into()),
        CoreError::NotConfigured(secret.into()),
        CoreError::UnsupportedInPhase1(secret.into()),
        CoreError::UnknownModelProfile(secret.into()),
        CoreError::InvalidLanguageCode(secret.into()),
        CoreError::Glossary(secret.into()),
        CoreError::Stt(secret.into()),
        CoreError::Sink(secret.into()),
        CoreError::Ipc(secret.into()),
        CoreError::InvalidSecretRef(secret.into()),
        CoreError::SecretStore(secret.into()),
        CoreError::SecretMissing(secret.into()),
    ];
    for err in errors {
        let reason = err.translation_failure_reason();
        assert!(!reason.is_empty());
        assert!(!reason.contains("sk-very"), "{reason}");
    }
    assert_eq!(
        CoreError::Translation(secret.into()).translation_failure_reason(),
        "translation provider error"
    );
}

#[test]
fn domain_error_secret_variants_map_to_fixed_reasons() {
    assert_eq!(
        CoreError::SecretMissing("translation.deepl.api_key".into()).translation_failure_reason(),
        "translation provider not configured"
    );
    for err in [
        CoreError::InvalidSecretRef("x".into()),
        CoreError::SecretStore("x".into()),
    ] {
        assert_eq!(err.translation_failure_reason(), "secret store error");
    }
}

#[test]
fn domain_error_kind_is_a_fixed_name_without_the_message() {
    let secret = "sk-very-secret provider body";
    for err in [
        CoreError::Translation(secret.into()),
        CoreError::Stt(secret.into()),
        CoreError::Sink(secret.into()),
        CoreError::NotConfigured(secret.into()),
        CoreError::SecretMissing(secret.into()),
    ] {
        let kind = err.kind();
        assert!(!kind.is_empty());
        assert!(!kind.contains("sk-very"), "{kind}");
    }
    assert_eq!(CoreError::Translation(secret.into()).kind(), "translation");
    assert_eq!(CoreError::ChannelClosed("x").kind(), "channel_closed");
}
