//! **The LAN door**: what `[server] port` does once built-in TLS is on (#76).
//!
//! Plain HTTP does not go away when HTTPS arrives, because three things still
//! need it. The CA's **HTTP-01 challenge** is fetched over plain HTTP from port
//! 80 by definition. A **Trunk Recorder on the same Pi** posts to
//! `http://127.0.0.1:3000` today, and pointing it at the public name instead
//! needs hairpin NAT that many home routers do not do — the maintainer's own
//! scanner is that setup. And a **Listener on the LAN** reaches the box by its
//! local address, for the same reason.
//!
//! So the plain port answers by *who is asking*: a challenge to anyone, the
//! whole app to a peer on this machine or the local network, and a redirect to
//! HTTPS to everyone else. A VPS with TLS on therefore exposes nothing over
//! plain HTTP, and a home scanner changes nothing for the devices already
//! talking to it.
//!
//! **The peer is the TCP peer, never a forwarded address.** A tunnel that
//! relays from loopback is serving a public visitor, and what it relays is
//! already HTTPS at its edge — redirecting it would send the visitor in a
//! circle. Whoever can open a socket from the LAN is, for this decision, on the
//! LAN — and so is a stranger whose connection arrives *through* something that
//! rewrites its source to a private address. Rootless Docker and Docker Desktop
//! both do that to every published port, which is why `docs/deploy.md` says to
//! publish the plain port to the internet only where Docker hands the container
//! the real address.
//!
//! rdio-scanner keeps serving the whole app on its plain port to everyone once
//! `ssl_listen` is set, admin password and all.

use std::net::IpAddr;

/// Where an ACME HTTP-01 challenge is fetched from (RFC 8555 §8.3).
pub const CHALLENGE_PREFIX: &str = "/.well-known/acme-challenge/";

/// What the plain port does with one request, once TLS is on.
#[derive(Debug, PartialEq, Eq)]
pub enum Door<'a> {
    /// The CA asking for the key authorization behind this token — answered to
    /// anyone, because the CA is on the internet.
    Challenge(&'a str),
    /// A peer on this machine or the local network: the whole app.
    Serve,
    /// Anyone else: go to HTTPS.
    Redirect,
}

/// **The whole of the LAN door's decision**, as a value.
pub fn door(peer: IpAddr, path: &str) -> Door<'_> {
    if let Some(token) = path.strip_prefix(CHALLENGE_PREFIX)
        && !token.is_empty()
        && !token.contains('/')
    {
        return Door::Challenge(token);
    }
    match is_local(peer) {
        true => Door::Serve,
        false => Door::Redirect,
    }
}

/// Whether `peer` is this machine or the network it sits on.
///
/// The private ranges a home router hands out, link-local, and **CGNAT's
/// `100.64.0.0/10`** — which is where Tailscale numbers a tailnet, and a
/// tailnet is a LAN its Operator built on purpose. It is also ISP carrier-NAT
/// space, where some ISPs route customers to one another; kept by the
/// maintainer's call in #76's review, and said in `docs/deploy.md`. An IPv4 peer reaching an
/// IPv6 socket arrives mapped (`::ffff:192.168.1.5`) and is judged as the
/// address it is.
pub fn is_local(peer: IpAddr) -> bool {
    match peer {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_local(IpAddr::V4(v4)),
            None => {
                let first = v6.segments()[0];
                v6.is_loopback()
                    // Unique local, fc00::/7 — IPv6's private range.
                    || (first & 0xfe00) == 0xfc00
                    // Link-local, fe80::/10.
                    || (first & 0xffc0) == 0xfe80
            }
        },
    }
}

/// Where a redirected request goes: the same path and query, over HTTPS.
///
/// The base is `public_url` when it is an `https://` one — with ACME on that is
/// the certificate's own name (`Config::public_url`) — and otherwise the host
/// the request asked for, with `https_port` unless it is 443. `None` when there
/// is neither, which is a request with no `Host` header: nothing to send it to.
pub fn redirect_to(
    public_url: Option<&str>,
    host: Option<&str>,
    https_port: u16,
    path_and_query: &str,
) -> Option<String> {
    if let Some(base) = public_url.filter(|url| url.starts_with("https://")) {
        return Some(format!("{}{path_and_query}", base.trim_end_matches('/')));
    }
    let authority: http::uri::Authority = host?.parse().ok()?;
    let port = match https_port {
        443 => String::new(),
        port => format!(":{port}"),
    };
    Some(format!(
        "https://{}{port}{path_and_query}",
        authority.host()
    ))
}

/// The LAN door as middleware on the plain listener — [`door`], performed.
///
/// Layered inside the request log, so a redirected stranger and an answered
/// challenge are each one line like any other request (#28).
pub async fn lan_door(
    axum::extract::State(tls): axum::extract::State<super::Tls>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    match door(peer.ip(), request.uri().path()) {
        Door::Serve => next.run(request).await,
        Door::Challenge(token) => match tls.http_challenge(token) {
            Some(key_authorization) => (
                [(http::header::CONTENT_TYPE, "application/octet-stream")],
                key_authorization,
            )
                .into_response(),
            None => crate::failure::Reason::ChallengeNotFound.into_response(),
        },
        Door::Redirect => {
            let host = request
                .headers()
                .get(http::header::HOST)
                .and_then(|value| value.to_str().ok());
            let path_and_query = request
                .uri()
                .path_and_query()
                .map_or("/", |path| path.as_str());
            match redirect_to(tls.public_url(), host, tls.https_port(), path_and_query) {
                // 307 rather than 308: a permanent redirect is cached by a
                // browser for good, and an Operator who later turns built-in
                // TLS off would find the plain address unreachable from every
                // browser that ever saw one — HSTS's stickiness, which this
                // module declines for the same reason. Method-preserving
                // either way, so a public Recorder's POST follows it intact.
                Some(location) => (
                    http::StatusCode::TEMPORARY_REDIRECT,
                    [(http::header::LOCATION, location)],
                )
                    .into_response(),
                None => crate::failure::Reason::NoHostToRedirectTo.into_response(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("an address")
    }

    /// Who counts as local: this machine, the ranges a home router hands out,
    /// link-local, and a tailnet — and nobody on the internet, including the
    /// addresses that sit just outside each range.
    #[rstest]
    #[case::loopback("127.0.0.1", true)]
    #[case::all_of_loopback("127.8.9.10", true)]
    #[case::ten("10.1.2.3", true)]
    #[case::one_seven_two("172.16.0.1", true)]
    #[case::one_seven_two_top("172.31.255.255", true)]
    #[case::just_past_one_seven_two("172.32.0.1", false)]
    #[case::one_nine_two("192.168.1.20", true)]
    #[case::link_local("169.254.10.1", true)]
    #[case::tailscale("100.101.102.103", true)]
    #[case::tailscale_bottom("100.64.0.0", true)]
    #[case::tailscale_top("100.127.255.255", true)]
    #[case::just_below_cgnat("100.63.255.255", false)]
    #[case::just_above_cgnat("100.128.0.0", false)]
    #[case::the_internet("203.0.113.9", false)]
    #[case::v6_loopback("::1", true)]
    #[case::v6_unique_local("fd12:3456::1", true)]
    #[case::v6_unique_local_fc("fc00::1", true)]
    #[case::v6_link_local("fe80::1", true)]
    #[case::v6_link_local_top("febf::1", true)]
    #[case::just_past_link_local("fec0::1", false)]
    #[case::v6_the_internet("2001:db8::1", false)]
    #[case::mapped_lan("::ffff:192.168.1.5", true)]
    #[case::mapped_internet("::ffff:203.0.113.9", false)]
    fn who_is_on_the_lan(#[case] peer: &str, #[case] local: bool) {
        assert_eq!(is_local(ip(peer)), local, "{peer}");
    }

    /// The door: a challenge for anyone, the app for the LAN, a redirect for
    /// the internet.
    #[rstest]
    #[case::the_ca(
        "203.0.113.9",
        "/.well-known/acme-challenge/abc_DEF-1",
        Door::Challenge("abc_DEF-1")
    )]
    #[case::a_challenge_from_the_lan(
        "192.168.1.5",
        "/.well-known/acme-challenge/tok",
        Door::Challenge("tok")
    )]
    #[case::a_recorder_on_the_box("127.0.0.1", "/api/call-upload", Door::Serve)]
    #[case::a_listener_on_the_lan("192.168.1.5", "/", Door::Serve)]
    #[case::a_stranger("203.0.113.9", "/", Door::Redirect)]
    #[case::a_stranger_at_admin("203.0.113.9", "/api/admin/login", Door::Redirect)]
    // Not a challenge: no token, or a path that is deeper than one.
    #[case::no_token("203.0.113.9", "/.well-known/acme-challenge/", Door::Redirect)]
    #[case::deeper("203.0.113.9", "/.well-known/acme-challenge/a/b", Door::Redirect)]
    #[case::a_prefix_lookalike("203.0.113.9", "/.well-known/acme-challengex", Door::Redirect)]
    fn the_door_decides_by_path_then_peer(
        #[case] peer: &str,
        #[case] path: &str,
        #[case] expected: Door<'_>,
    ) {
        assert_eq!(door(ip(peer), path), expected);
    }

    /// Where a redirect goes: the certificate's own name when there is one, the
    /// host that was asked for otherwise, and always the path and query that
    /// were asked for.
    #[rstest]
    #[case::the_public_url(
        Some("https://scanner.example"),
        Some("1.2.3.4:3000"),
        443,
        "/a?b=c",
        Some("https://scanner.example/a?b=c")
    )]
    #[case::a_trailing_slash(
        Some("https://scanner.example/"),
        None,
        443,
        "/",
        Some("https://scanner.example/")
    )]
    #[case::the_host_asked_for(
        None,
        Some("scanner.example"),
        443,
        "/x",
        Some("https://scanner.example/x")
    )]
    #[case::the_host_without_its_plain_port(
        None,
        Some("scanner.example:3000"),
        443,
        "/x",
        Some("https://scanner.example/x")
    )]
    #[case::a_non_standard_https_port(
        None,
        Some("scanner.example:3000"),
        8443,
        "/x",
        Some("https://scanner.example:8443/x")
    )]
    #[case::an_ipv6_host(
        None,
        Some("[2001:db8::1]:3000"),
        443,
        "/",
        Some("https://[2001:db8::1]/")
    )]
    // A plain public_url would redirect HTTPS-bound traffic back to plain HTTP,
    // so it is passed over for the host.
    #[case::a_plain_public_url(
        Some("http://scanner.example"),
        Some("scanner.example"),
        443,
        "/",
        Some("https://scanner.example/")
    )]
    #[case::nowhere_to_go(None, None, 443, "/", None)]
    #[case::a_host_that_is_not_one(None, Some("not a host"), 443, "/", None)]
    fn a_redirect_keeps_the_path_and_finds_a_host(
        #[case] public_url: Option<&str>,
        #[case] host: Option<&str>,
        #[case] https_port: u16,
        #[case] path_and_query: &str,
        #[case] expected: Option<&str>,
    ) {
        assert_eq!(
            redirect_to(public_url, host, https_port, path_and_query).as_deref(),
            expected
        );
    }

    /// The door as the plain listener runs it, in front of a stand-in app —
    /// asked by `peer` for `uri`, with `host` as its `Host` header.
    ///
    /// Through a router rather than over a socket because the case worth
    /// proving is a **public** peer, and every peer the suite can open a socket
    /// from is loopback. `ConnectInfo` is exactly what `axum::serve` would have
    /// put on the request.
    async fn through_the_door(
        tls: super::super::Tls,
        peer: &str,
        uri: &str,
        host: Option<&str>,
    ) -> axum::response::Response {
        use tower::ServiceExt;

        let app = axum::Router::new()
            .fallback(|| async { "the app" })
            .layer(axum::middleware::from_fn_with_state(tls, lan_door));
        let mut request = axum::extract::Request::builder().uri(uri);
        if let Some(host) = host {
            request = request.header(http::header::HOST, host);
        }
        let mut request = request.body(axum::body::Body::empty()).expect("a request");
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::new(
                ip(peer),
                50_000,
            )));
        app.oneshot(request).await.expect("an answer")
    }

    async fn body_of(response: axum::response::Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("a body");
        String::from_utf8(bytes.to_vec()).expect("utf-8")
    }

    fn files_tls(public_url: Option<&str>) -> super::super::Tls {
        let config = super::super::TlsConfig {
            cert_file: Some("/c.pem".into()),
            key_file: Some("/k.pem".into()),
            ..Default::default()
        };
        let tls = super::super::Tls::new(config, Default::default(), public_url.map(Into::into));
        tls.bound(8443);
        tls
    }

    /// A stranger is sent to HTTPS — at the public name when there is one,
    /// keeping the path and query, with a redirect a browser does not cache
    /// for good.
    #[tokio::test]
    async fn a_stranger_is_sent_to_https() {
        let response = through_the_door(
            files_tls(Some("https://scanner.example")),
            "203.0.113.9",
            "/archive?talkgroup=7",
            Some("1.2.3.4:3000"),
        )
        .await;

        assert_eq!(response.status(), 307);
        assert_eq!(
            response.headers()[http::header::LOCATION],
            "https://scanner.example/archive?talkgroup=7"
        );
    }

    /// ...or at the host they asked for, on the port HTTPS actually bound.
    #[tokio::test]
    async fn without_a_public_url_the_redirect_names_the_host_asked_for() {
        let response =
            through_the_door(files_tls(None), "203.0.113.9", "/", Some("scanner.example")).await;

        assert_eq!(
            response.headers()[http::header::LOCATION],
            "https://scanner.example:8443/"
        );
    }

    /// With neither, there is nowhere to send them — and the plain app is not
    /// theirs, so they are refused rather than served.
    #[tokio::test]
    async fn a_stranger_with_nowhere_to_go_is_refused_not_served() {
        let response = through_the_door(files_tls(None), "203.0.113.9", "/", None).await;

        assert_eq!(response.status(), 400);
    }

    /// The LAN is served the app, unchanged.
    #[tokio::test]
    async fn a_neighbour_is_served_the_app() {
        let response =
            through_the_door(files_tls(None), "192.168.1.5", "/", Some("pi.local:3000")).await;

        assert_eq!(response.status(), 200);
        assert_eq!(body_of(response).await, "the app");
    }

    /// The CA is shown the key authorization it is owed, from anywhere —
    /// exactly, as an octet stream (RFC 8555 §8.3).
    #[tokio::test]
    async fn the_ca_is_shown_the_key_authorization() {
        let tls = files_tls(None);
        tls.offer_http_challenge("tok", "tok.thumbprint".into());

        let response = through_the_door(
            tls,
            "203.0.113.9",
            "/.well-known/acme-challenge/tok",
            Some("scanner.example"),
        )
        .await;

        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()[http::header::CONTENT_TYPE],
            "application/octet-stream"
        );
        assert_eq!(body_of(response).await, "tok.thumbprint");
    }

    proptest! {
        /// Whatever the path, a public peer is never served and a local one is
        /// never redirected — the challenge is the only thing they share.
        #[test]
        fn a_public_peer_is_never_served_the_app(path in "/[ -~]{0,40}", last in 0u8..=255) {
            let stranger = IpAddr::from([203, 0, 113, last]);
            let neighbour = IpAddr::from([192, 168, 1, last]);
            prop_assert_ne!(door(stranger, &path), Door::Serve);
            prop_assert_ne!(door(neighbour, &path), Door::Redirect);
        }
    }
}
