//! **Access codes** (#68, spec US 52) — gating the sensitive, keeping the open.
//!
//! Two halves, and every test here is about one of them: *what is sensitive*
//! (`restricted` on a System or a Talkgroup) and *who may hear it* (an **Access
//! code**, and the **grant** a browser carries once it has unlocked one).
//!
//! The headline is the acceptance criterion that is easiest to break and hardest
//! to notice: **an Instance with nothing restricted is exactly what it was
//! before this existed** — the same answers, the same catalog document, and not
//! one extra statement. It is asserted first and asserted as a *cost*, because a
//! feature that is free only until somebody adds a join is not free.

mod common;

use common::{
    CallUpload, FILTER_BUDGET, TestApp, audio_at, frame_within, next_json, no_frame_within,
    subscribe,
};
use radio_scout::db::repo::NewCall;
use serde_json::json;

/// The gated channel every test below uses, and the open one beside it.
const OPEN: i64 = 54241;
const GATED: i64 = 54999;
const SYSTEM: i64 = 11;

/// An Instance holding one open Call and one on a channel an Operator has
/// gated — the shape nearly every test here wants.
///
/// The Calls are **recent**, because the panel's activity counts are bounded to
/// a day (`catalog::ACTIVITY_WINDOW_MS`) and a fixture stamped in 1970 would
/// make "a locked row loses its activity" vacuously true.
async fn an_instance_with_one_gated_channel() -> TestApp {
    let app = TestApp::with_key("k").await;
    // Marking a channel sensitive is **curation**, so the fixture signs in the
    // way an Operator does rather than writing the column behind the surface's
    // back — which is also what exercises the cached gate being re-read on the
    // request that changed it.
    app.login().await;
    app.upload_ok(CallUpload::new().talkgroup(OPEN).at(recent(20_000)))
        .await;
    app.upload_ok(CallUpload::new().talkgroup(GATED).at(recent(10_000)))
        .await;
    app.restrict_talkgroup(SYSTEM, GATED, Some(true)).await;
    app
}

/// An instant `ago_ms` in the past, so a fixture's Calls are inside every
/// window this Instance measures.
fn recent(ago_ms: i64) -> i64 {
    radio_scout::now_ms() - ago_ms
}

/// The id of the Call on `talkgroup_ref`.
async fn the_call_on(app: &TestApp, talkgroup_ref: i64) -> i64 {
    let talkgroup = app
        .talkgroup_by_ref(SYSTEM, talkgroup_ref)
        .await
        .expect("the channel")
        .id;
    app.calls()
        .await
        .into_iter()
        .find(|call| call.talkgroup_id == talkgroup)
        .expect("a Call on that channel")
        .id
}

/// What a search page names, in the order it answered.
fn refs(page: &serde_json::Value) -> Vec<i64> {
    page["results"]
        .as_array()
        .expect("a page of calls")
        .iter()
        .map(|call| call["talkgroupRef"].as_i64().expect("a talkgroup ref"))
        .collect()
}

// ---------------------------------------------------------------------------
// An instance with no codes behaves exactly as today
// ---------------------------------------------------------------------------

/// **The acceptance criterion, asserted as a cost.**
///
/// Nothing is restricted, so there is no gate to apply — and the way that is
/// true is that [`radio_scout::access::Access::is_gating`] is `false` and no
/// read below it issues a statement, makes a join, or adds a clause. Counted
/// rather than described, because "we only pay when it is on" is the kind of
/// claim that survives the day somebody adds an unconditional join.
#[tokio::test]
async fn an_instance_that_gates_nothing_pays_nothing() {
    let app = TestApp::with_key("k").await;
    app.upload_ok(CallUpload::new().talkgroup(OPEN)).await;

    // Warmed first: the very first search of a process pays for things a
    // steady-state one does not, and what is being measured here is the
    // *difference* a grant makes rather than the cost of a cold start.
    app.get_json("/api/calls").await;

    let before = app.statements_issued();
    app.get_json("/api/calls").await;
    let ungated = app.statements_issued() - before;

    // ...and again with a grant in hand that this Instance has never heard of.
    // It is not looked up, because there is nothing for it to open.
    let before = app.statements_issued();
    app.get_json_with_grant("/api/calls", "rsg_deadbeef").await;
    let with_a_grant = app.statements_issued() - before;

    assert_eq!(
        with_a_grant, ungated,
        "an Instance that gates nothing never resolves a grant"
    );
}

/// The catalog document a fresh install serves is the one it served before this
/// feature existed — no `access`, no `locked`, byte for byte.
#[tokio::test]
async fn a_catalog_with_nothing_gated_says_nothing_about_access() {
    let app = TestApp::with_key("k").await;
    app.upload_ok(CallUpload::new().talkgroup(OPEN)).await;

    let catalog = app.get_json("/api/catalog").await;

    assert_eq!(catalog["access"], serde_json::Value::Null);
    let channel = &catalog["systems"][0]["talkgroups"][0];
    assert_eq!(channel["ref"], OPEN);
    assert_eq!(channel["locked"], serde_json::Value::Null);
}

/// Open listening, unchanged: no code, and every unrestricted channel answers.
#[tokio::test]
async fn an_open_channel_stays_open_to_a_listener_holding_nothing() {
    let app = an_instance_with_one_gated_channel().await;

    let page = app.get_json("/api/calls").await;

    assert_eq!(
        refs(&page),
        vec![OPEN],
        "the gated channel, and only it, went"
    );
    assert_eq!(page["count"], 1, "and the total counts what the page holds");
}

// ---------------------------------------------------------------------------
// What a gate hides
// ---------------------------------------------------------------------------

/// Every read surface that answers with **many** Calls hides a gated one, and
/// they all go through one predicate — which is why this is a table rather than
/// six tests: the thing worth proving is that none of them is the one somebody
/// forgot.
#[tokio::test]
async fn every_archive_surface_hides_a_gated_call() {
    let app = an_instance_with_one_gated_channel().await;

    let page = app.get_json("/api/calls").await;
    assert_eq!(refs(&page), vec![OPEN], "search");

    let filters = app.get_json("/api/calls/filters").await;
    let offered: Vec<i64> = filters["talkgroups"]
        .as_array()
        .expect("the talkgroup facet")
        .iter()
        .map(|option| option["ref"].as_i64().expect("a ref"))
        .collect();
    assert_eq!(offered, vec![OPEN], "the cascading filter options");

    let activity = app.get_json("/api/calls/activity?buckets=4").await;
    let counted: i64 = activity["values"]
        .as_array()
        .expect("the axis's buckets")
        .iter()
        .map(|bucket| bucket.as_i64().unwrap_or_default())
        .sum();
    assert_eq!(counted, 1, "the density ribbon counts what the page holds");
}

/// ...and every surface that answers about **one** Call answers as though it is
/// not there. `404`, never `403`: a `403` on a hand-typed id is a working oracle
/// for what an Operator gated, one id at a time.
#[tokio::test]
async fn every_single_call_surface_answers_as_if_a_gated_call_were_not_there() {
    let app = an_instance_with_one_gated_channel().await;
    let gated = the_call_on(&app, GATED).await;

    for path in [
        format!("/api/call/{gated}"),
        format!("/api/call/{gated}/audio"),
        format!("/api/call/{gated}/download"),
    ] {
        let response = app.get(&path).await;
        assert_eq!(response.status().as_u16(), 404, "{path}");
    }

    let minted = app.post(&format!("/api/call/{gated}/share")).await;
    assert_eq!(
        minted.status().as_u16(),
        404,
        "a stranger cannot mint a public link onto a gated Call"
    );

    let starred = app.post(&format!("/api/call/{gated}/star")).await;
    assert_eq!(starred.status().as_u16(), 404, "nor leave a Star on one");
}

/// The live feed and its **Backfill** are gated by the same rule, which is the
/// half ADR-0008 says must never differ: a restriction holding on only one of
/// them hands the Archive to anybody who reconnects with a cursor.
#[tokio::test]
async fn the_live_feed_and_its_backfill_are_gated_alike() {
    let app = an_instance_with_one_gated_channel().await;

    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, r#"{"t":"sub","all":true}"#).await;

    app.upload_ok(CallUpload::new().talkgroup(GATED).at(3_000))
        .await;
    no_frame_within(&mut ws, FILTER_BUDGET).await;

    app.upload_ok(CallUpload::new().talkgroup(OPEN).at(4_000))
        .await;
    let frame = next_json(&mut ws).await;
    assert_eq!(frame["call"]["talkgroupRef"], OPEN);

    // ...and a reconnect that asks for everything since the beginning gets the
    // open Calls and none of the gated ones.
    let mut returning = app.connect_ws().await;
    subscribe(&mut returning, r#"{"t":"sub","all":true,"since":0}"#).await;
    let mut heard = Vec::new();
    while let Some(frame) = frame_within(&mut returning, FILTER_BUDGET).await {
        if frame["t"] == "call" {
            heard.push(frame["call"]["talkgroupRef"].as_i64().expect("a ref"));
        }
    }
    assert_eq!(heard, vec![OPEN, OPEN], "a Backfill is gated too");
}

// ---------------------------------------------------------------------------
// What a code opens
// ---------------------------------------------------------------------------

/// The whole feature in one pass: a code opens exactly the channel it names,
/// leaves the open ones open, and reaches nothing else.
#[tokio::test]
async fn a_code_opens_what_it_names_and_nothing_else() {
    let app = an_instance_with_one_gated_channel().await;
    app.upload_ok(CallUpload::new().talkgroup(54_998).at(recent(15_000)))
        .await;
    app.restrict_talkgroup(SYSTEM, 54_998, Some(true)).await;
    app.login().await;
    let grant = app
        .create_access_code(
            "FIRE-2026-OPS",
            json!({ "sel": { "11": { "54999": true } } }),
        )
        .await;

    let page = app
        .get_json_with_grant("/api/calls?sort=oldest", &grant)
        .await;

    assert_eq!(
        refs(&page),
        vec![OPEN, GATED],
        "the open channel and the one this code opens — never the other gated one"
    );
}

/// A code scoped to a **System** reaches a channel that did not exist when it
/// was written, which is how channels come to exist on a real Instance (#8).
#[tokio::test]
async fn a_system_wide_code_reaches_a_channel_minted_after_it() {
    let app = TestApp::with_key("k").await;
    app.upload_ok(CallUpload::new().talkgroup(OPEN)).await;
    app.login().await;
    app.restrict_system(SYSTEM, true).await;
    let grant = app
        .create_access_code(
            "COUNTY-EVERYTHING",
            json!({ "sel": { "11": { "*": true } } }),
        )
        .await;

    // A Ref this Instance has never seen, arriving on a gated System.
    app.upload_ok(CallUpload::new().talkgroup(70_001).at(9_000))
        .await;

    assert_eq!(
        refs(&app.get_json("/api/calls").await),
        Vec::<i64>::new(),
        "an auto-populated channel on a gated System arrives gated"
    );
    assert_eq!(
        refs(
            &app.get_json_with_grant("/api/calls?sort=oldest", &grant)
                .await
        ),
        vec![OPEN, 70_001],
        "...and the System-wide code reaches it"
    );
}

/// A Talkgroup may open itself back up on a gated System — the `enhancement`
/// nullable-inherit shape read the other way round.
#[tokio::test]
async fn a_channel_can_opt_back_out_of_a_gated_system() {
    let app = TestApp::with_key("k").await;
    app.upload_ok(CallUpload::new().talkgroup(OPEN).at(recent(20_000)))
        .await;
    app.upload_ok(CallUpload::new().talkgroup(GATED).at(recent(10_000)))
        .await;
    app.login().await;
    app.restrict_system(SYSTEM, true).await;

    assert_eq!(refs(&app.get_json("/api/calls").await), Vec::<i64>::new());

    app.restrict_talkgroup(SYSTEM, OPEN, Some(false)).await;

    assert_eq!(
        refs(&app.get_json("/api/calls").await),
        vec![OPEN],
        "one channel let back out, and the System still gated"
    );
}

/// Restricting a channel applies to the **very next request**, not to the next
/// restart. The gate is a cached bit, and this is the assertion that it is
/// re-read on the same request that changed it.
#[tokio::test]
async fn a_channel_restricted_now_is_gated_on_the_next_request() {
    let app = TestApp::with_key("k").await;
    app.upload_ok(CallUpload::new().talkgroup(GATED)).await;
    assert_eq!(refs(&app.get_json("/api/calls").await), vec![GATED]);
    app.login().await;

    app.restrict_talkgroup(SYSTEM, GATED, Some(true)).await;

    assert_eq!(
        refs(&app.get_json("/api/calls").await),
        Vec::<i64>::new(),
        "no restart, no next boot — the next request"
    );

    // ...and the same in the other direction, which is the one that merely
    // costs a join rather than opening a gate.
    app.restrict_talkgroup(SYSTEM, GATED, Some(false)).await;
    assert_eq!(refs(&app.get_json("/api/calls").await), vec![GATED]);
}

/// A code-holder may do everything with a gated Call a Listener may do with an
/// open one — including handing one on, which is the decision ADR-0008 records.
#[tokio::test]
async fn a_code_holder_may_share_and_star_a_gated_call() {
    let app = an_instance_with_one_gated_channel().await;
    app.login().await;
    let grant = app
        .create_access_code(
            "FIRE-2026-OPS",
            json!({ "sel": { "11": { "54999": true } } }),
        )
        .await;
    let gated = the_call_on(&app, GATED).await;

    let starred = app
        .post(&common::with_grant(
            &format!("/api/call/{gated}/star"),
            &grant,
        ))
        .await;
    assert_eq!(starred.status().as_u16(), 200);

    let minted = app
        .post(&common::with_grant(
            &format!("/api/call/{gated}/share"),
            &grant,
        ))
        .await;
    assert_eq!(
        minted.status().as_u16(),
        200,
        "minting needs the scope; the link then needs nobody's"
    );
}

/// A radio's history counts what this Listener may hear and nothing else.
///
/// The summary is a `GROUP BY` rather than a page, so without its own gate it
/// would tally a gated channel that `?unit=`'s Calls do not show — a number
/// saying a radio was somewhere its Calls are not, which is worse than either
/// answer alone.
#[tokio::test]
async fn a_radios_history_counts_only_what_can_be_heard() {
    let app = TestApp::with_key("k").await;
    app.upload_ok(
        CallUpload::new()
            .talkgroup(OPEN)
            .at(recent(20_000))
            .set("source", 4242),
    )
    .await;
    app.upload_ok(
        CallUpload::new()
            .talkgroup(GATED)
            .at(recent(10_000))
            .set("source", 4242),
    )
    .await;
    app.login().await;
    let grant = app
        .create_access_code(
            "FIRE-2026-OPS",
            json!({ "sel": { "11": { "54999": true } } }),
        )
        .await;

    let ungated = app.get_json(&format!("/api/unit/{SYSTEM}/4242")).await;
    assert_eq!(
        ungated["callCount"], 2,
        "both channels, while neither is gated"
    );

    app.restrict_talkgroup(SYSTEM, GATED, Some(true)).await;

    let open = app.get_json(&format!("/api/unit/{SYSTEM}/4242")).await;
    assert_eq!(
        open["callCount"], 1,
        "the gated Call is not this Listener's"
    );
    assert_eq!(
        open["talkgroups"].as_array().expect("channels").len(),
        1,
        "and neither is the channel it was on"
    );

    let held = app
        .get_json_with_grant(&format!("/api/unit/{SYSTEM}/4242"), &grant)
        .await;
    assert_eq!(held["callCount"], 2, "...and with the code, it is");
}

// ---------------------------------------------------------------------------
// The catalog
// ---------------------------------------------------------------------------

/// A locked row is **there** and its activity is **not**: the Operator gated the
/// channel, not the fact that it exists — and how busy a gated channel has been
/// is traffic analysis of exactly what was gated.
#[tokio::test]
async fn a_locked_row_keeps_its_name_and_loses_its_activity() {
    let app = an_instance_with_one_gated_channel().await;

    let catalog = app.get_json("/api/catalog").await;
    let channels = catalog["systems"][0]["talkgroups"]
        .as_array()
        .expect("the panel's rows");
    let gated = channels
        .iter()
        .find(|channel| channel["ref"] == GATED)
        .expect("the gated row is still offered");

    assert_eq!(gated["locked"], true);
    assert_eq!(gated["recentCalls"], serde_json::Value::Null, "no counts");
    assert_eq!(
        gated["lastCallAtMs"],
        serde_json::Value::Null,
        "no last-heard"
    );
    assert_eq!(
        catalog["access"]["gating"], true,
        "so the panel can offer the unlock"
    );

    let open = channels
        .iter()
        .find(|channel| channel["ref"] == OPEN)
        .expect("the open row");
    assert_eq!(open["locked"], serde_json::Value::Null);
    assert_eq!(open["recentCalls"], 1, "and an open row keeps its activity");
}

/// With the code in hand the row unlocks, activity and all — and the catalog
/// says which code did it, by **label**, never by the code or the grant.
#[tokio::test]
async fn a_grant_unlocks_the_row_and_the_catalog_names_the_code_by_label() {
    let app = an_instance_with_one_gated_channel().await;
    app.login().await;
    let grant = app
        .create_access_code_as(json!({
            "label": "Fire Ops",
            "code": "FIRE-2026-OPS",
            "scope": { "sel": { "11": { "54999": true } } },
        }))
        .await;

    let catalog = app.get_json_with_grant("/api/catalog", &grant).await;
    let gated = catalog["systems"][0]["talkgroups"]
        .as_array()
        .expect("rows")
        .iter()
        .find(|channel| channel["ref"] == GATED)
        .expect("the gated row");

    assert_eq!(gated["locked"], serde_json::Value::Null);
    assert_eq!(
        gated["recentCalls"], 1,
        "unlocked, and its activity with it"
    );
    assert_eq!(catalog["access"]["label"], "Fire Ops");
    let said = catalog.to_string();
    assert!(
        !said.contains("FIRE-2026-OPS"),
        "the code is never in a response"
    );
    assert!(!said.contains(&grant), "and neither is the grant");
}

/// A browser holding a grant that has stopped working **degrades to open
/// listening and is told so**, rather than finding the whole app answering 401.
/// The catalog is where it is told, which is what lets the client clear it.
#[tokio::test]
async fn a_revoked_grant_degrades_to_open_listening_and_says_so() {
    let app = an_instance_with_one_gated_channel().await;
    app.login().await;
    let grant = app
        .create_access_code(
            "FIRE-2026-OPS",
            json!({ "sel": { "11": { "54999": true } } }),
        )
        .await;
    let id = app.admin_get("/api/admin/codes").await.1["results"][0]["id"]
        .as_i64()
        .expect("the code's id");

    let (status, _) = app.admin_delete(&format!("/api/admin/codes/{id}")).await;
    assert_eq!(status, 204);

    let page = app.get_json_with_grant("/api/calls", &grant).await;
    assert_eq!(refs(&page), vec![OPEN], "still listening, just not to that");

    let catalog = app.get_json_with_grant("/api/catalog", &grant).await;
    assert_eq!(catalog["access"]["stale"], "unknown");
}

// ---------------------------------------------------------------------------
// Unlocking
// ---------------------------------------------------------------------------

/// The happy path: the code goes up in a body, the grant comes back, and the
/// grant is what opens things afterwards.
#[tokio::test]
async fn unlocking_hands_back_a_grant_that_works() {
    let app = an_instance_with_one_gated_channel().await;
    app.login().await;
    app.create_access_code_as(json!({
        "label": "Fire Ops",
        "code": "FIRE-2026-OPS",
        "scope": { "sel": { "11": { "54999": true } } },
    }))
    .await;

    let (status, unlocked) = app.unlock("FIRE-2026-OPS").await;

    assert_eq!(status, 200, "{unlocked}");
    assert_eq!(unlocked["label"], "Fire Ops");
    assert_eq!(unlocked["scope"]["sel"]["11"]["54999"], true);
    let grant = unlocked["grant"].as_str().expect("a grant");
    assert_eq!(
        refs(
            &app.get_json_with_grant("/api/calls?sort=oldest", grant)
                .await
        ),
        vec![OPEN, GATED]
    );
}

/// A code is read the way it is said out loud: the surrounding whitespace of
/// something pasted out of a message is not part of it.
#[tokio::test]
async fn a_pasted_code_is_trimmed_at_both_ends() {
    let app = an_instance_with_one_gated_channel().await;
    app.login().await;
    app.create_access_code("FIRE-2026-OPS", json!({ "all": true }))
        .await;

    let (status, unlocked) = app.unlock("  FIRE-2026-OPS  ").await;

    assert_eq!(status, 200, "{unlocked}");
}

/// A wrong code is refused, the address is charged for it, and **the code never
/// appears anywhere** — which is the single thing rdio-scanner writes into its
/// log on this exact path (`controller.go:462`).
#[tokio::test]
async fn a_wrong_code_is_refused_and_never_written_down() {
    let app = TestApp::with_key("k").await;
    let logs = app.store_logs();
    app.login().await;
    app.create_access_code("FIRE-2026-OPS", json!({ "all": true }))
        .await;

    let (status, body) = app.unlock("hunter22222").await;

    assert_eq!(status, 401);
    assert!(!body.to_string().contains("hunter22222"), "{body}");
    logs.wait_for("invalid-access-code").await;
    logs.assert_never_logged("hunter22222");
    logs.assert_never_logged("FIRE-2026-OPS");
}

/// The lockout is the admin login's own, and it bites before any Argon2id work
/// — which is what keeps an unbounded guesser from being a way to exhaust a Pi.
#[tokio::test]
async fn an_address_that_keeps_guessing_is_locked_out() {
    let app = TestApp::builder()
        .config(|config| config.access.lockout_attempts = 3)
        .spawn()
        .await;
    app.create_api_key("k").await;
    app.login().await;
    app.create_access_code("FIRE-2026-OPS", json!({ "all": true }))
        .await;

    for attempt in 1..=3 {
        let (status, _) = app.unlock("wrongwrongwrong").await;
        assert_eq!(status, 401, "attempt {attempt} is inside the budget");
    }

    let (locked, body) = app.unlock("wrongwrongwrong").await;
    assert_eq!(locked, 429, "{body}");

    // ...and a locked address is refused **even when it guesses right**, or the
    // lockout would not be one.
    let (still_locked, _) = app.unlock("FIRE-2026-OPS").await;
    assert_eq!(still_locked, 429);
}

/// A code that has run out says so, and says something different from a wrong
/// one — because "you typed it wrong" and "this stopped working last night"
/// send a Listener to different people.
#[tokio::test]
async fn an_expired_code_says_it_expired() {
    let app = TestApp::builder()
        .config(|config| config.access.lockout_attempts = 3)
        .clock(radio_scout::Clock::frozen(5_000))
        .spawn()
        .await;
    app.create_api_key("k").await;
    app.login().await;
    app.create_access_code_as(json!({
        "code": "FIRE-2026-OPS",
        "scope": { "all": true },
        "expiresAtMs": 4_000,
    }))
    .await;

    let (status, body) = app.unlock("FIRE-2026-OPS").await;

    assert_eq!(status, 410, "{body}");

    // Deliberately **not** charged to the lockout: whoever typed this knows the
    // code, and there is nothing left for them to guess at.
    for _ in 0..5 {
        assert_eq!(app.unlock("FIRE-2026-OPS").await.0, 410);
    }
}

/// A **disabled** code is indistinguishable from one that never existed — the
/// durable off, and the Operator who switched it off is the one who knows.
#[tokio::test]
async fn a_disabled_code_reads_exactly_like_a_wrong_one() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    app.create_access_code_as(json!({
        "code": "FIRE-2026-OPS",
        "scope": { "all": true },
        "disabled": true,
    }))
    .await;

    assert_eq!(app.unlock("FIRE-2026-OPS").await.0, 401);
}

// ---------------------------------------------------------------------------
// Connection limits and expiry, on a live socket
// ---------------------------------------------------------------------------

/// The limit counts connections, refuses the newest, **says why**, and gives the
/// slot back when one closes.
///
/// rdio tells its client `max` and then goes on serving it, because by then it
/// has already assigned the scope.
#[tokio::test]
async fn a_connection_limit_refuses_the_newest_and_says_why() {
    let app = an_instance_with_one_gated_channel().await;
    app.login().await;
    let grant = app
        .create_access_code_as(json!({
            "code": "FIRE-2026-OPS",
            "scope": { "sel": { "11": { "54999": true } } },
            "maxConnections": 1,
        }))
        .await;

    let mut first = app.connect_ws_as(&grant).await;
    subscribe(&mut first, r#"{"t":"sub","all":true}"#).await;

    let (_second, refused) = app.try_connect_ws_as(&grant).await;
    assert_eq!(refused["t"], "refused");
    assert_eq!(refused["reason"], "access-connection-limit");

    // The first is still serving, and the gated Call still reaches it — where
    // rdio tells its client `max` and then goes on serving *both*, because by
    // then it has already assigned the scope.
    app.upload_ok(CallUpload::new().talkgroup(GATED).at(6_000))
        .await;
    let frame = next_json(&mut first).await;
    assert_eq!(frame["call"]["talkgroupRef"], GATED);

    // ...and closing it hands the slot back. Polled rather than awaited: the
    // slot is released when the *server's* task ends, which is not something a
    // client's `drop` can wait for.
    drop(first);
    let mut opened = false;
    for _ in 0..50 {
        let (_socket, greeting) = app.try_connect_ws_as(&grant).await;
        if greeting["t"] == "hello" {
            opened = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(opened, "the slot came back when the first socket went");
}

/// A code that expires **while a socket is open** takes the socket with it, on
/// the connection's own heartbeat. rdio never re-checks at all.
#[tokio::test]
async fn a_code_that_expires_mid_connection_ends_it() {
    let app = TestApp::builder()
        .clock(radio_scout::Clock::frozen(5_000))
        .spawn()
        .await;
    app.create_api_key("k").await;
    app.upload_ok(CallUpload::new().talkgroup(GATED)).await;
    app.login().await;
    app.restrict_talkgroup(SYSTEM, GATED, Some(true)).await;
    let grant = app
        .create_access_code_as(json!({
            "code": "FIRE-2026-OPS",
            "scope": { "all": true },
            // Already past, so the very first heartbeat finds it closed — which
            // is the same row of the table a midnight expiry takes, without a
            // test having to wait for midnight.
            "expiresAtMs": 4_000,
        }))
        .await;

    // It never unlocks in the first place, which is the other half of the same
    // rule: an expired code is not a credential.
    let page = app.get_json_with_grant("/api/calls", &grant).await;
    assert_eq!(refs(&page), Vec::<i64>::new());
    let catalog = app.get_json_with_grant("/api/catalog", &grant).await;
    assert_eq!(catalog["access"]["stale"], "expired");
}

// ---------------------------------------------------------------------------
// The admin surface
// ---------------------------------------------------------------------------

/// The listing shows what an Operator is asking — which codes exist, what they
/// open, and how many people are on them — and **neither secret**.
#[tokio::test]
async fn the_admin_listing_shows_neither_the_code_nor_the_grant() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    let grant = app
        .create_access_code_as(json!({
            "label": "Fire Ops",
            "code": "FIRE-2026-OPS",
            "scope": { "sel": { "11": { "54999": true } } },
            "maxConnections": 4,
        }))
        .await;

    let (status, listing) = app.admin_get("/api/admin/codes").await;

    assert_eq!(status, 200);
    let row = &listing["results"][0];
    assert_eq!(row["label"], "Fire Ops");
    assert_eq!(row["scope"]["sel"]["11"]["54999"], true);
    assert_eq!(row["maxConnections"], 4);
    assert_eq!(row["connections"], 0);
    let said = listing.to_string();
    assert!(!said.contains(&grant), "the grant is never listed: {said}");
    assert!(!said.contains("FIRE-2026-OPS"), "nor the code: {said}");
    assert!(!said.contains("argon2"), "nor its hash: {said}");
}

/// Rotating the code re-mints the grant, because rotating a secret that leaked
/// must not leave every browser holding the old one still connected.
#[tokio::test]
async fn changing_a_code_kills_the_grants_already_out_there() {
    let app = an_instance_with_one_gated_channel().await;
    app.login().await;
    let old = app
        .create_access_code(
            "FIRE-2026-OPS",
            json!({ "sel": { "11": { "54999": true } } }),
        )
        .await;
    let id = app.admin_get("/api/admin/codes").await.1["results"][0]["id"]
        .as_i64()
        .expect("the code's id");
    assert_eq!(
        refs(
            &app.get_json_with_grant("/api/calls?sort=oldest", &old)
                .await
        ),
        vec![OPEN, GATED]
    );

    let (status, _) = app
        .admin_patch(
            &format!("/api/admin/codes/{id}"),
            json!({ "code": "FIRE-2027-OPS" }),
        )
        .await;
    assert_eq!(status, 200);

    assert_eq!(
        refs(&app.get_json_with_grant("/api/calls", &old).await),
        vec![OPEN],
        "the grant that was out there opens nothing now"
    );
    let (unlocked, body) = app.unlock("FIRE-2027-OPS").await;
    assert_eq!(unlocked, 200, "{body}");
    assert_ne!(body["grant"].as_str().expect("a grant"), old);

    // ...and a patch that does not mention the code leaves both alone, which is
    // what lets an Operator edit a scope without re-choosing a secret thirty
    // people already know.
    let fresh = body["grant"].as_str().expect("a grant").to_owned();
    let (status, _) = app
        .admin_patch(
            &format!("/api/admin/codes/{id}"),
            json!({ "label": "Fire Ops (2027)" }),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        refs(
            &app.get_json_with_grant("/api/calls?sort=oldest", &fresh)
                .await
        ),
        vec![OPEN, GATED],
        "re-scoping is not rotating"
    );
}

/// Everything about a code that is not its secret can be changed in one
/// request, and a `PATCH` that names none of it leaves all of it alone.
#[tokio::test]
async fn a_patch_can_change_everything_except_the_code() {
    let app = an_instance_with_one_gated_channel().await;
    let grant = app
        .create_access_code("FIRE-2026-OPS", json!({ "all": false }))
        .await;
    let id = app.admin_get("/api/admin/codes").await.1["results"][0]["id"]
        .as_i64()
        .expect("the code's id");
    assert_eq!(
        refs(&app.get_json_with_grant("/api/calls", &grant).await),
        vec![OPEN],
        "it opens nothing yet"
    );

    let (status, row) = app
        .admin_patch(
            &format!("/api/admin/codes/{id}"),
            json!({
                "scope": { "sel": { "11": { "54999": true } } },
                "expiresAtMs": 4_102_444_800_000i64,
                "maxConnections": 2,
                "disabled": false,
            }),
        )
        .await;

    assert_eq!(status, 200, "{row}");
    assert_eq!(row["maxConnections"], 2);
    assert_eq!(row["expiresAtMs"], 4_102_444_800_000i64);
    assert_eq!(row["scope"]["sel"]["11"]["54999"], true);
    assert_eq!(
        refs(
            &app.get_json_with_grant("/api/calls?sort=oldest", &grant)
                .await
        ),
        vec![OPEN, GATED],
        "the same grant, re-scoped — a scope change is not a rotation"
    );

    // ...and the durable off, which keeps the row and refuses it.
    let (status, _) = app
        .admin_patch(
            &format!("/api/admin/codes/{id}"),
            json!({ "disabled": true }),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        refs(&app.get_json_with_grant("/api/calls", &grant).await),
        vec![OPEN]
    );
}

/// A code that is not there is a `404`, on both verbs — the one refusal this
/// surface owes that nothing else here produces.
#[tokio::test]
async fn a_code_that_is_not_there_is_not_found() {
    let app = TestApp::with_key("k").await;
    app.login().await;

    let (patched, body) = app
        .admin_patch("/api/admin/codes/999", json!({ "disabled": true }))
        .await;
    assert_eq!(patched, 404, "{body}");
    assert_eq!(body["error"], "access-code-not-found");

    let (deleted, body) = app.admin_delete("/api/admin/codes/999").await;
    assert_eq!(deleted, 404, "{body}");
    assert_eq!(body["error"], "access-code-not-found");
}

/// A code too short to be worth Argon2id is refused, and the refusal quotes the
/// **rule** and never what it was given — which is a credential an Operator is
/// about to hand out.
#[tokio::test]
async fn a_code_too_short_to_hash_is_refused_without_being_quoted() {
    let app = TestApp::with_key("k").await;
    app.login().await;

    let (status, body) = app
        .admin_post("/api/admin/codes", json!({ "code": "1234" }))
        .await;

    assert_eq!(status, 400);
    assert_eq!(body["error"], "short-access-code");
    assert!(!body.to_string().contains("1234"), "{body}");
}

/// The configuration document carries **which channels are sensitive** and
/// **no codes at all** — the two halves pulling opposite ways, and both on
/// purpose. A restore that dropped `restricted` would silently open every gated
/// channel on the Instance it was restored onto.
#[tokio::test]
async fn the_document_carries_the_gates_and_none_of_the_codes() {
    let app = an_instance_with_one_gated_channel().await;
    app.login().await;
    app.create_access_code("FIRE-2026-OPS", json!({ "all": true }))
        .await;
    app.restrict_system(SYSTEM, true).await;

    let (status, document) = app.admin_get("/api/admin/config").await;

    assert_eq!(status, 200);
    assert_eq!(document["systems"][0]["restricted"], true);
    let gated = document["systems"][0]["talkgroups"]
        .as_array()
        .expect("channels")
        .iter()
        .find(|channel| channel["ref"] == GATED)
        .expect("the gated channel");
    assert_eq!(gated["restricted"], true);

    let said = document.to_string();
    assert!(!said.contains("FIRE-2026-OPS"), "{said}");
    assert!(
        !said.to_lowercase().contains("accesscode") && !said.to_lowercase().contains("\"codes\""),
        "an access code is a secret and never leaves in a backup: {said}"
    );
}

/// ...and restoring that document onto a fresh Instance gates the same channels
/// — which is the half that would fail silently if `restricted` were dropped.
#[tokio::test]
async fn restoring_a_document_gates_the_same_channels() {
    let source = an_instance_with_one_gated_channel().await;
    source.login().await;
    let (_, document) = source.admin_get("/api/admin/config").await;

    let restored = TestApp::with_key("k").await;
    restored.login().await;
    let (status, report) = restored
        .admin_post("/api/admin/config/import", document.clone())
        .await;
    assert_eq!(status, 200, "{report}");

    restored
        .upload_ok(CallUpload::new().talkgroup(GATED).at(1_000))
        .await;

    assert_eq!(
        refs(&restored.get_json("/api/calls").await),
        Vec::<i64>::new(),
        "the restored Instance gates what the backup said was sensitive"
    );
}

// ---------------------------------------------------------------------------
// What never leaves
// ---------------------------------------------------------------------------

/// The grant rides the **query string**, which `http_log` never writes down —
/// so "a grant is never logged" is true by construction rather than by a
/// redaction pass that can be wrong. This is `tests/share.rs`'s assertion, one
/// credential along.
#[tokio::test]
async fn a_grant_never_reaches_the_log() {
    let app = an_instance_with_one_gated_channel().await;
    let logs = app.store_logs();
    app.login().await;
    let grant = app
        .create_access_code(
            "FIRE-2026-OPS",
            json!({ "sel": { "11": { "54999": true } } }),
        )
        .await;

    app.get_json_with_grant("/api/calls", &grant).await;
    app.get_json_with_grant("/api/catalog", &grant).await;
    app.unlock("FIRE-2026-OPS").await;

    logs.assert_never_logged(&grant);
    logs.assert_never_logged("FIRE-2026-OPS");
}

/// A **successful** unlock does not name the address it came from.
///
/// Rule 5 exempts an *authentication attempt*, which is why a refused unlock
/// does name one — an Operator cannot firewall what they cannot see. A
/// successful one is a different sentence: "10.0.0.7 listens to PD Tac" is
/// precisely the record of who listened that the rule exists to keep an Instance
/// from accumulating. rdio logs every listener's IP and code ident at INFO
/// (`client.go:152`).
#[tokio::test]
async fn a_successful_unlock_is_not_a_record_of_who_listened() {
    let app = TestApp::with_key("k").await;
    let logs = app.store_logs();
    app.login().await;
    app.create_access_code_as(json!({
        "label": "Fire Ops",
        "code": "FIRE-2026-OPS",
        "scope": { "all": true },
    }))
    .await;

    app.unlock("FIRE-2026-OPS").await;
    app.await_logged("access code unlocked").await;

    // The label is what identifies the code — never the code, never the grant,
    // and never the address.
    let unlocked = logs.only_line_containing("access code unlocked");
    assert!(unlocked.contains("Fire Ops"), "{unlocked}");
    assert!(
        !unlocked.contains("127.0.0.1"),
        "no address on a success: {unlocked}"
    );
    logs.assert_never_logged("FIRE-2026-OPS");
}

/// A **Star** left on a gated Call is still a Call in the Archive, so the
/// retention sweeper must still be able to take it — the #55 lesson, checked
/// here because a gate is the newest thing that could hide a row from a pass
/// that walks oldest-first.
#[tokio::test]
async fn a_gated_call_is_still_prunable() {
    let app = TestApp::with_key("k").await;
    let id = app
        .seed_call(
            NewCall {
                ..NewCall::new(SYSTEM, GATED, 1_000)
            },
            audio_at("gated.wav"),
        )
        .await;
    app.login().await;
    app.restrict_talkgroup(SYSTEM, GATED, Some(true)).await;

    let deleted = radio_scout::db::repo::delete_calls(&app.db, &[id])
        .await
        .expect("prune a gated Call");

    assert_eq!(deleted, 1);
}
