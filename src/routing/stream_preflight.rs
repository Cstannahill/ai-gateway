//! Look at the start of a provider's stream before committing to it.
//!
//! A streaming response is sent to the client as soon as the provider's HTTP
//! status is good, which means a failure that comes *afterwards* — a reset
//! connection, an `{"error": ...}` frame, a stream that ends empty, a provider
//! that goes quiet — used to reach the client as a broken or blank answer even
//! though another candidate could have served it. [`preflight`] reads events
//! until the first one that is real output; if the stream fails or stalls
//! before that, the caller treats it as a failed attempt (retry, fallback). The
//! events read so far are returned so nothing is lost when it does commit.
//!
//! Once real output exists the client may already have seen it, so failures
//! after that point are not hidden — that boundary is the whole design.

use std::time::Duration;

use futures::{Stream, StreamExt};

use crate::providers::events::ProviderEvent;

/// A stream failure seen before any output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightFailure {
    pub code: String,
    pub message: String,
    pub status_code: Option<u16>,
    pub retryable: bool,
    pub retry_after_ms: Option<u64>,
    /// `true` when the stream never emitted its own terminal event, so the
    /// caller must record the attempt as failed itself (an `Error` event is
    /// already recorded by the stream instrumentation).
    pub unrecorded: bool,
}

#[derive(Debug)]
pub enum Preflight {
    /// Real output (or a clean finish) was seen. `buffered` holds every event
    /// read, in order, ending with the one that triggered the commit; replay
    /// it ahead of the rest of the stream.
    Committed { buffered: Vec<ProviderEvent> },
    /// The stream failed or stalled first. Nothing was sent to the client.
    Failed(PreflightFailure),
}

/// Events that mean the provider is actually producing a response. Lifecycle
/// events (`Started`, `Connected`, `FirstByte` — which also fires for SSE
/// keep-alive comments) and a bare usage update do not.
fn is_output(event: &ProviderEvent) -> bool {
    matches!(
        event,
        ProviderEvent::ContentDelta { .. }
            | ProviderEvent::ReasoningDelta { .. }
            | ProviderEvent::ToolCallDelta { .. }
            | ProviderEvent::Completed { .. }
    )
}

/// Read `stream` until its first output event, an error, its end, or
/// `first_token_timeout` (whichever comes first; `None` waits indefinitely).
pub async fn preflight<S>(stream: &mut S, first_token_timeout: Option<Duration>) -> Preflight
where
    S: Stream<Item = ProviderEvent> + Unpin,
{
    let read = async {
        let mut buffered = Vec::new();
        while let Some(event) = stream.next().await {
            if let ProviderEvent::Error {
                code,
                message,
                status_code,
                retryable,
                retry_after_ms,
                ..
            } = &event
            {
                return Preflight::Failed(PreflightFailure {
                    code: code.clone(),
                    message: message.clone(),
                    status_code: *status_code,
                    retryable: *retryable,
                    retry_after_ms: *retry_after_ms,
                    unrecorded: false,
                });
            }
            let commits = is_output(&event);
            buffered.push(event);
            if commits {
                return Preflight::Committed { buffered };
            }
        }
        // Ended with no output and no terminal event: the connection was
        // closed on us. A model that genuinely says nothing still sends a
        // finish frame, which counts as output above.
        Preflight::Failed(PreflightFailure {
            code: "empty_stream".to_string(),
            message: "Provider closed the stream without producing a response".to_string(),
            status_code: None,
            retryable: true,
            retry_after_ms: None,
            unrecorded: true,
        })
    };

    match first_token_timeout {
        None => read.await,
        Some(limit) => match tokio::time::timeout(limit, read).await {
            Ok(outcome) => outcome,
            Err(_) => Preflight::Failed(PreflightFailure {
                code: "timeout".to_string(),
                message: format!("No output from the provider within {}s", limit.as_secs()),
                status_code: Some(504),
                retryable: true,
                retry_after_ms: None,
                unrecorded: true,
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use futures::stream;

    fn started() -> ProviderEvent {
        ProviderEvent::Started { timestamp: Utc::now(), provider: "p".into(), model: "m".into() }
    }
    fn first_byte() -> ProviderEvent {
        ProviderEvent::FirstByte { timestamp: Utc::now() }
    }
    fn content(s: &str) -> ProviderEvent {
        ProviderEvent::ContentDelta { timestamp: Utc::now(), index: 0, delta: s.into() }
    }
    fn error(code: &str) -> ProviderEvent {
        ProviderEvent::Error {
            timestamp: Utc::now(),
            code: code.into(),
            message: "boom".into(),
            status_code: Some(503),
            retryable: true,
            retry_after_ms: Some(1500),
        }
    }
    fn completed() -> ProviderEvent {
        ProviderEvent::Completed {
            timestamp: Utc::now(),
            finish_reason: Some("stop".into()),
            provider_response_id: None,
            upstream_provider: None,
            raw_response: None,
        }
    }

    #[tokio::test]
    async fn commits_on_first_content_and_leaves_the_rest_unread() {
        let mut s = stream::iter(vec![started(), first_byte(), content("hi"), content("there")]);
        let Preflight::Committed { buffered } = preflight(&mut s, None).await else {
            panic!("expected commit");
        };
        assert_eq!(buffered.len(), 3, "lifecycle events + the committing event");
        assert!(matches!(buffered.last(), Some(ProviderEvent::ContentDelta { delta, .. }) if delta == "hi"));
        assert!(matches!(s.next().await, Some(ProviderEvent::ContentDelta { delta, .. }) if delta == "there"));
    }

    #[tokio::test]
    async fn a_clean_empty_completion_still_commits() {
        let mut s = stream::iter(vec![started(), completed()]);
        assert!(matches!(preflight(&mut s, None).await, Preflight::Committed { .. }));
    }

    #[tokio::test]
    async fn an_error_before_output_fails_with_its_details() {
        let mut s = stream::iter(vec![started(), first_byte(), error("503")]);
        let Preflight::Failed(f) = preflight(&mut s, None).await else { panic!("expected failure") };
        assert_eq!(f.code, "503");
        assert_eq!(f.status_code, Some(503));
        assert_eq!(f.retry_after_ms, Some(1500));
        assert!(f.retryable);
        assert!(!f.unrecorded, "the instrumented stream already recorded the Error event");
    }

    #[tokio::test]
    async fn ending_without_output_is_an_empty_stream_failure() {
        let mut s = stream::iter(vec![started(), first_byte()]);
        let Preflight::Failed(f) = preflight(&mut s, None).await else { panic!("expected failure") };
        assert_eq!(f.code, "empty_stream");
        assert!(f.retryable && f.unrecorded);
    }

    #[tokio::test]
    async fn a_stalled_stream_times_out() {
        let mut s = stream::iter(vec![started()]).chain(stream::pending());
        let Preflight::Failed(f) = preflight(&mut s, Some(Duration::from_millis(50))).await else {
            panic!("expected timeout");
        };
        assert_eq!(f.code, "timeout");
        assert_eq!(f.status_code, Some(504));
        assert!(f.unrecorded);
    }

    #[tokio::test]
    async fn output_arriving_in_time_beats_the_timeout() {
        let mut s = stream::iter(vec![started(), content("x")]);
        assert!(matches!(
            preflight(&mut s, Some(Duration::from_secs(5))).await,
            Preflight::Committed { .. }
        ));
    }
}
