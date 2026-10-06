//! **Who may put this Instance in a frame** (#75, ADR-0008).
//!
//! One page may be framed by anybody: the **Embed** page, which exists to be
//! framed by a fire department's homepage or a newsroom's. Everything else may
//! be framed by nobody, and says so on every response — the app (which carries
//! the admin surface and the unlock form), a share page, an Event's page, the
//! API, a 404.
//!
//! # Why deny the rest, when the admin cookie is already `SameSite=Strict`
//!
//! A cross-site frame of the admin surface already has no session — the cookie
//! is `Strict` (ADR-0008), so a browser neither sends it into a third-party
//! frame nor accepts one set from inside one — and a frame of the Listener app
//! has no grant either, since a third-party frame's storage is partitioned. So
//! this closes no hole that is open today. It is the layer that does not depend
//! on a browser's cookie and storage rules to hold, which is what
//! clickjacking defence is supposed to be, and it makes the embed route the
//! *one* framable surface by construction rather than by everything else
//! happening to be unusable in a frame.
//!
//! The cost is real and was taken knowingly: rdio-scanner sends no framing
//! header at all, so an Operator can iframe the whole app — into a Home
//! Assistant dashboard, say. That stops working here; the Embed is what to frame
//! instead (maintainer's call, grilling #75).
//!
//! # The shape
//!
//! A handler opts in by putting [`Framable`] on its response, as an extension;
//! one `map_response` layer over the whole router reads it and writes the
//! headers. So the decision is made by the handler that knows it is the embed,
//! and *saying it* is not something any other handler can forget to do — a route
//! added tomorrow is unframable until it asks to be otherwise.
//!
//! Both headers on a denial: `Content-Security-Policy: frame-ancestors 'none'`
//! is what a current browser reads, and `X-Frame-Options: DENY` is what an old
//! one does (a browser that understands the first ignores the second). The
//! permission is explicit too — `frame-ancestors *` — rather than left to the
//! absence of a header, so "this page may be framed" is a fact a test reads off
//! the wire rather than an inference from silence.
//!
//! **These are the only `Content-Security-Policy` this Instance sends.** A later
//! policy with other directives must be composed with this one here rather than
//! set beside it, or the second header to be written wins.

use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;

/// Put on a response — `(Extension(Framable), page)` — to let any site frame
/// it. The embed page's, and nothing else's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Framable;

/// Who may frame one response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// Any site at all.
    Anywhere,
    /// No site, this one included.
    Nowhere,
}

impl Framing {
    /// The decision, from whether the handler asked for it.
    pub fn of(response: &Response) -> Framing {
        match response.extensions().get::<Framable>() {
            Some(Framable) => Framing::Anywhere,
            None => Framing::Nowhere,
        }
    }

    /// Say it on the wire.
    fn write(self, headers: &mut HeaderMap) {
        match self {
            Framing::Anywhere => {
                headers.insert(
                    header::CONTENT_SECURITY_POLICY,
                    HeaderValue::from_static("frame-ancestors *"),
                );
            }
            Framing::Nowhere => {
                headers.insert(
                    header::CONTENT_SECURITY_POLICY,
                    HeaderValue::from_static("frame-ancestors 'none'"),
                );
                headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
            }
        }
    }
}

/// The layer: every response leaves with its framing said — `map_response`, so
/// it sees the router's own 404s and 405s as well as every handler's answer.
///
/// **Outermost**, over the request log as well: that is where a 5xx is rebuilt
/// to carry only its request id (`failure::redact`), and a header this wrote
/// underneath it would leave with the body it replaced.
/// `tests/embed.rs::a_failure_may_not_be_framed_either` is what would notice.
pub async fn apply(mut response: Response) -> Response {
    Framing::of(&response).write(response.headers_mut());
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Extension;
    use axum::response::IntoResponse;

    fn headers_of(response: &Response) -> (Option<&str>, Option<&str>) {
        (
            response
                .headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .and_then(|value| value.to_str().ok()),
            response
                .headers()
                .get(header::X_FRAME_OPTIONS)
                .and_then(|value| value.to_str().ok()),
        )
    }

    /// A response nobody said anything about may be framed by nobody.
    #[tokio::test]
    async fn an_ordinary_response_may_not_be_framed() {
        let response = apply("hello".into_response()).await;

        assert_eq!(
            headers_of(&response),
            (Some("frame-ancestors 'none'"), Some("DENY"))
        );
    }

    /// The marker is the whole of opting in — and the permission is said, not
    /// implied by a missing header.
    #[tokio::test]
    async fn a_framable_response_may_be_framed_anywhere() {
        let response = apply((Extension(Framable), "embed").into_response()).await;

        assert_eq!(Framing::of(&response), Framing::Anywhere);
        assert_eq!(headers_of(&response), (Some("frame-ancestors *"), None));
    }

    /// A handler's own framing header is overwritten, not doubled: the policy is
    /// this module's, and two `frame-ancestors` would be read as the stricter.
    #[tokio::test]
    async fn the_layer_owns_the_header() {
        let response = (
            [(header::CONTENT_SECURITY_POLICY, "frame-ancestors https://x")],
            "hello",
        )
            .into_response();

        let response = apply(response).await;

        assert_eq!(
            response
                .headers()
                .get_all(header::CONTENT_SECURITY_POLICY)
                .iter()
                .count(),
            1
        );
        assert_eq!(
            headers_of(&response),
            (Some("frame-ancestors 'none'"), Some("DENY"))
        );
    }
}
