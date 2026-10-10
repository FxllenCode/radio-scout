//! The **Instance identity** (#77, spec US 4, 57, 58) — what an Operator
//! curates about the Instance itself, served to every Listener's app, and the
//! **Reference page** published beside it.

mod common;
use common::TestApp;
use serde_json::json;

/// An Instance nobody has named reads every default, and says nothing it has
/// not been told — no name, no help link, no page.
#[tokio::test]
async fn a_fresh_instance_reads_the_defaults() {
    let app = TestApp::spawn().await;

    assert_eq!(
        app.get_json("/api/instance").await,
        json!({
            "timeFormat": "24h",
            "panelSort": "name",
            "playbackGoesLive": false,
            "dimmerSecs": 0,
            "showListeners": false,
            "reference": false,
        })
    );
}

/// What the Operator saves is what the very next Listener is sent — re-read on
/// the request that changed it rather than at the next boot — and what the
/// Instance still says after one.
#[tokio::test]
async fn what_an_operator_saves_every_listener_reads_and_a_restart_keeps() {
    let mut app = TestApp::spawn().await;
    app.login().await;

    let (status, saved) = app
        .admin_patch(
            "/api/admin/instance",
            json!({
                "name": "Fulton Scanner",
                "helpUrl": "ops@example.com",
                "timeFormat": "12h",
                "panelSort": "ref",
                "playbackGoesLive": true,
                "dimmerSecs": 30,
                "showListeners": true,
            }),
        )
        .await;
    assert_eq!(status, 200, "{saved}");

    let expected = json!({
        "name": "Fulton Scanner",
        "helpUrl": "mailto:ops@example.com",
        "timeFormat": "12h",
        "panelSort": "ref",
        "playbackGoesLive": true,
        "dimmerSecs": 30,
        "showListeners": true,
        "reference": false,
    });
    assert_eq!(saved, expected);
    assert_eq!(app.get_json("/api/instance").await, expected);
    // ...and the admin surface's own read is the same document, because
    // nothing in it is the Operator's alone.
    assert_eq!(
        app.admin_get("/api/admin/instance").await,
        (200, expected.clone())
    );

    app.restart().await;
    assert_eq!(app.get_json("/api/instance").await, expected);
}

/// A patch is an edit: what it does not name stays, and `null` clears.
#[tokio::test]
async fn a_patch_leaves_alone_what_it_does_not_name() {
    let app = TestApp::spawn().await;
    app.login().await;
    app.admin_patch(
        "/api/admin/instance",
        json!({ "name": "Fulton Scanner", "dimmerSecs": 30 }),
    )
    .await;

    let (_, renamed) = app
        .admin_patch("/api/admin/instance", json!({ "name": null }))
        .await;

    assert!(renamed.get("name").is_none(), "{renamed}");
    assert_eq!(renamed["dimmerSecs"], 30);
}

/// A refusal names the field, so the form can put it under the input — and
/// stores nothing, so a half-good save is not half saved.
#[tokio::test]
async fn a_refused_change_says_which_field_and_saves_nothing() {
    let app = TestApp::spawn().await;
    app.login().await;

    let (status, refused) = app
        .admin_patch(
            "/api/admin/instance",
            json!({ "name": "x".repeat(65), "showListeners": true }),
        )
        .await;
    assert_eq!(status, 400);
    assert_eq!(refused["error"], "too-long");
    assert_eq!(refused["field"], "name");

    let (status, refused) = app
        .admin_patch(
            "/api/admin/instance",
            json!({ "helpUrl": "javascript:alert(1)" }),
        )
        .await;
    assert_eq!(status, 400);
    assert_eq!(refused["error"], "unusable-help-link");

    assert_eq!(app.get_json("/api/instance").await["showListeners"], false);
}

// ---------------------------------------------------------------------------
// The Reference page (#77, spec US 58)
// ---------------------------------------------------------------------------

/// No page is no page: nothing to read, and the app is told there is no entry
/// to draw — while the editor still has the seed to start from.
#[tokio::test]
async fn an_instance_with_no_page_serves_none_and_offers_the_seed() {
    let app = TestApp::spawn().await;
    app.login().await;

    assert_eq!(app.get("/api/reference").await.status(), 404);
    assert_eq!(app.get_json("/api/instance").await["reference"], false);

    let (status, editor) = app.admin_get("/api/admin/reference").await;
    assert_eq!(status, 200);
    assert!(editor.get("markdown").is_none(), "{editor}");
    assert!(
        editor["seed"]
            .as_str()
            .expect("a seed")
            .contains("Phonetic alphabets"),
        "{editor}"
    );
}

/// Published, it is read as HTML by anybody, the app is told to draw its entry,
/// and the editor opens on what was written. Taken down, it is gone again.
#[tokio::test]
async fn a_published_page_is_read_by_everyone_until_it_is_taken_down() {
    let mut app = TestApp::builder()
        .clock(radio_scout::Clock::frozen(1_700_000_000_000))
        .spawn()
        .await;
    app.login().await;

    let (status, published) = app
        .admin_put(
            "/api/admin/reference",
            json!({ "markdown": "# Codes\n\n10-4 is *acknowledged*.\n" }),
        )
        .await;
    assert_eq!(status, 200, "{published}");
    assert_eq!(
        published["markdown"],
        "# Codes\n\n10-4 is *acknowledged*.\n"
    );
    assert_eq!(published["publishedAtMs"], 1_700_000_000_000_i64);

    let page = app.get_json("/api/reference").await;
    assert_eq!(
        page,
        json!({
            "html": "<h1>Codes</h1>\n<p>10-4 is <em>acknowledged</em>.</p>\n",
            "publishedAtMs": 1_700_000_000_000_i64,
        })
    );
    assert_eq!(app.get_json("/api/instance").await["reference"], true);

    // ...and it is the page after a restart, rendered again from what was kept.
    app.restart().await;
    app.login().await;
    assert_eq!(app.get_json("/api/reference").await, page);

    let (status, _) = app.admin_delete("/api/admin/reference").await;
    assert_eq!(status, 204);
    assert_eq!(app.get("/api/reference").await.status(), 404);
    assert_eq!(app.get_json("/api/instance").await["reference"], false);
}

/// The page is markdown and nothing more, all the way to the Listener: raw
/// HTML arrives as text and a `javascript:` link as its words.
#[tokio::test]
async fn a_published_page_cannot_run_anything() {
    let app = TestApp::spawn().await;
    app.login().await;
    app.admin_put(
        "/api/admin/reference",
        json!({ "markdown": "<script>alert(1)</script>\n\n[tap](javascript:alert(1))\n" }),
    )
    .await;

    let html = app.get_json("/api/reference").await["html"]
        .as_str()
        .expect("html")
        .to_owned();

    assert!(!html.contains("<script"), "{html}");
    assert!(!html.contains("javascript:"), "{html}");
    assert!(html.contains("&lt;script&gt;"), "{html}");
}

/// Publishing changes the page and nothing else — the settings beside it in the
/// one row survive, and so does the page when the settings change.
#[tokio::test]
async fn the_page_and_the_settings_do_not_overwrite_each_other() {
    let app = TestApp::spawn().await;
    app.login().await;
    app.admin_patch("/api/admin/instance", json!({ "name": "Fulton Scanner" }))
        .await;

    app.admin_put("/api/admin/reference", json!({ "markdown": "# Codes" }))
        .await;
    app.admin_patch("/api/admin/instance", json!({ "dimmerSecs": 5 }))
        .await;

    let instance = app.get_json("/api/instance").await;
    assert_eq!(instance["name"], "Fulton Scanner");
    assert_eq!(instance["reference"], true);
}

/// A blank page is refused rather than published as nothing — taking a page
/// down is `DELETE`, which says so.
#[tokio::test]
async fn a_blank_page_is_refused() {
    let app = TestApp::spawn().await;
    app.login().await;

    let (status, refused) = app
        .admin_put("/api/admin/reference", json!({ "markdown": "  \n" }))
        .await;

    assert_eq!(status, 400);
    assert_eq!(refused["error"], "field-required");
    assert_eq!(refused["field"], "markdown");
}
