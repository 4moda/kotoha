//! [`SubtitleSink`] decorator that can permanently drop the output of chosen sources.
//!
//! Used for a pipeline that is being retired (e.g. after a language change, #54): the retiring
//! pipeline keeps draining its in-flight utterances, but it no longer receives `SourceReset` /
//! disconnect events, which only reach the new pipeline. Once a source is cleared after the
//! switch, [`SuppressibleSubtitleSink::suppress_source`] makes sure no late update from the
//! retiring pipeline brings that source's cleared cards back.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::domain::{CoreError, SubtitleEntry, SubtitleSink};

/// Forwards to an inner [`SubtitleSink`] except for entries of suppressed sources.
///
/// The suppression check and the forwarded `emit` run under one lock, and
/// [`Self::suppress_source`] takes the same lock. So once `suppress_source` has returned, every
/// later `emit` for that source is dropped, and no `emit` that passed the check earlier is still
/// in progress: a `clear_source` issued afterwards (by another pipeline) cannot be undone by this
/// sink.
///
/// # The inner sink must be fast
///
/// Because the forwarded `emit` runs under that lock, every `emit` through this sink is
/// serialized across all sources of the pipeline using it, and [`Self::suppress_source`] waits
/// for the `emit` in progress. A slow or blocking inner sink therefore stalls that pipeline's
/// subtitle output for every source, and whoever calls `suppress_source` (the bridge's IPC event
/// loop). Wrap only sinks whose `emit` returns promptly, such as one that just forwards to the
/// UI.
pub struct SuppressibleSubtitleSink {
    inner: Arc<dyn SubtitleSink>,
    suppressed: Mutex<HashSet<String>>,
}

impl SuppressibleSubtitleSink {
    /// Wraps `inner` with no source suppressed.
    #[must_use]
    pub fn new(inner: Arc<dyn SubtitleSink>) -> Self {
        Self {
            inner,
            suppressed: Mutex::new(HashSet::new()),
        }
    }

    /// Drops every later [`SubtitleSink::emit`] for `source`, permanently.
    ///
    /// Waits for an `emit` in progress to finish first (see the type docs).
    pub async fn suppress_source(&self, source: &str) {
        self.suppressed.lock().await.insert(source.to_owned());
    }
}

#[async_trait]
impl SubtitleSink for SuppressibleSubtitleSink {
    async fn emit(&self, entry: SubtitleEntry) -> Result<(), CoreError> {
        let suppressed = self.suppressed.lock().await;
        if suppressed.contains(&entry.speaker_key.source) {
            return Ok(());
        }
        let result = self.inner.emit(entry).await;
        drop(suppressed);
        result
    }

    async fn clear_source(&self, source: &str) -> Result<(), CoreError> {
        self.inner.clear_source(source).await
    }
}
