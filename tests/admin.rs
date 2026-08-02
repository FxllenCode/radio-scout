//! Admin auth over real HTTP (#19, ADR-0008): the cookie session, the CSRF
//! token that rides with it, and the brute-force lockout in front of both.

mod common;

use common::logs::LogCapture;
use common::{ADMIN_PASSWORD, TestApp, header_of};
use radio_scout::db::entities::talkgroup;
use rstest::rstest;
use serde_json::json;

/// A CSV the importer would happily apply — the payload that must not land
/// without a session.
const CSV: &str = "system,ref,label\n1,100,Fire Dispatch\n";

/// The way in. A correct password opens a session, and the cookie carrying it
/// has the attributes that make it a session cookie rather than a token
/// anything on the page can read.
#[tokio::test]
async fn a_correct_password_opens_a_session() {
    let app = TestApp::spawn().await;

    let response = app
        .post_json("/api/admin/login", json!({ "password": ADMIN_PASSWORD }))
        .await;

    assert_eq!(response.status(), 200);
    let cookie = header_of(&response, "set-cookie").expect("a session cookie");
    assert!(cookie.starts_with("rs_session="), "{cookie}");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Strict"), "{cookie}");
    assert!(cookie.contains("Path=/"), "{cookie}");
    assert!(
        !cookie.contains("Secure"),
        "a Secure cookie is dropped by the browser over plain HTTP, which is \
         what zero-config ships (ADR-0008): {cookie}"
    );
}

/// `Secure` when — and only when — the request actually came over TLS.
///
/// v1 serves plain HTTP and recommends a reverse proxy for TLS (ADR-0008), so a
/// cookie marked `Secure` unconditionally would be dropped by the browser on
/// every zero-config LAN install and make admin login impossible. The flag
/// therefore follows `X-Forwarded-Proto` — believed on exactly the terms #17
/// already set for `X-Forwarded-For`, since a header nobody vouched for is
/// attacker-controlled.
#[rstest]
// Behind a proxy the operator named, over TLS: the flag rides.
#[case(Some("127.0.0.1"), Some("https"), true)]
// The same claim from a peer nobody vouched for is not believed — the default
// trust list is empty, which is what ships.
#[case(None, Some("https"), false)]
// A trusted proxy that terminated plain HTTP says so, and is believed.
#[case(Some("127.0.0.1"), Some("http"), false)]
// No claim at all: a direct plain-HTTP request.
#[case(Some("127.0.0.1"), None, false)]
#[case(None, None, false)]
#[tokio::test]
async fn the_secure_flag_follows_a_trusted_proxys_protocol(
    #[case] trusted: Option<&str>,
    #[case] proto: Option<&str>,
    #[case] expected: bool,
) {
    let mut builder = TestApp::builder();
    if let Some(trusted) = trusted {
        builder = builder.trusted_proxies(trusted);
    }
    let app = builder.spawn().await;

    let mut request = app.login_request(ADMIN_PASSWORD);
    if let Some(proto) = proto {
        request = request.header("x-forwarded-proto", proto);
    }
    let response = request.send().await.expect("POST");

    let cookie = header_of(&response, "set-cookie").expect("a session cookie");
    assert_eq!(
        cookie.contains("Secure"),
        expected,
        "trusted={trusted:?} proto={proto:?} cookie={cookie}"
    );
}

/// The gap this ticket closes. #18's Talkgroup importer shipped before the
/// session existed, so anyone who could reach the server could rewrite an
/// operator's Talkgroup curation. It answers 401 now — and, because the guard
/// is a layer over the whole prefix, so does every route added beside it.
#[tokio::test]
async fn the_admin_surface_is_closed_without_a_session() {
    let app = TestApp::spawn().await;

    let (status, body) = app
        .post_bytes(
            "/api/admin/talkgroups/import",
            "text/csv",
            CSV.as_bytes().to_vec(),
        )
        .await;

    assert_eq!(status, 401, "{body}");
    assert_eq!(
        app.count::<talkgroup::Entity>().await,
        0,
        "a rejected request must not have run the handler"
    );
}

/// ...and opens with one.
#[tokio::test]
async fn a_session_opens_the_admin_surface() {
    let app = TestApp::spawn().await;
    app.login().await;

    let (status, body) = app
        .post_admin_bytes(
            "/api/admin/talkgroups/import",
            "text/csv",
            CSV.as_bytes().to_vec(),
        )
        .await;

    assert_eq!(status, 200, "{body}");
    assert_eq!(app.count::<talkgroup::Entity>().await, 1);
}

/// A session cookie alone is not enough to *change* anything: the browser
/// attaches cookies to a cross-site request whether or not the page meant to
/// send it, so an unsafe method must also echo the token only this origin's
/// script can have read. `SameSite=Strict` is the first line of that defence;
/// this is the one that does not depend on the browser.
#[rstest]
// No token at all — a form posted from another site.
#[case(None)]
// A token from somewhere else. It is 64 hex characters, so it fails on its
// value rather than on its shape.
#[case(Some("a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90"))]
// The right shape, empty.
#[case(Some(""))]
#[tokio::test]
async fn a_state_changing_request_without_the_csrf_token_is_refused(#[case] csrf: Option<&str>) {
    let app = TestApp::spawn().await;
    app.login().await;

    let response = app
        .admin_request("/api/admin/talkgroups/import", csrf)
        .header("content-type", "text/csv")
        .body(CSV)
        .send()
        .await
        .expect("POST");

    assert_eq!(response.status(), 403, "csrf={csrf:?}");
    assert_eq!(
        app.count::<talkgroup::Entity>().await,
        0,
        "a refused request must not have run the handler"
    );
}

/// One session's token is no use against another's. Two operators (or two tabs
/// that each logged in) must not be able to act for each other.
#[tokio::test]
async fn one_sessions_csrf_token_does_not_work_on_another() {
    let app = TestApp::spawn().await;
    let first = app.login().await;
    // A second login on the same handle replaces the cookie jar's session.
    let second = app.login().await;
    assert_ne!(first, second, "each session gets its own token");

    let response = app
        .admin_request("/api/admin/talkgroups/import", Some(&first))
        .header("content-type", "text/csv")
        .body(CSV)
        .send()
        .await
        .expect("POST");

    assert_eq!(response.status(), 403);
}

/// Logging out revokes the session **on the server**, which is the whole reason
/// ADR-0008 chose server-side state over a JWT: a token the client merely
/// forgets is still valid to anyone who kept a copy. So this keeps presenting
/// the cookie after the browser has been told to drop it.
#[tokio::test]
async fn logging_out_revokes_the_session_server_side() {
    let app = TestApp::spawn().await;
    app.login().await;
    let stolen = app.session_cookie();

    let response = app.post_admin("/api/admin/logout").await;

    assert_eq!(response.status(), 204);
    let cleared = header_of(&response, "set-cookie").expect("a cleared cookie");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");

    let replayed = app
        .client()
        .get(app.url("/api/admin/session"))
        .header("cookie", &stolen)
        .send()
        .await
        .expect("GET");
    assert_eq!(
        replayed.status(),
        401,
        "a revoked session must not come back with the cookie"
    );
}

/// An app that locks an address out after three wrong passwords, so the test
/// spends three Argon2 verifications rather than the shipped five.
async fn app_locking_after_three() -> TestApp {
    TestApp::builder()
        .config(|config| config.admin.lockout_attempts = 3)
        .spawn()
        .await
}

/// Guessing costs the guesser. After the budget is spent the address is locked
/// out — and locked out **even from the right password**, or the guard would do
/// nothing to slow the guess that eventually lands.
///
/// It answers 429 with `Retry-After`, not the 401 rdio-scanner uses
/// (`admin.go:407`): an operator who has locked themselves out needs to be able
/// to tell "wrong password" from "stop trying for a while", and rdio's two
/// outcomes are byte-identical.
#[tokio::test]
async fn a_spent_budget_locks_the_address_out_even_from_the_right_password() {
    let app = app_locking_after_three().await;

    for attempt in 1..=3 {
        let response = app.login_as("wrong").await;
        assert_eq!(response.status(), 401, "attempt {attempt}");
    }

    let response = app.login_as(ADMIN_PASSWORD).await;
    assert_eq!(response.status(), 429);
    let retry_after: u64 = header_of(&response, "retry-after")
        .expect("a Retry-After")
        .parse()
        .expect("a number of seconds");
    assert!(retry_after > 0, "retry_after={retry_after}");
}

/// The lockout counts the address the *network* established, not the one the
/// request claims. rdio-scanner keys its attempt ledger on `GetRemoteAddr`,
/// which reads `X-Forwarded-For` unconditionally (`main.go:265`) — so on a
/// public instance an attacker rotates the header, is never locked out, and can
/// lock anyone else out by forging their address. With no trusted proxies
/// configured — what ships — the header is not read at all.
#[tokio::test]
async fn a_forged_forwarded_for_does_not_buy_a_fresh_budget() {
    let app = app_locking_after_three().await;

    for guess in 0..3 {
        let response = app
            .login_request("wrong")
            .header("x-forwarded-for", format!("10.0.0.{guess}"))
            .send()
            .await
            .expect("POST");
        assert_eq!(response.status(), 401, "guess {guess}");
    }

    let response = app
        .login_request("wrong")
        .header("x-forwarded-for", "10.0.0.99")
        .send()
        .await
        .expect("POST");
    assert_eq!(
        response.status(),
        429,
        "a fourth guess from a fourth claimed address is still the same peer"
    );
}

/// The ledger only says it is full when it is (#83).
///
/// `Lockout::forget_spent` ends with an unconditional WARN, reached only when
/// the early return above it did not fire — that is, only after the trim loop
/// actually evicted something. The guard and the warning are therefore one
/// mechanism, and #83's inventory got this wrong: it filed the guard's `<` as a
/// "known equivalent, do not chase", when flipping it makes an ordinary ledger
/// fall straight through to the warning and cry full on **every failed login**.
///
/// A WARN is where ADR-0011 rule 7 puts "an operator must act", so a false one
/// on the admin surface is the kind that teaches an operator to ignore the log
/// — and the log is the only place a real password-guessing attempt shows up.
#[tokio::test]
async fn an_ordinary_failed_login_does_not_claim_the_ledger_is_full() {
    let app = app_locking_after_three().await;
    let capture = LogCapture::start();

    assert_eq!(app.login_as("wrong").await.status(), 401);

    capture.assert_never_logged("ledger is full");
}

/// Getting in clears the address's record. rdio clears the *whole* ledger on any
/// successful login (`admin.go:454`), so one operator logging in hands every
/// attacker a fresh budget.
#[tokio::test]
async fn a_successful_login_clears_the_addresss_failures() {
    let app = app_locking_after_three().await;

    for _ in 0..2 {
        assert_eq!(app.login_as("wrong").await.status(), 401);
    }
    assert_eq!(app.login_as(ADMIN_PASSWORD).await.status(), 200);

    // The budget is whole again: two more wrong guesses would have locked it.
    for attempt in 1..=2 {
        assert_eq!(
            app.login_as("wrong").await.status(),
            401,
            "attempt {attempt}"
        );
    }
}

/// Every refused login says why, and says where from.
///
/// ADR-0011 rule 5 keeps listener addresses out of the log; an authentication
/// *attempt* is the third class the rule names, because "someone is guessing my
/// admin password" cannot be acted on without an address to put in a firewall
/// rule — and it is not a record of who listened. The password itself never
/// appears at any level (rule 2), which is the assertion that matters most
/// here: a wrong guess is very often a *right* password for something else.
#[tokio::test]
async fn a_refused_login_logs_its_reason_and_its_source_but_never_the_password() {
    let app = app_locking_after_three().await;
    let capture = LogCapture::start();

    for _ in 0..3 {
        app.login_as("hunter2").await;
    }
    // ...and one more, now that the address is spent.
    app.login_as("hunter2").await;

    // `reason=invalid-password` unquoted — the spelling that greps, which
    // since #92 is the only one there is (ADR-0011 rule 6).
    let refusals = capture.lines_containing("reason=invalid-password");
    assert_eq!(refusals.len(), 3, "one line per wrong guess: {refusals:#?}");
    for (guess, line) in refusals.iter().enumerate() {
        assert!(line.contains(" WARN "), "{line}");
        assert!(line.contains("client_addr=127.0.0.1"), "{line}");
        // The running count is what turns a stray typo into a visible attack.
        assert!(line.contains(&format!("failures={}", guess + 1)), "{line}");
    }

    let locked = capture.only_line_containing("reason=admin-locked-out");
    assert!(locked.contains(" WARN "), "{locked}");
    assert!(locked.contains("client_addr=127.0.0.1"), "{locked}");
    assert!(locked.contains("retry_after_secs="), "{locked}");

    capture.assert_never_logged("hunter2");
    capture.assert_never_logged(ADMIN_PASSWORD);
}

/// A safe method needs no CSRF token — there is nothing to forge into. It is
/// how a reloaded page finds out it is still logged in, and gets back the token
/// it needs for the next thing it changes.
#[tokio::test]
async fn the_session_route_reports_the_live_session_without_a_csrf_token() {
    let app = TestApp::spawn().await;
    let csrf = app.login().await;

    let body = app.get_json("/api/admin/session").await;

    assert_eq!(body["csrf_token"], csrf);
    assert!(
        body["expires_in_secs"].as_u64().expect("a number") > 0,
        "{body}"
    );
}
