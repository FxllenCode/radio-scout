//! **Per-Call public share links**, driven end to end (#64, spec US 32).
//!
//! What a share link *says* is unit-tested where it belongs, in
//! `src/share/page.rs`, against a `StoredCall` a test constructs — every OG tag,
//! every escape, every mark, and the three ways of being dead. What is here is
//! everything that can only be said about a **running Instance**:
//!
//! - that a minted link really plays its Call, over real HTTP, with real bytes
//!   and real range requests — which is the whole promise, and the half no pure
//!   test can make
//! - that a second mint hands back the *same* link, which is the abuse bound
//! - that nothing else on the Instance is reachable from one
//! - that an expired link, a revoked one and a switched-off feature each answer
//!   the way they claim to, page and audio alike
//! - and that a shared Call is still **prunable**, which is the failure a
//!   forgotten child table hides until an Operator's disk stops being bounded
//!
//! **The spec pencilled Playwright in here** ("Playwright only where … a
//! standalone page is involved (embed page, share-link preview)") and this is
//! the layer instead, because the Playwright suite runs against `vite preview`
//! serving `client/dist` — there is no Rust process in it, so it cannot render
//! a share page at all. What it *can* see is the one thing this file cannot:
//! that the **service worker** does not answer `/s` out of its cache, which is
//! `client/e2e/pwa.spec.ts`. The page's own markup is asserted where it is
//! built, in `src/share/page.rs`, over a `StoredCall` a test constructs.

mod common;

use common::logs::LogCapture;
use common::{CallUpload, TestApp};
use radio_scout::db::entities::{call, share_link};
use radio_scout::db::repo::NewCall;
use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};

/// A transmission instant nothing else in this file shares — `tests/quiet.rs`'s
/// rule and for its reason: two uploads on one channel a few hundred
/// milliseconds apart are one transmission as far as **Ingest** is concerned
/// (#46).
fn a_fresh_instant() -> i64 {
    static NEXT: std::sync::atomic::AtomicI64 =
        std::sync::atomic::AtomicI64::new(1_669_740_338_000);
    NEXT.fetch_add(600_000, std::sync::atomic::Ordering::Relaxed)
}

/// An Instance with one stored Call, and that Call's id.
async fn an_instance_with_a_call() -> (TestApp, i64) {
    let app = TestApp::with_key("k").await;
    let (status, _) = app.upload(CallUpload::new().at(a_fresh_instant())).await;
    assert_eq!(status, 200);
    let id = app.the_call().await.id;
    (app, id)
}

/// Move a link's expiry into the past.
///
/// `TestApp::age_call`'s trick, for the same reason it exists: a frozen clock
/// stops time rather than passing it, so "this link has run out" is a fact a
/// test arranges on the row rather than one it waits for.
async fn expire(app: &TestApp, at_ms: i64) {
    let link = share_link::Entity::find()
        .one(&app.db)
        .await
        .expect("read the link")
        .expect("a link to expire");
    let mut row = link.into_active_model();
    row.expires_at_ms = Set(at_ms);
    row.update(&app.db).await.expect("expire the link");
}

/// **The whole promise.** A link plays its one Call, publicly, with no session,
/// no key and no app — and the page a recipient opens carries the facts a
/// preview card is built from.
#[tokio::test]
async fn a_share_link_plays_its_call() {
    let (app, id) = an_instance_with_a_call().await;

    let link = app.mint_share(id).await;

    let page = app.get(&link.page).await;
    assert_eq!(page.status(), 200);
    assert_eq!(
        common::header_of(&page, "content-type"),
        Some("text/html; charset=utf-8")
    );
    let html = page.text().await.expect("a page");
    assert!(html.contains("og:title"), "{html}");

    let audio = app.get(&link.audio).await;
    assert_eq!(audio.status(), 200);
    assert_eq!(common::header_of(&audio, "accept-ranges"), Some("bytes"));
    assert!(!audio.bytes().await.expect("audio bytes").is_empty());
}

/// **iOS will not play audio a server cannot range-request** (ADR-0002), and a
/// share link is served to whatever browser a recipient happens to have. The
/// same [`radio_scout::serve`] decision through a different door, which is what
/// makes "works on both storage backends" free rather than rebuilt.
#[tokio::test]
async fn a_share_link_answers_a_range_request() {
    let (app, id) = an_instance_with_a_call().await;
    let link = app.mint_share(id).await;

    let response = app.get_range(&link.audio, "bytes=0-3").await;

    assert_eq!(response.status(), 206);
    assert_eq!(response.bytes().await.expect("bytes").len(), 4);
}

/// **One live link per Call** — the abuse bound on an unauthenticated mint. A
/// second share hands back the link already in circulation with its window
/// pushed out, so a loop of requests writes one row rather than a million.
#[tokio::test]
async fn sharing_a_call_twice_hands_back_the_same_link() {
    let (app, id) = an_instance_with_a_call().await;

    let first = app.mint_share(id).await;
    // Wind the stored expiry back to a moment still in the future, so the
    // second mint has something to *extend* and the extension is visible.
    let soon = radio_scout::now_ms() + 60_000;
    expire(&app, soon).await;
    let second = app.mint_share(id).await;

    assert_eq!(first.page, second.page);
    assert_eq!(app.count::<share_link::Entity>().await, 1);
    assert!(
        second.expires_at_ms > soon,
        "the window was not pushed out: {}",
        second.expires_at_ms
    );
}

/// ...and two Calls are two links, because a link opens exactly one Call.
#[tokio::test]
async fn two_calls_are_two_links() {
    let (app, first) = an_instance_with_a_call().await;
    let (status, _) = app
        .upload(CallUpload::new().talkgroup(99).at(a_fresh_instant()))
        .await;
    assert_eq!(status, 200);
    let second = app
        .calls()
        .await
        .into_iter()
        .map(|call| call.id)
        .find(|call| *call != first)
        .expect("a second Call");

    let one = app.mint_share(first).await;
    let two = app.mint_share(second).await;

    assert_ne!(one.page, two.page);
    assert_eq!(app.count::<share_link::Entity>().await, 2);
}

/// **An expired link says so** — page and audio alike, in the one status that
/// means "this was here and is not any more".
#[tokio::test]
async fn an_expired_link_says_it_expired() {
    let (app, id) = an_instance_with_a_call().await;
    let link = app.mint_share(id).await;
    expire(&app, 1_000).await;

    let page = app.get(&link.page).await;
    assert_eq!(page.status(), 410);
    let html = page.text().await.expect("a page");
    assert!(html.contains("This link has expired"), "{html}");

    assert_eq!(app.get(&link.audio).await.status(), 410);
}

/// ...and re-sharing an expired Call mints a **new** link, killing the old URL
/// rather than bringing it back to life: an expiry is a promise about the link
/// that was handed out.
#[tokio::test]
async fn resharing_an_expired_call_replaces_its_link() {
    let (app, id) = an_instance_with_a_call().await;
    let stale = app.mint_share(id).await;
    expire(&app, 1_000).await;

    let fresh = app.mint_share(id).await;

    assert_ne!(stale.page, fresh.page);
    assert_eq!(app.get(&fresh.page).await.status(), 200);
    assert_eq!(
        app.get(&stale.page).await.status(),
        404,
        "the expired URL came back to life"
    );
    // Still one row: the abuse bound survives a re-issue.
    assert_eq!(app.count::<share_link::Entity>().await, 1);
}

/// A token nobody minted, and a bare `/s` with no token at all, are the same
/// ordinary refusal rather than axum's bare 400 — which would tell a recipient
/// nothing and leave nothing an Operator could grep.
#[tokio::test]
async fn a_token_nobody_minted_opens_nothing() {
    let app = TestApp::spawn().await;

    for path in ["/s?t=nope", "/s", "/s/audio?t=nope"] {
        assert_eq!(app.get(path).await.status(), 404, "{path}");
    }
}

/// **Nothing else is reachable from a share link.** The page itself has no way
/// out, and the namespace it lives in does not fall through to the app — so a
/// mistyped share URL is a 404 rather than the whole Instance.
#[tokio::test]
async fn nothing_else_is_reachable_from_a_share_link() {
    let (app, id) = an_instance_with_a_call().await;
    let link = app.mint_share(id).await;

    let html = app.get(&link.page).await.text().await.expect("a page");
    assert!(!html.contains("<a "), "{html}");
    assert!(!html.contains("/api/calls"), "{html}");

    for path in ["/s/", "/s/anything", "/s/audio/deeper"] {
        assert_eq!(app.get(path).await.status(), 404, "{path}");
    }
}

/// The token is a credential, so it must never reach the request log — which is
/// why it rides the **query string**, the one part of a URL `http_log` has never
/// written down (ADR-0011 rule 2).
#[tokio::test]
async fn a_share_token_never_reaches_the_log() {
    let (app, id) = an_instance_with_a_call().await;
    let capture = LogCapture::start();
    let link = app.mint_share(id).await;

    app.get(&link.page).await;
    app.get(&link.audio).await;
    app.get("/s?t=definitely-not-a-token").await;

    let logged = capture.text();
    assert!(
        !logged.contains(&link.token),
        "the token was logged: {logged}"
    );
    assert!(
        !logged.contains("definitely-not-a-token"),
        "a query string was logged: {logged}"
    );
}

/// **An Operator can revoke one**, and the URL is dead for good — a token is 128
/// random bits and is never reissued to the same value.
#[tokio::test]
async fn revoking_a_link_kills_its_url() {
    let (app, id) = an_instance_with_a_call().await;
    let link = app.mint_share(id).await;
    app.login().await;

    let (status, listing) = app.admin_get("/api/admin/shares").await;
    assert_eq!(status, 200);
    let row = &listing["results"][0];
    assert_eq!(row["call"]["id"], id);
    assert_eq!(row["expired"], false);
    // The token is the credential and this screen is not where it leaks from
    // (the **Webhook** URL rule, one row along).
    assert!(
        !serde_json::to_string(&listing)
            .expect("the listing serializes")
            .contains(&link.token),
        "the admin listing carried the token: {listing}"
    );

    let (status, _) = app
        .admin_delete(&format!("/api/admin/shares/{}", row["id"]))
        .await;

    assert_eq!(status, 204);
    assert_eq!(app.get(&link.page).await.status(), 404);
    assert_eq!(app.get(&link.audio).await.status(), 404);
    assert_eq!(app.count::<share_link::Entity>().await, 0);
}

/// **Every link is reachable, so every link is revocable.**
///
/// The listing is *paged* rather than capped, and the difference matters here
/// more than on any other admin screen: minting takes no credential, so the
/// number of rows is the number of **Calls** rather than the number of times an
/// Operator's Listeners pressed a button. A screen that showed the newest N and
/// stopped would leave the rest invisible *and* unrevocable, which is the one
/// thing it exists for.
#[tokio::test]
async fn the_listing_pages_rather_than_stopping() {
    let app = TestApp::with_key("k").await;
    let mut minted = Vec::new();
    for talkgroup in [1, 2, 3] {
        let (status, _) = app
            .upload(CallUpload::new().talkgroup(talkgroup).at(a_fresh_instant()))
            .await;
        assert_eq!(status, 200);
    }
    for call in app.calls().await {
        minted.push(app.mint_share(call.id).await);
    }
    app.login().await;

    let (status, first) = app.admin_get("/api/admin/shares?limit=2").await;
    assert_eq!(status, 200);
    let (_, second) = app.admin_get("/api/admin/shares?limit=2&offset=2").await;

    // The total describes every link, not the window above it (#98's rule).
    assert_eq!(first["count"], 3);
    assert_eq!(first["hasMore"], true);
    assert_eq!(first["results"].as_array().expect("rows").len(), 2);
    assert_eq!(second["hasMore"], false);
    assert_eq!(second["results"].as_array().expect("rows").len(), 1);

    // ...and between the two pages, every Call that was shared is on screen.
    let mut seen: Vec<i64> = [&first, &second]
        .iter()
        .flat_map(|page| page["results"].as_array().expect("rows"))
        .map(|row| row["call"]["id"].as_i64().expect("a call id"))
        .collect();
    seen.sort_unstable();
    let mut stored: Vec<i64> = app.calls().await.into_iter().map(|call| call.id).collect();
    stored.sort_unstable();
    assert_eq!(seen, stored);
    assert_eq!(minted.len(), 3);
}

/// Revoking something that is not there is a named refusal rather than a
/// silently successful `DELETE`.
#[tokio::test]
async fn revoking_a_link_that_is_not_there_says_so() {
    let app = TestApp::spawn().await;
    app.login().await;

    let (status, body) = app.admin_delete("/api/admin/shares/404").await;

    assert_eq!(status, 404);
    assert_eq!(body["error"], "share-link-not-found");
}

/// **`[share] enabled = false` is a lever, not a hidden button**: it stops the
/// links already minted as well as the minting.
#[tokio::test]
async fn switching_sharing_off_stops_the_links_already_minted() {
    let (app, id) = an_instance_with_a_call().await;
    let link = app.mint_share(id).await;

    let mut app = app;
    app.restart_with(|config| config.share.enabled = false)
        .await;

    assert_eq!(app.get(&link.page).await.status(), 404);
    assert_eq!(app.get(&link.audio).await.status(), 404);
    assert_eq!(
        app.post(&format!("/api/call/{id}/share")).await.status(),
        404
    );
    // Indistinguishable from an unknown link to whoever is knocking, and told
    // apart in the log — which is where the Operator who closed the door goes.
    assert!(
        app.get(&link.page)
            .await
            .text()
            .await
            .expect("a page")
            .contains("This link is not valid")
    );
}

/// A link to a Call that does not exist is refused rather than minted: a typo'd
/// id must not produce a working URL onto a page that can never render.
#[tokio::test]
async fn a_call_that_is_not_there_cannot_be_shared() {
    let app = TestApp::spawn().await;

    let response = app.post("/api/call/404/share").await;

    assert_eq!(response.status(), 404);
    assert_eq!(app.count::<share_link::Entity>().await, 0);
}

/// An **Encrypted Call** is a row with no audio object at all (spec US 9) and is
/// still worth sending somebody — that the channel was busy is the fact. The
/// page says so; there is nothing to play.
#[tokio::test]
async fn an_encrypted_call_is_shareable() {
    let app = TestApp::spawn().await;
    // A row with no audio object at all, which is what **Ingest** writes for a
    // Talkgroup a Recorder told it was encrypted (#42, spec US 9).
    let id = app
        .seed_call(
            NewCall {
                encrypted: true,
                ..NewCall::new(11, 54241, a_fresh_instant())
            },
            None,
        )
        .await;

    let link = app.mint_share(id).await;

    let html = app.get(&link.page).await.text().await.expect("a page");
    assert!(html.contains("This call has no audio."), "{html}");
    assert_eq!(app.get(&link.audio).await.status(), 404);
}

/// **A shared Call is still prunable.** A child table left out of
/// `repo::delete_calls` passes every test that stores and reads, and then fails
/// the **retention sweep** — which walks oldest-first, so from the moment the
/// oldest shared Call comes due every sweep fails at the same row forever and an
/// Operator's disk quietly stops being bounded. That is #55's lesson, and this
/// is the test that would notice.
#[tokio::test]
async fn a_shared_call_is_still_prunable() {
    let (app, id) = an_instance_with_a_call().await;
    let link = app.mint_share(id).await;

    let pruned = radio_scout::db::repo::delete_calls(&app.db, &[id])
        .await
        .expect("prune the Call");

    assert_eq!(pruned, 1);
    assert_eq!(app.count::<call::Entity>().await, 0);
    assert_eq!(app.count::<share_link::Entity>().await, 0);
    assert_eq!(app.get(&link.page).await.status(), 404);
}

/// With `[server] public_url` set, the card gains the three tags that cannot be
/// relative — which is what a messaging app needs for an image.
#[tokio::test]
async fn a_public_url_completes_the_preview_card() {
    let app = TestApp::builder()
        .config(|config| config.server.public_url = Some(String::from("https://scan.example")))
        .spawn()
        .await;
    app.create_api_key("k").await;
    let (status, _) = app.upload(CallUpload::new().at(a_fresh_instant())).await;
    assert_eq!(status, 200);
    let id = app.the_call().await.id;

    let link = app.mint_share(id).await;

    let html = app.get(&link.page).await.text().await.expect("a page");
    assert!(html.contains("https://scan.example/s?t="), "{html}");
    assert!(
        html.contains(r#"<meta property="og:image" content="https://scan.example/icon-512.png">"#),
        "{html}"
    );
}
