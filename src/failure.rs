//! What a 5xx says — to the operator, and to the client (ADR-0011 rule 4, #29).
//!
//! On 2026-07-25 a missing column made every ingest return HTTP 500, and the raw
//! SQL error travelled to Trunk Recorder while the server recorded nothing. Both
//! halves of that are wrong, and they are the same bug: the one machine that
//! could have written the cause down handed it to the one that couldn't.
//!
//! So a handler that fails returns a [`ServerError`] naming what it was doing and
//! why it failed, and exactly one place decides what happens to it: the cause is
//! logged at ERROR against the request's correlation id — in the span the failure
//! happened in, so an ingest 500 still names its System and Talkgroup — and the
//! client gets that id and nothing else. An operator greps one ref and finds
//! everything; internals stop travelling to clients and into pasted logs.
//!
//! The redaction is applied to **every** 5xx, not only to the ones built here — a
//! future handler that 500s some other way is covered by construction rather than
//! by remembering. Such a response has no cause to log, which is itself worth an
//! ERROR line: a 500 nobody wrote down is the thing this module exists to
//! prevent.
//!
//! The known rdio-compatible outcomes (`Call imported successfully.`, `duplicate
//! call rejected`, `Incomplete call data: no talkgroup`) are a wire contract and
//! are *not* failures — they never pass through here.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use tracing::{Span, error};

use crate::http_log::RequestId;

/// An internal failure on its way to becoming a 500.
///
/// `stage` names what the server was doing, as a single greppable slug in the
/// same shape as an ingest rejection's `reason` — `dedup`, `store-call`,
/// `search-calls`. `cause` is the underlying error's own words, which go to the
/// log and nowhere else.
#[derive(Debug, Clone)]
pub struct ServerError {
    stage: &'static str,
    cause: String,
    /// Where the failure happened, captured at construction and re-entered when
    /// the line is finally written. The line is emitted after the handler has
    /// returned, so without this it would carry the request id and nothing else
    /// — an ingest 500 has to name the System and Talkgroup it was about.
    origin: Span,
}

impl ServerError {
    /// Wrap an error with the stage that produced it.
    pub fn new(stage: &'static str, cause: impl std::fmt::Display) -> Self {
        ServerError {
            stage,
            cause: cause.to_string(),
            origin: Span::current(),
        }
    }
}

impl IntoResponse for ServerError {
    /// A 500 carrying the cause in its extensions, where [`redact`] picks it up.
    /// The body is written there too, since only the middleware knows the ref.
    fn into_response(self) -> Response {
        let mut response = StatusCode::INTERNAL_SERVER_ERROR.into_response();
        response.extensions_mut().insert(self);
        response
    }
}

/// The body a client gets for any 5xx: the correlation ref, and nothing else.
fn ref_body(request_id: &RequestId) -> String {
    format!("internal error (ref: {request_id})\n")
}

/// Record a failing response's cause against `request_id` and replace its body
/// with the ref alone. Anything that isn't a 5xx passes through untouched.
///
/// Call this inside the request's span, so the ERROR line carries the same
/// `request_id` the client is handed.
pub(crate) fn redact(response: Response, request_id: &RequestId) -> Response {
    let status = response.status();
    if !status.is_server_error() {
        return response;
    }
    match response.extensions().get::<ServerError>() {
        // Written where it happened, not where it was noticed: re-entering the
        // handler's span puts the upload's System and Talkgroup back on the line
        // (and the request id with them, since that span is a child of this one).
        // `%stage`, not the default: `stage=dedup` greps and `stage="dedup"`
        // does not (as with the request log's path).
        Some(failure) => failure
            .origin
            .in_scope(|| error!(stage = %failure.stage, cause = %failure.cause, "server error")),
        // Not reachable from our own handlers, and deliberately loud if it ever
        // is: a 500 with no recorded cause is the 2026-07-25 failure mode.
        None => error!("server error with no recorded cause"),
    }
    (status, ref_body(request_id)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::LogCapture;
    use axum::body::to_bytes;
    use rstest::rstest;

    async fn body_text(response: Response) -> String {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        String::from_utf8(bytes.to_vec()).expect("utf-8 body")
    }

    /// The client is told where to look and nothing more; the operator is told
    /// everything, against the same ref.
    #[tokio::test]
    async fn a_failure_becomes_a_ref_for_the_client_and_a_cause_for_the_log() {
        let capture = LogCapture::start();
        let id = RequestId::new();

        let response = redact(
            ServerError::new("store-call", "no such column: systems.auto_populate").into_response(),
            &id,
        );

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = body_text(response).await;
        assert_eq!(body, format!("internal error (ref: {id})\n"));
        assert!(
            !body.contains("auto_populate"),
            "the cause must not travel to the client: {body:?}"
        );

        let logged = capture.text();
        assert!(logged.contains(" ERROR "), "{logged}");
        assert!(logged.contains("stage=store-call"), "{logged}");
        assert!(
            logged.contains("cause=no such column: systems.auto_populate"),
            "{logged}"
        );
    }

    /// Only failures are rewritten. A 200 body is a contract elsewhere in the
    /// app — the rdio response strings above all — and must arrive untouched.
    #[rstest]
    #[case(StatusCode::OK)]
    #[case(StatusCode::UNAUTHORIZED)]
    #[case(StatusCode::EXPECTATION_FAILED)]
    #[case(StatusCode::NOT_FOUND)]
    #[tokio::test]
    async fn a_non_failure_passes_through_untouched(#[case] status: StatusCode) {
        let capture = LogCapture::start();

        let response = redact(
            (status, "Call imported successfully.\n").into_response(),
            &RequestId::new(),
        );

        assert_eq!(response.status(), status);
        assert_eq!(body_text(response).await, "Call imported successfully.\n");
        assert_eq!(
            capture.text(),
            "",
            "nothing to report: {:?}",
            capture.text()
        );
    }

    /// A 5xx from somewhere that didn't build a [`ServerError`] still loses its
    /// body and still leaves a line — silence is the one outcome ruled out.
    #[tokio::test]
    async fn a_5xx_with_no_recorded_cause_is_still_redacted_and_still_logged() {
        let capture = LogCapture::start();
        let id = RequestId::new();

        let response = redact(
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "connection pool exhausted at db.rs:41\n",
            )
                .into_response(),
            &id,
        );

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body_text(response).await,
            format!("internal error (ref: {id})\n")
        );
        let logged = capture.text();
        assert!(logged.contains(" ERROR "), "{logged}");
        assert!(logged.contains("no recorded cause"), "{logged}");
        capture.assert_never_logged("db.rs:41");
    }

    /// The ref in the body is the request id verbatim — that identity is what
    /// makes "grep the server for this string" work at all.
    #[test]
    fn the_body_carries_the_request_id_verbatim() {
        let id = RequestId::new();
        assert_eq!(ref_body(&id), format!("internal error (ref: {id})\n"));
        assert!(ref_body(&id).contains(id.as_str()));
    }
}
