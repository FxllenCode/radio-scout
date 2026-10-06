//! **The embeddable player**, driven end to end (#75, spec US 59).
//!
//! An **Embed** is a Selection an Operator publishes for another site to frame:
//! a fire department's homepage, a local newsroom. What is here is everything
//! that can only be said about a running Instance:
//!
//! - that an Operator makes one in admin and is handed the URL a snippet frames
//! - that the page's feed carries its Selection's recent Calls and nothing else
//!   — and **never a restricted channel**, because the URL sits in somebody
//!   else's public HTML and so cannot be a credential
//! - that the embed route, and only the embed route, may be framed
//! - that changing an embed's Selection changes what the host's snippet plays,
//!   without the host editing anything, and deleting one kills it
//! - that the token never reaches the log, and that a feed costs the same
//!   whatever the Archive holds
//!
//! What the page *does* with a feed (listen, play, reconnect) is the client's,
//! in `client/src/embed/`; that it plays inside a frame on another origin is
//! `client/e2e/embed.spec.ts`.

mod common;

use common::logs::LogCapture;
use common::{CallUpload, TestApp, header_of};
use serde_json::{Value, json};

const SYSTEM: i64 = 11;
const FIRE: i64 = 100;
const POLICE: i64 = 200;

/// A transmission instant nothing else in this file shares — two uploads on one
/// channel a few hundred milliseconds apart are one transmission as far as
/// **Ingest** is concerned (#46), so every Call here gets its own minute.
fn a_fresh_instant() -> i64 {
    static NEXT: std::sync::atomic::AtomicI64 =
        std::sync::atomic::AtomicI64::new(1_700_000_000_000);
    NEXT.fetch_add(60_000, std::sync::atomic::Ordering::Relaxed)
}

/// An Instance with one Call on each of two channels, the fire one first, and an
/// Operator signed in.
async fn an_instance() -> TestApp {
    let app = TestApp::with_key("k").await;
    app.upload_ok(CallUpload::new().talkgroup(FIRE).at(a_fresh_instant()))
        .await;
    app.upload_ok(CallUpload::new().talkgroup(POLICE).at(a_fresh_instant()))
        .await;
    app.login().await;
    app
}

/// Just the fire channel.
fn fire_only() -> Value {
    // Keyed as the matrix keys a Ref — a string (ADR-0004) — and spelled whole,
    // `all` included, because that is how a stored Selection reads back.
    json!({ "sel": { "11": { "100": true } }, "all": false })
}

/// Make an Embed through the admin surface, and answer with its row.
async fn add_embed(app: &TestApp, name: &str, selection: Value) -> Value {
    let (status, row) = app
        .admin_post(
            "/api/admin/embeds",
            json!({ "name": name, "selection": selection }),
        )
        .await;
    assert_eq!(status, 201, "making an embed: {row}");
    row
}

/// The path an embed's page lives at, as its row hands it out.
fn page_of(row: &Value) -> String {
    row["path"]
        .as_str()
        .expect("where the embed lives")
        .to_owned()
}

/// The token an embed's row hands out — read off the row's own path, which is
/// [`radio_scout::embed::link_to`]'s spelling and the only one there is.
fn token_of(row: &Value) -> String {
    page_of(row)
        .strip_prefix(&radio_scout::embed::link_to(""))
        .expect("an embed's address")
        .to_owned()
}

/// ...and the feed behind it.
fn feed_of(row: &Value) -> String {
    radio_scout::embed::feed_to(&token_of(row))
}

/// What a feed names, in the order it answered.
fn refs(feed: &Value) -> Vec<i64> {
    feed["calls"]
        .as_array()
        .expect("the recent calls")
        .iter()
        .map(|call| call["talkgroupRef"].as_i64().expect("a talkgroup ref"))
        .collect()
}

// ---------------------------------------------------------------------------
// An Operator makes one
// ---------------------------------------------------------------------------

/// The tracer: an Operator names a Selection, is handed where it lives, and the
/// page's feed carries that Selection's Calls — newest first, and nothing else.
#[tokio::test]
async fn an_embed_carries_its_selection_s_recent_calls() {
    let app = an_instance().await;
    app.upload_ok(CallUpload::new().talkgroup(FIRE).at(a_fresh_instant()))
        .await;

    let row = add_embed(&app, "Fire dispatch", fire_only()).await;
    assert!(
        page_of(&row).starts_with(&radio_scout::embed::link_to("")),
        "{row}"
    );

    let feed = app.get_json(&feed_of(&row)).await;

    assert_eq!(feed["name"], "Fire dispatch");
    assert_eq!(feed["selection"], fire_only());
    assert_eq!(
        refs(&feed),
        vec![FIRE, FIRE],
        "the police Call is not in it"
    );
    let at: Vec<i64> = feed["calls"]
        .as_array()
        .expect("calls")
        .iter()
        .map(|call| call["timestamp"].as_i64().expect("an instant"))
        .collect();
    assert!(at[0] > at[1], "newest first: {at:?}");
}

/// The listing carries every embed with where it lives, because the snippet is
/// the thing an Operator came here for.
#[tokio::test]
async fn the_listing_hands_out_every_embed_s_address() {
    let app = an_instance().await;
    let made = add_embed(&app, "Fire dispatch", fire_only()).await;

    let (status, listing) = app.admin_get("/api/admin/embeds").await;

    assert_eq!(status, 200);
    let row = &listing["results"][0];
    assert_eq!(row["name"], "Fire dispatch");
    assert_eq!(row["path"], made["path"]);
    assert_eq!(row["selection"], fire_only());
    // No `[server] public_url`, so there is no absolute address to hand out:
    // the screen makes one from the origin it is on and says so, rather than
    // this guessing one.
    assert_eq!(row["url"], Value::Null);
}

/// With `[server] public_url` set, the address is absolute — which is what a
/// snippet on somebody else's site has to carry.
#[tokio::test]
async fn a_public_url_makes_the_address_absolute() {
    let app = TestApp::builder()
        .config(|config| config.server.public_url = Some("https://scanner.example.org".into()))
        .spawn()
        .await;
    app.login().await;

    let row = add_embed(&app, "Fire dispatch", fire_only()).await;

    assert_eq!(
        row["url"],
        format!("https://scanner.example.org{}", page_of(&row))
    );
}

/// An embed is named, because the name is what the page says at its top and what
/// the snippet's `title` reads to a screen reader.
#[tokio::test]
async fn an_embed_needs_a_name() {
    let app = an_instance().await;

    let (status, body) = app
        .admin_post(
            "/api/admin/embeds",
            json!({ "name": "  ", "selection": fire_only() }),
        )
        .await;

    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"], "field-required", "{body}");
    assert_eq!(body["field"], "name", "{body}");
}

/// **The host never edits their snippet.** An Operator re-scopes or renames an
/// embed and the same address answers with the new Selection — which is the
/// whole reason an embed is a row rather than a `?sel=` in somebody's HTML.
#[tokio::test]
async fn re_scoping_an_embed_changes_what_the_same_address_plays() {
    let app = an_instance().await;
    let row = add_embed(&app, "Fire dispatch", fire_only()).await;
    let id = row["id"].as_i64().expect("an id");

    let (status, edited) = app
        .admin_patch(
            &format!("/api/admin/embeds/{id}"),
            json!({ "name": "Everything", "selection": { "all": true } }),
        )
        .await;
    assert_eq!(status, 200, "{edited}");
    assert_eq!(edited["path"], row["path"], "the address did not move");

    let feed = app.get_json(&feed_of(&row)).await;
    assert_eq!(feed["name"], "Everything");
    assert_eq!(refs(&feed), vec![POLICE, FIRE]);
}

/// A rename alone leaves the Selection where it was — absent means "leave it".
#[tokio::test]
async fn renaming_an_embed_leaves_its_selection_alone() {
    let app = an_instance().await;
    let row = add_embed(&app, "Fire dispatch", fire_only()).await;
    let id = row["id"].as_i64().expect("an id");

    let (status, edited) = app
        .admin_patch(
            &format!("/api/admin/embeds/{id}"),
            json!({ "name": "County fire" }),
        )
        .await;

    assert_eq!(status, 200, "{edited}");
    assert_eq!(edited["name"], "County fire");
    assert_eq!(edited["selection"], fire_only());
}

/// A rename to nothing is refused like a create with nothing — the name is
/// what the frame says at its top.
#[tokio::test]
async fn an_embed_cannot_be_renamed_to_nothing() {
    let app = an_instance().await;
    let row = add_embed(&app, "Fire dispatch", fire_only()).await;
    let id = row["id"].as_i64().expect("an id");

    let (status, body) = app
        .admin_patch(&format!("/api/admin/embeds/{id}"), json!({ "name": " " }))
        .await;

    assert_eq!(status, 400, "{body}");
    assert_eq!(body["field"], "name", "{body}");
    let feed = app.get_json(&feed_of(&row)).await;
    assert_eq!(feed["name"], "Fire dispatch", "nothing was written");
}

/// Deleting an embed is the revoke: the address a host is framing stops
/// answering, page feed and all.
#[tokio::test]
async fn deleting_an_embed_kills_its_address() {
    let app = an_instance().await;
    let row = add_embed(&app, "Fire dispatch", fire_only()).await;
    let id = row["id"].as_i64().expect("an id");

    let (status, _) = app.admin_delete(&format!("/api/admin/embeds/{id}")).await;
    assert_eq!(status, 204);

    let response = app.get(&feed_of(&row)).await;
    assert_eq!(response.status(), 404);
    assert_eq!(response.text().await.expect("a body"), "embed not found\n");
    let (_, listing) = app.admin_get("/api/admin/embeds").await;
    assert_eq!(listing["results"], json!([]));
}

/// An embed that is not there, on either verb an Operator uses, is named as
/// one.
#[tokio::test]
async fn an_embed_that_is_not_there_is_named() {
    let app = an_instance().await;

    let (patched, body) = app
        .admin_patch("/api/admin/embeds/99", json!({ "name": "x" }))
        .await;
    assert_eq!(patched, 404, "{body}");
    assert_eq!(body["error"], "embed-not-found", "{body}");

    let (deleted, body) = app.admin_delete("/api/admin/embeds/99").await;
    assert_eq!(deleted, 404, "{body}");
}

/// A feed by a token nobody minted, or by none at all, is an ordinary "not
/// found" — the page renders "not available" from it.
#[tokio::test]
async fn a_feed_nobody_made_is_not_found() {
    let app = an_instance().await;

    for path in [
        radio_scout::embed::feed_to("0123456789abcdef"),
        radio_scout::embed::FEED_PATH.to_owned(),
    ] {
        let response = app.get(&path).await;
        assert_eq!(response.status(), 404, "{path}");
        assert_eq!(
            response.text().await.expect("a body"),
            "embed not found\n",
            "{path}"
        );
    }
}

/// Only a page's worth: a host's homepage is not where an Archive is browsed.
#[tokio::test]
async fn a_feed_carries_a_page_of_recent_calls_not_the_archive() {
    let app = an_instance().await;
    for _ in 0..25 {
        app.upload_ok(CallUpload::new().talkgroup(FIRE).at(a_fresh_instant()))
            .await;
    }
    let row = add_embed(&app, "Fire dispatch", fire_only()).await;

    let feed = app.get_json(&feed_of(&row)).await;

    assert_eq!(refs(&feed).len(), radio_scout::embed::RECENT_CALLS as usize);
}

// ---------------------------------------------------------------------------
// Open listening, and nothing more
// ---------------------------------------------------------------------------

/// **An embed never reaches a restricted channel**, whatever its Selection
/// names: its address is in a stranger's public HTML, so it cannot be a
/// credential. It hears what a Listener holding no code hears.
#[tokio::test]
async fn an_embed_never_reaches_a_restricted_channel() {
    let app = an_instance().await;
    app.restrict_talkgroup(SYSTEM, POLICE, Some(true)).await;
    let row = add_embed(&app, "Everything", json!({ "all": true })).await;

    let feed = app.get_json(&feed_of(&row)).await;

    assert_eq!(refs(&feed), vec![FIRE]);
}

/// ...and the screen says so, rather than letting an Operator find out from the
/// host that a channel they picked never plays.
#[tokio::test]
async fn the_listing_counts_the_restricted_channels_an_embed_will_not_play() {
    let app = an_instance().await;
    app.restrict_talkgroup(SYSTEM, POLICE, Some(true)).await;
    add_embed(&app, "Everything", json!({ "all": true })).await;
    add_embed(&app, "Fire dispatch", fire_only()).await;

    let (_, listing) = app.admin_get("/api/admin/embeds").await;

    let rows = listing["results"].as_array().expect("rows");
    assert_eq!(rows[0]["restricted"], 1, "{}", rows[0]);
    assert_eq!(rows[1]["restricted"], 0, "{}", rows[1]);
}

/// A Call still waiting out a **Delay** is not in a feed: an embed is a Listener,
/// and no Listener hears one early (#73).
#[tokio::test]
async fn a_delayed_call_is_not_in_a_feed_early() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    let (status, body) = app
        .admin_post(
            "/api/admin/systems",
            json!({ "ref": SYSTEM, "delayMinutes": 10 }),
        )
        .await;
    assert_eq!(status, 201, "{body}");
    app.upload_ok(CallUpload::new().talkgroup(FIRE)).await;
    let row = add_embed(&app, "Fire dispatch", fire_only()).await;

    let feed = app.get_json(&feed_of(&row)).await;

    assert_eq!(refs(&feed), Vec::<i64>::new());
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// **The embed page may be framed by anybody** — said explicitly, rather than
/// left to the absence of a header.
#[tokio::test]
async fn the_embed_page_may_be_framed_anywhere() {
    let app = an_instance().await;
    let row = add_embed(&app, "Fire dispatch", fire_only()).await;

    let response = app.get(&page_of(&row)).await;

    assert_eq!(response.status(), 200);
    assert_eq!(
        header_of(&response, "content-security-policy"),
        Some("frame-ancestors *")
    );
    assert_eq!(header_of(&response, "x-frame-options"), None);
    assert!(
        header_of(&response, "content-type").is_some_and(|value| value.starts_with("text/html")),
        "a page"
    );
}

/// ...and **nothing else may be**: not the app (which carries the admin
/// surface), not a share page, not the embed page's own asset under another
/// name, not the feed. Both headers, because `X-Frame-Options` is what an old
/// browser reads and `frame-ancestors` is what a new one does.
#[tokio::test]
async fn nothing_else_may_be_framed() {
    let app = an_instance().await;

    for path in [
        "/",
        "/talkgroups",
        "/settings/admin/embeds",
        "/embed.html",
        "/s?t=nope",
        "/api/catalog",
        "/api/embed?t=nope",
        "/api/admin/session",
        "/api/no-such-route",
        "/healthz",
    ] {
        let response = app.get(path).await;
        assert_eq!(
            header_of(&response, "x-frame-options"),
            Some("DENY"),
            "{path}"
        );
        assert_eq!(
            header_of(&response, "content-security-policy"),
            Some("frame-ancestors 'none'"),
            "{path}"
        );
    }
}

/// **A failure is unframable too.** A 5xx is rebuilt on its way out so its body
/// carries only the request id (ADR-0011 rule 4) — and a response rebuilt after
/// the framing was written would leave with none at all.
#[tokio::test]
async fn a_failure_may_not_be_framed_either() {
    let app = an_instance().await;
    app.refuse_reads_of("embeds");

    let response = app.get(&radio_scout::embed::feed_to("abc")).await;

    assert_eq!(response.status(), 500);
    assert_eq!(header_of(&response, "x-frame-options"), Some("DENY"));
    assert_eq!(
        header_of(&response, "content-security-policy"),
        Some("frame-ancestors 'none'")
    );
}

/// The embed page renders whatever its token: the page is one static document,
/// and "not available" is the feed's answer for the page to draw. A host whose
/// embed was deleted gets a sentence in their frame, not a browser error page.
#[tokio::test]
async fn the_embed_page_answers_for_a_token_nobody_made() {
    let app = an_instance().await;

    let response = app.get(&radio_scout::embed::link_to("nope")).await;

    assert_eq!(response.status(), 200);
    assert_eq!(
        header_of(&response, "content-security-policy"),
        Some("frame-ancestors *")
    );
}

// ---------------------------------------------------------------------------
// What it costs, and what it leaves behind
// ---------------------------------------------------------------------------

/// The token rides the query string, so it is never logged — not because it is
/// a secret (it is in a stranger's HTML) but because there is no reason for an
/// Operator's log to be a list of who embeds them, and the share link's rule
/// gets that for free.
#[tokio::test]
async fn an_embed_token_never_reaches_the_log() {
    let app = an_instance().await;
    let row = add_embed(&app, "Fire dispatch", fire_only()).await;
    let token = token_of(&row);
    let capture = LogCapture::start();

    app.get(&page_of(&row)).await;
    app.get(&feed_of(&row)).await;
    app.get(&radio_scout::embed::feed_to("definitely-not-a-token"))
        .await;

    let logged = capture.text();
    assert!(!logged.contains(&token), "the token was logged: {logged}");
    assert!(
        !logged.contains("definitely-not-a-token"),
        "a query string was logged: {logged}"
    );
}

/// **A feed costs the same whatever the Archive holds** — a newsroom's frontpage
/// can put a great many readers on a Pi, and every one of them loads this.
#[tokio::test]
async fn a_feed_costs_the_same_however_many_calls_it_carries() {
    let app = an_instance().await;
    let row = add_embed(&app, "Everything", json!({ "all": true })).await;
    let feed = feed_of(&row);
    app.get_json(&feed).await;

    let (_, few) = app.statements_during(app.get_json(&feed)).await;
    for _ in 0..12 {
        app.upload_ok(CallUpload::new().talkgroup(FIRE).at(a_fresh_instant()))
            .await;
    }
    let (_, many) = app.statements_during(app.get_json(&feed)).await;

    assert_eq!(many, few, "a feed of 14 Calls costs what a feed of 2 did");
}
