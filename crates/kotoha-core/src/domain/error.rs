//! Typed domain, application, and adapter errors for `kotoha-core`.

use kotoha_protocol::ProtocolError;
use thiserror::Error;

/// Unified error type across the `kotoha-core` domain, application, and infrastructure layers.
#[derive(Debug, Error)]
pub enum CoreError {
    /// Invalid language code syntax.
    #[error("invalid language code: '{0}'")]
    InvalidLanguageCode(String),

    /// Invalid audio input parameters.
    #[error("invalid audio parameters: {0}")]
    InvalidAudio(String),

    /// Protocol codec or framing error.
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),

    /// IPC transport error.
    #[error("IPC transport error: {0}")]
    Ipc(String),

    /// Speech-to-text engine error.
    #[error("STT error: {0}")]
    Stt(String),

    /// Translation provider error.
    #[error("translation error: {0}")]
    Translation(String),

    /// Requested model profile was not found in the catalog.
    #[error("unknown model profile: '{0}'")]
    UnknownModelProfile(String),

    /// Provider is not configured with required credentials or endpoints.
    #[error("provider not configured: {0}")]
    NotConfigured(String),

    /// A secret reference name is malformed.
    #[error("invalid secret reference: {0}")]
    InvalidSecretRef(String),

    /// The secret store could not be read or written.
    #[error("secret store error: {0}")]
    SecretStore(String),

    /// A referenced secret (API key, token, capability URL) is not available in the secret store.
    ///
    /// Carries only the non-secret reference name, never a secret value.
    #[error("secret '{0}' is not configured in the secret store")]
    SecretMissing(String),

    /// Provider backend requires Phase 2 engine crates not yet attached.
    #[error("provider unsupported in Phase 1 core: {0}")]
    UnsupportedInPhase1(String),

    /// Glossary loading, validation, or management error.
    #[error("glossary error: {0}")]
    Glossary(String),

    /// Subtitle sink emission failed.
    #[error("subtitle sink error: {0}")]
    Sink(String),

    /// Channel or runtime synchronization failure.
    #[error("pipeline channel closed: {0}")]
    ChannelClosed(&'static str),
}

/// Blast radius of a [`CoreError`] inside the streaming pipeline.
///
/// Variants are ordered from the narrowest to the widest scope, so `Ord` comparisons read as
/// "is at least as severe as". The pipeline recovers from every scope except [`Self::Fatal`] by
/// discarding only the affected unit of work and keeps processing other chunks, utterances, and
/// speakers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorScope {
    /// Only the current audio chunk is unusable: drop it and keep the speaker's STT stream.
    Chunk,
    /// Only the current utterance (or a single subtitle update) failed, e.g. a translation
    /// provider timeout or HTTP 429.
    Utterance,
    /// The speaker's STT stream is in an unknown state: drop it (it is recreated lazily).
    Speaker,
    /// The pipeline itself can no longer make progress (input transport failure or a broken
    /// internal invariant): `PipelineService::run` stops and returns the error.
    Fatal,
}

impl ErrorScope {
    /// Narrows `self` so that it never exceeds `ceiling`.
    ///
    /// Call sites use this to bound what an adapter error may affect: for example, a failing
    /// translator never takes down more than its own utterance, whatever variant it returns.
    #[must_use]
    pub fn at_most(self, ceiling: Self) -> Self {
        self.min(ceiling)
    }

    /// Returns `true` for [`Self::Fatal`].
    #[must_use]
    pub const fn is_fatal(self) -> bool {
        matches!(self, Self::Fatal)
    }
}

impl CoreError {
    /// Classifies the intrinsic blast radius of this error (pure, no I/O).
    ///
    /// The pipeline combines this with the call site through [`ErrorScope::at_most`] to pick the
    /// recovery action.
    #[must_use]
    pub const fn scope(&self) -> ErrorScope {
        match self {
            Self::InvalidAudio(_) => ErrorScope::Chunk,
            Self::InvalidLanguageCode(_)
            | Self::Translation(_)
            | Self::UnknownModelProfile(_)
            | Self::NotConfigured(_)
            | Self::UnsupportedInPhase1(_)
            | Self::Glossary(_)
            | Self::Sink(_) => ErrorScope::Utterance,
            Self::Stt(_) => ErrorScope::Speaker,
            Self::Protocol(_) | Self::Ipc(_) | Self::ChannelClosed(_) => ErrorScope::Fatal,
        }
    }

    /// Short, user-safe reason for a failed translation, suitable for a subtitle card.
    ///
    /// Returns a fixed phrase per variant instead of the error message: adapter messages can
    /// embed provider response bodies, so they are never forwarded to the UI.
    #[must_use]
    pub const fn translation_failure_reason(&self) -> &'static str {
        match self {
            Self::Translation(_) => "translation provider error",
            Self::NotConfigured(_) => "translation provider not configured",
            Self::UnknownModelProfile(_) | Self::UnsupportedInPhase1(_) => {
                "translation provider unavailable"
            }
            Self::InvalidLanguageCode(_) => "language pair not supported",
            Self::Glossary(_) => "glossary error",
            Self::InvalidAudio(_)
            | Self::Protocol(_)
            | Self::Ipc(_)
            | Self::Stt(_)
            | Self::Sink(_)
            | Self::ChannelClosed(_) => "internal error",
        }
    }
}
