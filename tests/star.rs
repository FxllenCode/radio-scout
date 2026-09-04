//! **Starred Calls**, driven end to end (#66, spec US 37).
//!
//! A **Star** is the one mark on a Call that no Recorder proved and no
//! Operator curated: a Listener put it there, from a browser holding no
//! credential at all. Three things follow, and this file is the three:
//!
//! - it is **the Instance's mark, not a browser's** — one column on the Call,
//!   any Listener sets or clears it, and everybody's "Starred" answers the same
//!   shortlist. What that buys is an abuse bound (#64's: a table keyed on
//!   anything but the Call could be filled by anybody who can POST in a loop)
//!   and the absence of a per-browser record of what somebody kept, which is
//!   the listening history ADR-0011 rule 5 exists to stop accumulating
//! - it is **filterable**, so `?starred=1` composes with every other filter and
//!   pages like every other search
//! - and it holds a Call back from **Retention** exactly as far as the Operator
//!   said, which is `[retention] starred_days` and nothing else — including
//!   *not at all*, which is what ships
//!
//! The policy itself is a pure value asserted where it lives
//! (`retention::tests::what_a_star_holds_a_call_back_from`); what is here is
//! everything that can only be said about a running Instance.

mod common;

use common::logs::LogCapture;
use common::{CallUpload, TestApp};
use radio_scout::retention::{self, RetentionConfig};

const DAY: i64 = 86_400_000;

/// A transmission instant nothing else in this file shares — `tests/share.rs`'s
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
    app.upload_ok(CallUpload::new().at(a_fresh_instant())).await;
    let id = app.the_call().await.id;
    (app, id)
}

/// Whether the Archive says this Call is starred, read the way a Listener's
/// screen reads it — off the search row, not out of the database.
async fn starred_on_the_wire(app: &TestApp, id: i64) -> bool {
    let page = app.get_json("/api/calls?limit=100").await;
    let row = page["results"]
        .as_array()
        .expect("results")
        .iter()
        .find(|call| call["id"] == id)
        .expect("the Call is in the Archive")
        .clone();
    // Absent is the answer for nearly every Call there is, which is why the
    // field is omitted rather than sent as `false` on every live frame.
    row["starred"].as_bool().unwrap_or(false)
}

#[tokio::test]
async fn a_star_goes_on_and_comes_off_again() {
    let (app, id) = an_instance_with_a_call().await;
    assert!(!starred_on_the_wire(&app, id).await, "born unstarred");

    let response = app.post(&format!("/api/call/{id}/star")).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await.expect("json")["starred"],
        serde_json::json!(true)
    );
    assert!(starred_on_the_wire(&app, id).await);

    let response = app.delete(&format!("/api/call/{id}/star")).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await.expect("json")["starred"],
        serde_json::json!(false)
    );
    assert!(!starred_on_the_wire(&app, id).await);
}

/// The mark is the Instance's, so a second Listener starring what a first
/// already starred changes nothing and refuses nothing — and un-starring it
/// twice is the same story the other way round. There is nobody to count.
#[tokio::test]
async fn starring_what_is_already_starred_is_still_one_star() {
    let (app, id) = an_instance_with_a_call().await;

    for _ in 0..3 {
        assert_eq!(
            app.post(&format!("/api/call/{id}/star")).await.status(),
            200
        );
    }
    assert!(starred_on_the_wire(&app, id).await);

    for _ in 0..2 {
        assert_eq!(
            app.delete(&format!("/api/call/{id}/star")).await.status(),
            200
        );
    }
    assert!(!starred_on_the_wire(&app, id).await);
}

/// A Call that is not there cannot be starred, and says so in the one refusal
/// vocabulary — at DEBUG, because a Listener with a stale link is ordinary
/// traffic and not something an Operator acts on (ADR-0011 rule 3, as #92
/// amended it).
#[tokio::test]
async fn starring_a_call_that_is_not_there_is_refused() {
    let logs = LogCapture::start();
    let (app, _) = an_instance_with_a_call().await;

    for response in [
        app.post("/api/call/9999/star").await,
        app.delete("/api/call/9999/star").await,
    ] {
        assert_eq!(response.status(), 404);
    }

    let lines = logs.lines_containing("reason=call-not-found");
    assert_eq!(lines.len(), 2, "both verbs record the refusal:\n{lines:#?}");
    assert!(
        !lines.iter().any(|line| line.contains("WARN")),
        "a stale link is not news:\n{lines:#?}"
    );
}

/// The filter is an ordinary dimension of the archive search: it pages, it
/// counts, and it composes with the filters beside it rather than replacing
/// them.
#[tokio::test]
async fn the_starred_filter_answers_with_the_starred_ones() {
    let app = TestApp::with_key("k").await;
    for talkgroup in [100, 200, 300] {
        app.upload_ok(CallUpload::new().talkgroup(talkgroup).at(a_fresh_instant()))
            .await;
    }
    let calls = app.calls().await;
    assert_eq!(calls.len(), 3);
    let starred = &calls[0];
    app.post(&format!("/api/call/{}/star", starred.id)).await;

    let page = app.get_json("/api/calls?starred=1").await;
    assert_eq!(page["count"], serde_json::json!(1));
    assert_eq!(page["results"][0]["id"], serde_json::json!(starred.id));

    // Absent and `false` both mean "no filter": a Listener who has never
    // touched the control, and one who has turned it off, are asking the same
    // question.
    for query in ["", "?starred=false"] {
        let page = app.get_json(&format!("/api/calls{query}")).await;
        assert_eq!(page["count"], serde_json::json!(3), "for {query:?}");
    }

    // ...and it narrows rather than replaces: a Talkgroup the starred Call is
    // not on answers with nothing.
    let page = app.get_json("/api/calls?starred=1&talkgroup=200").await;
    assert_eq!(page["count"], serde_json::json!(0));
}

/// What ships: a Star is a bookmark and nothing more, so an Operator who has
/// said nothing about it keeps the archive they asked for. Anything else and an
/// unauthenticated POST would commit somebody's disk.
#[tokio::test]
async fn a_star_holds_nothing_back_until_the_operator_says_it_may() {
    let (app, id) = an_instance_with_a_call().await;
    let now = app.the_call().await.call_at_ms + 30 * DAY;
    app.post(&format!("/api/call/{id}/star")).await;

    let report = retention::sweep(
        &app.db,
        app.store.as_ref(),
        &RetentionConfig {
            days: 7,
            ..Default::default()
        },
        now,
    )
    .await
    .expect("sweep");

    assert_eq!(report.aged_out, 1);
    assert_eq!(app.calls().await.len(), 0);
}

/// The exemption itself: a longer window, honoured by the age pass, and the
/// unstarred Call beside it going as it always would.
#[tokio::test]
async fn a_star_keeps_a_call_for_as_long_as_the_operator_allows() {
    let app = TestApp::with_key("k").await;
    let at = a_fresh_instant();
    app.upload_ok(CallUpload::new().talkgroup(100).at(at)).await;
    app.upload_ok(CallUpload::new().talkgroup(200).at(at + 1000))
        .await;
    let calls = app.calls().await;
    let kept = calls[0].id;
    app.post(&format!("/api/call/{kept}/star")).await;

    let config = RetentionConfig {
        days: 7,
        starred_days: Some(90),
        ..Default::default()
    };

    // Thirty days on: past the ordinary window, inside the Star's.
    let report = retention::sweep(&app.db, app.store.as_ref(), &config, at + 30 * DAY)
        .await
        .expect("sweep");
    assert_eq!(report.aged_out, 1, "the unstarred one went");
    assert_eq!(app.calls().await.len(), 1);
    assert_eq!(app.calls().await[0].id, kept);

    // ...and a hundred days on, the Star has run out too. The window is
    // measured from the transmission, like `days` — a Star buys a Call a longer
    // life, not a life that starts when somebody noticed it.
    let report = retention::sweep(&app.db, app.store.as_ref(), &config, at + 100 * DAY)
        .await
        .expect("sweep");
    assert_eq!(report.aged_out, 1);
    assert!(app.calls().await.is_empty());
}

/// `starred_days = 0` is "for good" — the reading `days`, `log_days` and
/// `listener_days` already have, so an Operator who wants an archive that keeps
/// what mattered says so the same way everywhere.
#[tokio::test]
async fn a_zero_star_window_keeps_a_starred_call_for_good() {
    let (app, id) = an_instance_with_a_call().await;
    let at = app.the_call().await.call_at_ms;
    app.post(&format!("/api/call/{id}/star")).await;

    let report = retention::sweep(
        &app.db,
        app.store.as_ref(),
        &RetentionConfig {
            days: 7,
            starred_days: Some(0),
            ..Default::default()
        },
        at + 3650 * DAY,
    )
    .await
    .expect("sweep");

    assert_eq!(report.aged_out, 0);
    assert_eq!(app.calls().await.len(), 1);
}

/// **The size cap outranks a Star**, because a cap a Listener can defeat is not
/// a cap — and this is the one surface where the Listener who can defeat it
/// holds no credential at all.
#[tokio::test]
async fn the_size_cap_outranks_a_star() {
    let app = TestApp::with_key("k").await;
    let at = a_fresh_instant();
    for (index, talkgroup) in [100, 200].into_iter().enumerate() {
        app.upload_ok(
            CallUpload::new()
                .talkgroup(talkgroup)
                .at(at + index as i64 * 1000)
                .audio_named(&[1u8; 8], "call.wav", "audio/x-wav"),
        )
        .await;
    }
    let oldest = app.calls().await[0].id;
    app.post(&format!("/api/call/{oldest}/star")).await;

    let report = retention::sweep(
        &app.db,
        app.store.as_ref(),
        &RetentionConfig {
            // No age pass at all, so the only thing that can take a Call here is
            // the cap — and the starred one is the oldest, which is what the cap
            // reaches for first.
            days: 0,
            starred_days: Some(0),
            max_size_bytes: Some(8),
            ..Default::default()
        },
        at + DAY,
    )
    .await
    .expect("sweep");

    assert_eq!(report.over_cap, 1);
    let remaining = app.calls().await;
    assert_eq!(remaining.len(), 1);
    assert_ne!(remaining[0].id, oldest, "the Star did not save it");
}

/// What this Instance does with a Star rides on the catalog, `sharing`'s
/// precedent (#64) taken one step on: a control that quietly means less than a
/// Listener thinks it does is a control that lies, and only the server knows
/// what `[retention] starred_days` says.
#[tokio::test]
async fn what_a_star_is_worth_here_rides_on_the_catalog() {
    let app = TestApp::builder()
        .config(|config| config.retention.starred_days = Some(90))
        .spawn()
        .await;

    let catalog = app.get_json("/api/catalog").await;
    assert_eq!(catalog["starred"]["kept"], serde_json::json!(true));
    assert_eq!(catalog["starred"]["keptDays"], serde_json::json!(90));

    let app = TestApp::spawn().await;
    let catalog = app.get_json("/api/catalog").await;
    assert_eq!(catalog["starred"]["kept"], serde_json::json!(false));
}
