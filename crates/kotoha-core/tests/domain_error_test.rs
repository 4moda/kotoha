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
        (CoreError::Stt("x".into()), ErrorScope::Speaker),
        (CoreError::Ipc("x".into()), ErrorScope::Fatal),
        (
            CoreError::Protocol(ProtocolError::InvalidMagic(0, 0)),
            ErrorScope::Fatal,
        ),
        (CoreError::ChannelClosed("x"), ErrorScope::Fatal),
    ];
    for (err, expected) in cases {
        assert_eq!(err.scope(), expected, "{err:?}");
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
    ];
    for err in errors {
        let reason = err.translation_failure_reason();
        assert!(!reason.is_empty());
        assert!(!reason.contains("secret"), "{reason}");
    }
    assert_eq!(
        CoreError::Translation(secret.into()).translation_failure_reason(),
        "translation provider error"
    );
}
