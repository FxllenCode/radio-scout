//! One line per HTTP request (ADR-0011, ticket #28).
//!
//! The server has to be able to answer *is anything reaching me?* — the question
//! that cost an hour on 2026-07-25, when a recorder was uploading into a 500 and
//! the only evidence either side produced was the recorder's own log. So every
//! request leaves a line naming what was asked for and how it went, and a
//! request that arrives malformed, 404s, or is rejected leaves one too: "nothing
//! arrived" and "something arrived wrong" must not look identical.
//!
//! What a line says, and at what level, is the whole design:
//!
//! - **Level = the louder of the route's class and its outcome.** The chatty
//!   classes sit at DEBUG — embedded SPA assets, `/api/call/{id}/audio` (a media
//!   element issues several range requests per Call), and the `/healthz` probe a
//!   packaged deployment (#23) hits every few seconds. A Pi must not write a
//!   line for each. A 4xx or 5xx escalates that to WARN or ERROR regardless of
//!   class, so a failing probe or a broken range request is never silent.
//! - **The path, never the query string.** Access codes are a query parameter in
//!   rdio-scanner's world and ADR-0008 keeps that shape; logging `uri()` whole
//!   would make rule 2 ("a secret is never logged") conditional on which routes
//!   happen to exist today.
//! - **Client addresses follow rule 5.** A recorder's IP rides an ingest line at
//!   any level — it is the operator's own infrastructure, and *which host sent
//!   this* is the diagnostic that matters. A listener's IP may ride only a line
//!   that is already DEBUG, which in practice means the audio and asset requests
//!   it makes: the archive and live-feed routes rest at INFO, so their addresses
//!   are unrecorded at *every* verbosity, not merely hidden at the default. That
//!   is stricter than rule 5 requires and deliberately so — rdio-scanner logs
//!   every listener's IP and access-code ident at info (`client.go:152`).
//! - **Every line carries a request id**, as a span field, so the lines a
//!   handler emits while serving the request carry it too. It is also the 5xx
//!   correlation ref (#29): a failing response's cause is logged against it here
//!   and the body replaced with the ref alone ([`crate::failure`]), and the
//!   response echoes it in `x-request-id` so the operator watching a client can
//!   grep the server.
//!
//! The address is the TCP peer's, never `X-Forwarded-For`: the header is
//! attacker-controlled, and trusting it would let anyone forge a recorder's IP
//! into the operator's log. Behind a reverse proxy every client therefore reads
//! as the proxy until #17's config can name the proxies that may be believed.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use axum::extract::ConnectInfo;
use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use tracing::{Instrument, Level, span};
use uuid::Uuid;

/// The response header carrying the request's correlation id. An operator
/// reading a client's failure can grep the server's log for the same value.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// A per-request correlation id.
///
/// 16 hex characters (64 bits): enough that two requests in an operator's log
/// window will not collide, short enough to read out of a 5xx body over the
/// phone and to repeat on every line of a request without drowning the fields
/// that carry the actual news.
///
/// It is always generated here and never taken from an inbound header, so
/// nothing a client sends can steer what lands in the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestId(String);

impl RequestId {
    pub(crate) fn new() -> Self {
        // The low 64 bits of a v4 UUID: random but for the two variant bits.
        RequestId(format!("{:016x}", Uuid::new_v4().as_u128() as u64))
    }

    /// The id as it appears in the log and in `x-request-id`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What kind of traffic a path is. This decides the line's resting level and
/// whether a client address may ride it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteClass {
    /// The recorder-facing upload endpoints. The operator's own infrastructure
    /// talking to us, and the traffic the request log exists for.
    Ingest,
    /// Traffic that arrives many times over for one user-visible event: SPA
    /// assets, a Call's audio ranges, the liveness probe.
    Chatty,
    /// Everything else — archive, admin, the live-feed upgrade, SPA navigation,
    /// and anything unrouted.
    Other,
}

impl RouteClass {
    /// Classify a request path (never a full URI — see the module docs).
    fn of(path: &str) -> RouteClass {
        match path {
            "/api/call-upload" | "/api/trunk-recorder-call-upload" => RouteClass::Ingest,
            // A packaged deployment (#23) probes this every few seconds; at INFO
            // that is thousands of lines a day saying nothing happened.
            "/healthz" => RouteClass::Chatty,
            _ if is_call_audio(path) || is_spa_asset(path) => RouteClass::Chatty,
            _ => RouteClass::Other,
        }
    }
}

/// `GET /api/call/{id}/audio`, which a media element re-requests by range.
fn is_call_audio(path: &str) -> bool {
    path.starts_with("/api/call/") && path.ends_with("/audio")
}

/// An embedded SPA asset: anything outside the API namespace whose last segment
/// has an extension (`/assets/index-a1b2.js`, `/favicon.svg`). A client-side
/// route (`/`, `/archive`) has none, so a navigation stays a notable event while
/// the dozen fetches it triggers do not. A client route that grew a dot in its
/// last segment would be demoted with them; ours are extension-free by design,
/// and the cost of being wrong is a navigation logged one level down.
fn is_spa_asset(path: &str) -> bool {
    !path.starts_with("/api/")
        && path
            .rsplit('/')
            .next()
            .is_some_and(|segment| segment.contains('.'))
}

/// The level a request's line is emitted at: the route's resting level, escalated
/// by a failing outcome. A 4xx or 5xx is never demoted by its class — a `/healthz`
/// that 500s or an audio range that 416s is exactly what an operator must see.
fn level_for(class: RouteClass, status: StatusCode) -> Level {
    if status.is_server_error() {
        Level::ERROR
    } else if status.is_client_error() {
        Level::WARN
    } else if class == RouteClass::Chatty {
        Level::DEBUG
    } else {
        Level::INFO
    }
}

/// May the client's address ride a line at `level` for `class` (ADR-0011 rule 5)?
///
/// Ingest routes always: a recorder is the operator's own hardware. Everything
/// else only on a line that is already DEBUG — which in practice means a
/// listener's address appears with the audio and asset requests it makes, and
/// only when someone has turned the verbosity up to look.
fn logs_client_addr(class: RouteClass, level: Level) -> bool {
    class == RouteClass::Ingest || matches!(level, Level::DEBUG | Level::TRACE)
}

/// Middleware: log one line per request, and hang a [`RequestId`] on the request
/// (extensions), the response (`x-request-id`) and the span its handler runs in.
pub async fn log_requests(mut request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let class = RouteClass::of(&path);
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());

    let request_id = RequestId::new();
    request.extensions_mut().insert(request_id.clone());

    // The span's level is the verbosity at which it *exists*, not a level it is
    // reported at. ERROR means it exists whenever anything is being logged at
    // all, so a handler's 5xx line still carries its id under `RUST_LOG=warn` —
    // an INFO span would vanish exactly when correlating matters most.
    let span = span!(Level::ERROR, "http", request_id = %request_id);

    let started = Instant::now();
    let response = next.run(request).instrument(span.clone()).await;
    let elapsed = started.elapsed();

    let status = response.status();
    let level = level_for(class, status);
    let client_addr = peer.filter(|_| logs_client_addr(class, level));
    // Inside the span, so the failure's ERROR line carries the same id the
    // client is handed (#29) — the cause first, then the request it belonged to.
    let mut response = span.in_scope(|| {
        let response = crate::failure::redact(response, &request_id);
        request_line(level, &method, &path, status, elapsed, client_addr);
        response
    });

    response.headers_mut().insert(
        HeaderName::from_static(REQUEST_ID_HEADER),
        HeaderValue::from_str(request_id.as_str()).expect("a request id is 16 hex characters"),
    );
    response
}

/// Emit the line. `tracing` bakes an event's level into a static callsite, so a
/// level chosen at runtime means one callsite per level — and one per shape,
/// since an absent `client_addr` has to be absent rather than `None`.
fn request_line(
    level: Level,
    method: &Method,
    path: &str,
    status: StatusCode,
    elapsed: Duration,
    client_addr: Option<IpAddr>,
) {
    let status = status.as_u16();
    // Microseconds, not milliseconds: a Pi answering a cached asset in 200 µs
    // must not read as `0`, and a slow request is just as visible either way.
    let duration_us = elapsed.as_micros() as u64;
    // `%path` rather than the default `Debug`: `path=/api/calls` greps, and
    // `path="/api/calls"` does not. Safe from log injection because hyper has
    // already rejected any request target containing a space or a control
    // character.
    macro_rules! emit_line {
        ($emit:ident) => {
            match client_addr {
                Some(client_addr) => {
                    tracing::$emit!(%method, %path, status, duration_us, %client_addr, "request")
                }
                None => tracing::$emit!(%method, %path, status, duration_us, "request"),
            }
        };
    }
    match level {
        Level::ERROR => emit_line!(error),
        Level::WARN => emit_line!(warn),
        Level::DEBUG => emit_line!(debug),
        // INFO is the resting level for everything not demoted; `level_for`
        // never chooses TRACE.
        _ => emit_line!(info),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::LogCapture;
    use proptest::prelude::*;
    use rstest::rstest;

    #[rstest]
    // The recorder-facing surface, both dialects (#5, #6).
    #[case("/api/call-upload", RouteClass::Ingest)]
    #[case("/api/trunk-recorder-call-upload", RouteClass::Ingest)]
    // Chatty: several range requests per Call, a dozen assets per page load, a
    // liveness probe every few seconds.
    #[case("/api/call/1/audio", RouteClass::Chatty)]
    #[case("/api/call/999999/audio", RouteClass::Chatty)]
    #[case("/healthz", RouteClass::Chatty)]
    #[case("/assets/index-a1b2c3.js", RouteClass::Chatty)]
    #[case("/assets/index-a1b2c3.css", RouteClass::Chatty)]
    #[case("/favicon.svg", RouteClass::Chatty)]
    #[case("/index.html", RouteClass::Chatty)]
    #[case("/manifest.webmanifest", RouteClass::Chatty)]
    // Client-side routes have no extension, so a navigation stays notable.
    #[case("/", RouteClass::Other)]
    #[case("/archive", RouteClass::Other)]
    #[case("/settings/talkgroups", RouteClass::Other)]
    // API traffic is never an asset, extension or not — `/api/` wins first.
    #[case("/api/calls", RouteClass::Other)]
    #[case("/api/calls/filters", RouteClass::Other)]
    #[case("/api/call/1/download", RouteClass::Other)]
    #[case("/api/live", RouteClass::Other)]
    #[case("/api/admin/talkgroups/import", RouteClass::Other)]
    #[case("/api/report.json", RouteClass::Other)]
    #[case("/api", RouteClass::Other)]
    // Near-misses that must not be mistaken for the real routes.
    #[case("/api/call-upload/extra", RouteClass::Other)]
    #[case("/api/call/1/audio/extra", RouteClass::Other)]
    #[case("/healthz/deep", RouteClass::Other)]
    fn paths_are_classified_by_how_often_they_arrive(
        #[case] path: &str,
        #[case] expected: RouteClass,
    ) {
        assert_eq!(RouteClass::of(path), expected, "path={path}");
    }

    #[rstest]
    // A resting level per class...
    #[case(RouteClass::Ingest, StatusCode::OK, Level::INFO)]
    #[case(RouteClass::Other, StatusCode::OK, Level::INFO)]
    #[case(RouteClass::Other, StatusCode::SWITCHING_PROTOCOLS, Level::INFO)]
    #[case(RouteClass::Other, StatusCode::TEMPORARY_REDIRECT, Level::INFO)]
    #[case(RouteClass::Chatty, StatusCode::OK, Level::DEBUG)]
    #[case(RouteClass::Chatty, StatusCode::PARTIAL_CONTENT, Level::DEBUG)]
    #[case(RouteClass::Chatty, StatusCode::NOT_MODIFIED, Level::DEBUG)]
    // ...escalated by the outcome, never demoted by the class. A probe that
    // 500s or an audio range that 416s is exactly what must not be quiet.
    #[case(RouteClass::Chatty, StatusCode::NOT_FOUND, Level::WARN)]
    #[case(RouteClass::Chatty, StatusCode::RANGE_NOT_SATISFIABLE, Level::WARN)]
    #[case(RouteClass::Chatty, StatusCode::INTERNAL_SERVER_ERROR, Level::ERROR)]
    #[case(RouteClass::Ingest, StatusCode::UNAUTHORIZED, Level::WARN)]
    #[case(RouteClass::Ingest, StatusCode::INTERNAL_SERVER_ERROR, Level::ERROR)]
    #[case(RouteClass::Other, StatusCode::BAD_REQUEST, Level::WARN)]
    #[case(RouteClass::Other, StatusCode::METHOD_NOT_ALLOWED, Level::WARN)]
    #[case(RouteClass::Other, StatusCode::BAD_GATEWAY, Level::ERROR)]
    fn level_is_the_louder_of_the_class_and_the_outcome(
        #[case] class: RouteClass,
        #[case] status: StatusCode,
        #[case] expected: Level,
    ) {
        assert_eq!(level_for(class, status), expected, "{class:?} {status}");
    }

    /// ADR-0011 rule 5. A recorder is the operator's own hardware and may be
    /// named at any level; a listener may be named only where someone has turned
    /// the verbosity up to look.
    #[rstest]
    #[case(RouteClass::Ingest, Level::INFO, true)]
    #[case(RouteClass::Ingest, Level::WARN, true)]
    #[case(RouteClass::Ingest, Level::ERROR, true)]
    #[case(RouteClass::Ingest, Level::DEBUG, true)]
    #[case(RouteClass::Chatty, Level::DEBUG, true)]
    #[case(RouteClass::Chatty, Level::TRACE, true)]
    #[case(RouteClass::Chatty, Level::WARN, false)]
    #[case(RouteClass::Chatty, Level::ERROR, false)]
    #[case(RouteClass::Other, Level::DEBUG, true)]
    #[case(RouteClass::Other, Level::INFO, false)]
    #[case(RouteClass::Other, Level::WARN, false)]
    #[case(RouteClass::Other, Level::ERROR, false)]
    fn only_a_recorder_or_a_debug_line_may_name_its_client(
        #[case] class: RouteClass,
        #[case] level: Level,
        #[case] expected: bool,
    ) {
        assert_eq!(
            logs_client_addr(class, level),
            expected,
            "{class:?} {level}"
        );
    }

    /// Every level and both shapes emit the same fields, because a line an
    /// operator only sees when something is wrong is the worst place to have a
    /// different format.
    #[rstest]
    #[case(Level::ERROR, " ERROR ")]
    #[case(Level::WARN, " WARN ")]
    #[case(Level::INFO, " INFO ")]
    #[case(Level::DEBUG, " DEBUG ")]
    #[case(Level::TRACE, " INFO ")] // never chosen by `level_for`; falls back
    fn a_line_carries_its_fields_at_every_level(#[case] level: Level, #[case] tag: &str) {
        let capture = LogCapture::start();
        request_line(
            level,
            &Method::POST,
            "/api/call-upload",
            StatusCode::OK,
            Duration::from_micros(1234),
            Some("10.0.0.7".parse().expect("ip")),
        );
        request_line(
            level,
            &Method::GET,
            "/api/calls",
            StatusCode::NOT_FOUND,
            Duration::from_micros(5),
            None,
        );

        let logged = capture.text();
        let mut lines = logged.lines();
        let with = lines.next().expect("a line with a client address");
        let without = lines.next().expect("a line without one");

        assert!(with.contains(tag), "{with}");
        assert!(with.contains("method=POST"), "{with}");
        assert!(with.contains("path=/api/call-upload"), "{with}");
        assert!(with.contains("status=200"), "{with}");
        assert!(with.contains("duration_us=1234"), "{with}");
        assert!(with.contains("client_addr=10.0.0.7"), "{with}");

        assert!(without.contains(tag), "{without}");
        assert!(without.contains("method=GET"), "{without}");
        assert!(without.contains("path=/api/calls"), "{without}");
        assert!(without.contains("status=404"), "{without}");
        assert!(without.contains("duration_us=5"), "{without}");
        // Absent, not `None`: an assertion about who was *not* recorded has to
        // be able to look for the field and find nothing.
        assert!(!without.contains("client_addr"), "{without}");
    }

    #[test]
    fn a_request_id_is_16_hex_characters_and_a_fresh_one_each_time() {
        let id = RequestId::new();
        assert_eq!(id.as_str().len(), 16, "{id}");
        assert!(id.as_str().chars().all(|c| c.is_ascii_hexdigit()), "{id}");
        assert_eq!(id.to_string(), id.as_str(), "Display is the id itself");

        let ids: std::collections::HashSet<String> =
            (0..1_000).map(|_| RequestId::new().0).collect();
        assert_eq!(ids.len(), 1_000, "ids must not repeat");
    }

    proptest! {
        /// Whatever a client asks for — and a request target is close to
        /// arbitrary bytes — classification is total and the API namespace is
        /// never mistaken for a static asset.
        #[test]
        fn any_path_classifies_without_the_api_namespace_leaking_into_assets(
            path in "[ -~]{0,40}",
        ) {
            let class = RouteClass::of(&path);
            if path.starts_with("/api/") {
                prop_assert!(!is_spa_asset(&path), "{path}");
            }
            // Only the two upload endpoints are ever ingest, whatever else the
            // path looks like. Rule 5's exemption is scoped to those two paths
            // and cannot be widened by anything a client asks for. (Anyone who
            // posts to them is named, authenticated or not — an unauthorized
            // upload is precisely when *which host sent this* matters.)
            prop_assert_eq!(
                class == RouteClass::Ingest,
                path == "/api/call-upload" || path == "/api/trunk-recorder-call-upload",
                "{}", path
            );
        }

        /// The escalation is absolute: no class ever quiets a failing outcome
        /// below the level its status earns.
        #[test]
        fn a_failing_status_is_never_demoted_by_its_class(code in 400u16..600) {
            let status = StatusCode::from_u16(code).expect("status");
            for class in [RouteClass::Ingest, RouteClass::Chatty, RouteClass::Other] {
                let level = level_for(class, status);
                prop_assert_eq!(
                    level,
                    if code < 500 { Level::WARN } else { Level::ERROR },
                    "{:?} {}", class, status
                );
                // ...and a failing listener request never carries an address.
                prop_assert_eq!(
                    logs_client_addr(class, level),
                    class == RouteClass::Ingest,
                    "{:?} {}", class, status
                );
            }
        }
    }
}
