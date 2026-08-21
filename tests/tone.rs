//! **Tone-out detection**, driven end to end (#55, spec US 20).
//!
//! The pure halves are unit-tested where they belong — the signal processing in
//! `src/tone/detect.rs` against synthesized page-outs and near-miss negatives,
//! the matching rules in `src/tone/mod.rs` against constructed runs. What is
//! here is everything that can only be said about a **running Instance**:
//!
//! - a real upload, over real HTTP, with real audio, that is really decoded
//! - that the `200` did not wait for any of it
//! - that an Instance with no **Tone profile** pays nothing per Call
//! - that a profile written in the browser applies to the very next Call,
//!   without a restart
//! - that the mark is visible in the Archive and filterable there
//! - and that it reaches a **Webhook**, which is the one place it leaves
//!
//! Nothing here is Listener-facing, and nothing here transcribes anything
//! (ADR-0013, ADR-0014).

mod common;

use common::{CallUpload, Sink, TestApp, page_out, routine_traffic};
use radio_scout::db::entities::call::ToneState;
use rstest::rstest;
use serde_json::json;

/// The two tones the fixture pages with — a real Motorola Quick Call II pair.
const A_HZ: f64 = 1122.5;
const B_HZ: f64 = 1465.6;

/// A transmission instant nothing else in this file shares — `tests/webhook.rs`'s
/// rule and for its reason: two uploads on one channel a few hundred
/// milliseconds apart are one transmission as far as **Ingest** is concerned
/// (#46), so a fixed instant would make the second of any pair a duplicate that
/// was never stored, which reads exactly like detection having failed.
fn a_fresh_instant() -> i64 {
    static NEXT: std::sync::atomic::AtomicI64 =
        std::sync::atomic::AtomicI64::new(1_669_740_338_000);
    NEXT.fetch_add(600_000, std::sync::atomic::Ordering::Relaxed)
}

async fn an_instance() -> TestApp {
    let app = TestApp::spawn().await;
    app.login().await;
    app.create_api_key("k").await;
    app.seed_system(11, true, None).await;
    app.seed_talkgroup(11, 54241).await;
    app
}

/// The internal id of the Talkgroup a profile is written against — which is what
/// the admin surface addresses, since a **Ref** is unique only within a System.
async fn channel(app: &TestApp) -> i64 {
    let (status, listed) = app.admin_get("/api/admin/talkgroups").await;
    assert_eq!(status, 200, "{listed}");
    listed["results"][0]["id"].as_i64().expect("a talkgroup id")
}

/// Write a station's profile through the admin surface an Operator uses — never
/// straight into the table, because *the roster gate re-arming on that request*
/// is half of what this file exists to prove.
async fn write_profile(app: &TestApp, label: &str, steps: serde_json::Value) -> i64 {
    let id = channel(app).await;
    let (status, created) = app
        .admin_post(
            &format!("/api/admin/talkgroups/{id}/tones"),
            json!({ "label": label, "steps": steps }),
        )
        .await;
    assert_eq!(status, 201, "{created}");
    created["id"].as_i64().expect("a profile id")
}

/// Quick Call II as an Operator writes it down.
fn quick_call() -> serde_json::Value {
    json!([
        { "hz": A_HZ, "minMs": 800 },
        { "hz": B_HZ, "minMs": 2000 },
    ])
}

/// Upload one Call with real audio and let everything it set in motion settle.
async fn upload(app: &TestApp, wav: &[u8]) -> i64 {
    let (status, body) = app
        .upload(
            CallUpload::new()
                .key("k")
                .system(11)
                .talkgroup(54241)
                .at(a_fresh_instant())
                .audio(wav),
        )
        .await;
    assert_eq!(status, 200, "the upload should have been accepted: {body}");
    app.settle().await;
    newest(app).await.id
}

/// The Call that arrived last. Several tests here upload twice on purpose —
/// before a profile and after it — so `TestApp::the_call` is deliberately not
/// what they want.
async fn newest(app: &TestApp) -> radio_scout::db::entities::call::Model {
    app.calls().await.pop().expect("a stored Call")
}

// ---------------------------------------------------------------------------
// The mark
// ---------------------------------------------------------------------------

/// **The headline test.** A page-out arrives as ordinary audio on an ordinary
/// upload; a moment later the Call carries the mark, and the Archive says which
/// station was paged and where in the Call.
#[tokio::test]
async fn a_page_out_marks_the_call_and_names_the_station() {
    let app = an_instance().await;
    let profile = write_profile(&app, "Station 12", quick_call()).await;

    let id = upload(&app, &page_out(A_HZ, B_HZ)).await;

    let call = app.get_json(&format!("/api/call/{id}")).await;
    assert_eq!(call["tone"], true, "{call}");
    assert_eq!(call["tones"][0]["label"], "Station 12", "{call}");
    assert!(
        call["tones"][0]["atMs"].as_i64().expect("an offset") < 200,
        "the page is at the top of this Call: {call}"
    );
    assert_eq!(call["tones"][1], serde_json::Value::Null, "one page, once");

    // **The row points back at the profile that fired**, beside the label it
    // snapshotted. The label is what is *shown*; this is what makes a page
    // traceable to the configuration that caught it — an Operator asking "why
    // did this fire?" has nothing else to follow.
    let paged = app.tone_matches(id).await;
    assert_eq!(paged.len(), 1, "{paged:?}");
    assert_eq!(paged[0].profile_id, Some(profile), "{paged:?}");
    assert_eq!(paged[0].label, "Station 12", "{paged:?}");
}

/// The other half, and the one that decides whether an Operator can trust the
/// first: routine traffic on a channel that *has* a profile is not a page.
#[tokio::test]
async fn routine_traffic_on_a_paging_channel_is_not_a_page() {
    let app = an_instance().await;
    write_profile(&app, "Station 12", quick_call()).await;

    let id = upload(&app, &routine_traffic()).await;

    let call = app.get_json(&format!("/api/call/{id}")).await;
    assert_eq!(call["tone"], serde_json::Value::Null, "omitted, not false");
    assert_eq!(
        call["tones"],
        serde_json::Value::Null,
        "omitted too, so no Call pays a key for a page it did not carry: {call}"
    );
    assert_eq!(
        newest(&app).await.tone,
        ToneState::CLEAR,
        "looked at, and clear — which is not the same as never looked at"
    );
}

/// A page on a channel whose profile names somebody else's tones is somebody
/// else's page. The near-miss table in `src/tone/detect.rs` covers the sequence
/// itself; this is the same claim made through a real Instance.
#[tokio::test]
async fn a_page_at_another_stations_tones_marks_nothing() {
    let app = an_instance().await;
    write_profile(&app, "Station 12", quick_call()).await;

    let id = upload(&app, &page_out(602.6, 1122.5)).await;

    let call = app.get_json(&format!("/api/call/{id}")).await;
    assert_eq!(call["tone"], serde_json::Value::Null, "{call}");
}

/// A channel may page several stations, and a sequence that satisfies two
/// profiles marks the Call once and names both — an Operator who wrote a
/// two-tone profile *and* a group-tone profile gets both answers.
#[tokio::test]
async fn a_call_may_page_more_than_one_profile() {
    let app = an_instance().await;
    write_profile(&app, "Station 12", quick_call()).await;
    write_profile(&app, "All Fire", json!([{ "hz": B_HZ, "minMs": 2000 }])).await;

    let id = upload(&app, &page_out(A_HZ, B_HZ)).await;

    let call = app.get_json(&format!("/api/call/{id}")).await;
    let paged: Vec<&str> = call["tones"]
        .as_array()
        .expect("an array")
        .iter()
        .map(|page| page["label"].as_str().expect("a label"))
        .collect();
    assert_eq!(paged, vec!["Station 12", "All Fire"], "{call}");
}

// ---------------------------------------------------------------------------
// What it costs the Instances not using it
// ---------------------------------------------------------------------------

/// **An Instance with no Tone profile pays nothing at all** — not one statement,
/// and not one row touched.
///
/// Asserted as a *difference between two uploads* rather than as a pinned number,
/// which is `tests/webhook.rs`'s arrangement: the point survives every statement
/// ingest gains later, where a number would have to be edited by whoever added
/// one and would stop meaning anything the first time somebody edited it wrong.
#[tokio::test]
async fn an_instance_with_no_profile_spends_nothing_per_call() {
    let app = an_instance().await;

    let before = app.statements_issued();
    upload(&app, &page_out(A_HZ, B_HZ)).await;
    let unarmed = app.statements_issued() - before;

    write_profile(&app, "Station 12", quick_call()).await;
    let before = app.statements_issued();
    upload(&app, &page_out(A_HZ, B_HZ)).await;
    let armed = app.statements_issued() - before;

    assert!(
        armed > unarmed,
        "an armed Instance marks the row and reads the profiles: {unarmed} then {armed}"
    );
    assert_eq!(newest(&app).await.tone, ToneState::MATCHED);
}

/// A Call the recorder sent with no audio — an **Encrypted Call** — is never
/// offered, so a System whose traffic is mostly encrypted does not spend a
/// decode and a WARN per Call discovering the empty object key.
#[tokio::test]
async fn an_encrypted_call_is_never_looked_at() {
    let app = an_instance().await;
    write_profile(&app, "Station 12", quick_call()).await;

    let (status, body) = app
        .upload_tr(CallUpload::tr(&format!(
            r#"{{"short_name":"sys11","talkgroup":54241,
                "start_time":{},"call_length_ms":4000,
                "emergency":0,"encrypted":1}}"#,
            a_fresh_instant() / 1000
        )))
        .await;
    assert_eq!(status, 200, "{body}");
    app.settle().await;

    assert_eq!(
        newest(&app).await.tone,
        ToneState::NONE,
        "never offered, rather than skipped once it was"
    );
}

/// **A profile written now applies to the very next Call.**
///
/// The roster gate is one cached bit, and this is what stops it being a bug: an
/// Operator who has just written their first profile must not have to restart
/// the Instance, and the only thing standing between those two facts is every
/// write re-reading it on its own request.
#[tokio::test]
async fn a_profile_written_now_is_looked_for_in_the_next_call() {
    let app = an_instance().await;

    let unwatched = upload(&app, &page_out(A_HZ, B_HZ)).await;
    write_profile(&app, "Station 12", quick_call()).await;
    let watched = upload(&app, &page_out(A_HZ, B_HZ)).await;

    let before = app.get_json(&format!("/api/call/{unwatched}")).await;
    let after = app.get_json(&format!("/api/call/{watched}")).await;
    assert_eq!(
        before["tone"],
        serde_json::Value::Null,
        "the Archive that came before a profile is left alone"
    );
    assert_eq!(after["tone"], true, "{after}");
}

/// ...and the same in reverse: deleting the last profile stops the looking,
/// without a restart.
#[tokio::test]
async fn deleting_the_last_profile_stops_the_looking() {
    let app = an_instance().await;
    let profile = write_profile(&app, "Station 12", quick_call()).await;

    let (status, removed) = app
        .admin_delete(&format!("/api/admin/tones/{profile}"))
        .await;
    assert_eq!(status, 204, "{removed}");
    upload(&app, &page_out(A_HZ, B_HZ)).await;

    assert_eq!(newest(&app).await.tone, ToneState::NONE);
}

// ---------------------------------------------------------------------------
// Off the ingest path
// ---------------------------------------------------------------------------

/// **A 200 never waits on it** — the ticket's own criterion.
///
/// The upload is answered while the Call is still `pending`, which is a *state*
/// a test can read rather than a duration it has to guess at: a timing assertion
/// here would be a flake on a loaded runner, and would prove nothing on a fast
/// one. `settle()` afterwards is what proves the work really did happen.
#[tokio::test]
async fn the_upload_is_answered_before_anything_is_decoded() {
    let app = an_instance().await;
    write_profile(&app, "Station 12", quick_call()).await;

    let (status, body) = app
        .upload(
            CallUpload::new()
                .key("k")
                .system(11)
                .talkgroup(54241)
                .at(a_fresh_instant())
                .audio(&page_out(A_HZ, B_HZ)),
        )
        .await;

    assert_eq!(status, 200, "{body}");
    assert_eq!(
        newest(&app).await.tone,
        ToneState::PENDING,
        "answered with the audio unread — the whole point of an off-path worker"
    );

    app.settle().await;
    assert_eq!(newest(&app).await.tone, ToneState::MATCHED);
}

/// A restart picks up what the last process had accepted and not finished — and
/// deliberately nothing else. `pending` is this Instance's own unfinished work;
/// `none` is the Archive from before a profile existed, and re-reading that on
/// every boot is what an Operator writing their first profile must not trigger.
#[tokio::test]
async fn a_restart_finishes_what_it_had_already_taken_on() {
    let mut app = an_instance().await;
    write_profile(&app, "Station 12", quick_call()).await;
    let id = upload(&app, &page_out(A_HZ, B_HZ)).await;
    // Put it back the way a process killed mid-detection would have left it,
    // and add a Call from before any profile existed beside it.
    radio_scout::db::repo::mark_tone(&app.db, id, ToneState::PENDING)
        .await
        .expect("put it back the way a killed process left it");
    let untouched = upload(&app, &page_out(A_HZ, B_HZ)).await;
    radio_scout::db::repo::mark_tone(&app.db, untouched, ToneState::NONE)
        .await
        .expect("an archive from before any profile existed");

    app.restart().await;
    app.settle().await;

    let resumed = app.get_json(&format!("/api/call/{id}")).await;
    let before = app.get_json(&format!("/api/call/{untouched}")).await;
    assert_eq!(resumed["tone"], true, "{resumed}");
    assert_eq!(before["tone"], serde_json::Value::Null, "{before}");
}

// ---------------------------------------------------------------------------
// In the Archive
// ---------------------------------------------------------------------------

/// The mark is **filterable**, over the same closed vocabulary a Webhook fires
/// on — so `?mark=tone` answers with the paged Calls and nothing else, and
/// `?mark=emergency` still means what it always did.
#[tokio::test]
async fn the_archive_filters_on_a_mark() {
    let app = an_instance().await;
    write_profile(&app, "Station 12", quick_call()).await;
    let paged = upload(&app, &page_out(A_HZ, B_HZ)).await;
    upload(&app, &routine_traffic()).await;

    let found = app.get_json("/api/calls?mark=tone").await;

    assert_eq!(found["count"], 1, "{found}");
    assert_eq!(found["results"][0]["id"], paged, "{found}");
    assert_eq!(found["results"][0]["tone"], true, "{found}");
    assert_eq!(
        app.get_json("/api/calls?mark=emergency").await["count"],
        0,
        "nobody pressed the button"
    );
    assert_eq!(
        app.get_json("/api/calls").await["count"],
        2,
        "both are there"
    );
}

/// A mark this release has never heard of is **named**, not ignored: a client
/// that asked for one and got an unfiltered page would render it as filtered,
/// and the Listener reading it has no way to tell.
#[tokio::test]
async fn a_mark_this_release_does_not_know_is_refused_by_name() {
    let app = an_instance().await;

    let refused = app.get("/api/calls?mark=dtmf").await;
    let status = refused.status();
    let body = refused.text().await.expect("a body");

    assert_eq!(status, 400, "{body}");
    assert!(
        body.contains("emergency") && body.contains("tone"),
        "{body}"
    );
}

// ---------------------------------------------------------------------------
// Out to a Webhook
// ---------------------------------------------------------------------------

/// **The one place a mark leaves this Instance** (#54): an Operator's own URL,
/// carrying which station was paged — because "a page happened" is not the
/// question somebody with twelve stations on one channel is asking.
#[tokio::test]
async fn a_page_reaches_a_webhook_and_says_which_station() {
    let app = an_instance().await;
    write_profile(&app, "Station 12", quick_call()).await;
    let sink = Sink::start().await;
    app.add_webhook(
        &sink.url(),
        &["tone"],
        json!({ "sel": { "11": { "*": true } } }),
    )
    .await;

    upload(&app, &page_out(A_HZ, B_HZ)).await;
    app.settle().await;

    let received = sink.received();
    assert_eq!(received.len(), 1, "one page, one message: {received:?}");
    assert_eq!(received[0]["marks"], json!(["tone"]));
    assert_eq!(received[0]["call"]["tone"], true);
    // **On the Call, not beside it.** An automation that has parsed a Call from
    // `GET /api/calls` has parsed this one, which is the whole promise of the
    // Radio-Scout body shape — and it means the station name reaches a webhook,
    // a search row and a live frame through one field rather than three.
    assert_eq!(received[0]["call"]["tones"][0]["label"], "Station 12");
}

/// A webhook watching only for Emergencies is not told about a page. The
/// overlap rule is unit-tested; this is it, through a real sink, because the
/// mark that fires this one is decided on a worker minutes after the Call was
/// queued for the other.
#[tokio::test]
async fn a_webhook_watching_for_something_else_is_not_told() {
    let app = an_instance().await;
    write_profile(&app, "Station 12", quick_call()).await;
    let sink = Sink::start().await;
    app.add_webhook(
        &sink.url(),
        &["emergency"],
        json!({ "sel": { "11": { "*": true } } }),
    )
    .await;

    upload(&app, &page_out(A_HZ, B_HZ)).await;
    app.settle().await;

    assert_eq!(sink.count(), 0, "{:?}", sink.received());
}

// ---------------------------------------------------------------------------
// Curating the profiles
// ---------------------------------------------------------------------------

/// **A profile that could never fire is refused**, rather than stored and
/// quietly never matching — which is the failure an Operator cannot see, because
/// a pager that is not being watched looks exactly like a pager that has not
/// gone off.
#[tokio::test]
async fn a_profile_that_could_never_fire_is_refused_with_a_reason() {
    let app = an_instance().await;
    let id = channel(&app).await;

    for (steps, expected) in [
        (json!([]), "at least one tone"),
        (json!([{ "hz": 40.0, "minMs": 800 }]), "outside"),
        (json!([{ "hz": A_HZ, "minMs": 10 }]), "at least"),
    ] {
        let (status, refused) = app
            .admin_post(
                &format!("/api/admin/talkgroups/{id}/tones"),
                json!({ "label": "Station 12", "steps": steps }),
            )
            .await;

        assert_eq!(status, 400, "{refused}");
        assert_eq!(refused["error"], "unusable-tone-profile", "{refused}");
        assert!(
            refused["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains(expected)),
            "{refused}"
        );
    }
}

/// Editing one changes what is looked for, from the next Call on — including
/// through a `PATCH` that only widens the tolerance, which has to be held to the
/// same rule as one that sends the tones too.
#[tokio::test]
async fn editing_a_profile_changes_what_is_looked_for() {
    let app = an_instance().await;
    // Written against the wrong B tone: 2.3% off, outside the default ±2%.
    let profile = write_profile(
        &app,
        "Station 12",
        json!([
            { "hz": A_HZ, "minMs": 800 },
            { "hz": 1499.0, "minMs": 2000 },
        ]),
    )
    .await;
    upload(&app, &page_out(A_HZ, B_HZ)).await;
    assert_eq!(newest(&app).await.tone, ToneState::CLEAR,);

    let (status, patched) = app
        .admin_patch(
            &format!("/api/admin/tones/{profile}"),
            json!({ "steps": [
                { "hz": A_HZ, "minMs": 800 },
                { "hz": B_HZ, "minMs": 2000 },
            ] }),
        )
        .await;
    assert_eq!(status, 200, "{patched}");
    upload(&app, &page_out(A_HZ, B_HZ)).await;

    assert_eq!(newest(&app).await.tone, ToneState::MATCHED);
}

/// Renaming a profile changes what the *next* page is called and leaves the
/// ones already caught saying what they said — which is the whole reason the
/// label on a Call is a snapshot rather than a join.
#[tokio::test]
async fn renaming_a_profile_leaves_the_pages_it_already_caught_alone() {
    let app = an_instance().await;
    let profile = write_profile(&app, "Station 12", quick_call()).await;
    let id = upload(&app, &page_out(A_HZ, B_HZ)).await;

    let (status, patched) = app
        .admin_patch(
            &format!("/api/admin/tones/{profile}"),
            json!({ "label": "Station 14" }),
        )
        .await;
    assert_eq!(status, 200, "{patched}");
    assert_eq!(patched["label"], "Station 14", "{patched}");

    let old = app.get_json(&format!("/api/call/{id}")).await;
    assert_eq!(
        old["tones"][0]["label"], "Station 12",
        "history says what happened, not what the profile is called now: {old}"
    );

    let next = upload(&app, &page_out(A_HZ, B_HZ)).await;
    let now = app.get_json(&format!("/api/call/{next}")).await;
    assert_eq!(now["tones"][0]["label"], "Station 14", "{now}");
}

/// A profile written against a channel that is not there is a **404**, not a
/// row hanging off nothing. The path names the Talkgroup, so this is the one
/// check that has to happen before anything else in the handler.
#[tokio::test]
async fn a_profile_cannot_be_written_against_a_channel_that_is_not_there() {
    let app = an_instance().await;

    let (status, refused) = app
        .admin_post(
            "/api/admin/talkgroups/9999/tones",
            json!({ "label": "Station 12", "steps": quick_call() }),
        )
        .await;

    assert_eq!(status, 404, "{refused}");
    assert_eq!(refused["error"], "talkgroup-not-found", "{refused}");
    // ...and the same for reading, which would otherwise answer an empty list
    // and read as "this channel pages nobody".
    let (status, listed) = app.admin_get("/api/admin/talkgroups/9999/tones").await;
    assert_eq!(status, 404, "{listed}");
}

/// A rename to nothing is refused by name, like every other required field.
#[tokio::test]
async fn a_profile_cannot_be_renamed_to_nothing() {
    let app = an_instance().await;
    let profile = write_profile(&app, "Station 12", quick_call()).await;

    let (status, refused) = app
        .admin_patch(
            &format!("/api/admin/tones/{profile}"),
            json!({ "label": "   " }),
        )
        .await;

    assert_eq!(status, 400, "{refused}");
    assert_eq!(refused["error"], "field-required", "{refused}");
    assert_eq!(refused["field"], "label", "{refused}");
}

/// A disabled profile is kept and not looked for — the switch an Operator
/// reaches for when a profile is paging on somebody else's tones and they want
/// it to stop *now*.
#[tokio::test]
async fn a_disabled_profile_is_kept_and_not_looked_for() {
    let app = an_instance().await;
    let profile = write_profile(&app, "Station 12", quick_call()).await;

    let (status, patched) = app
        .admin_patch(
            &format!("/api/admin/tones/{profile}"),
            json!({ "disabled": true }),
        )
        .await;
    assert_eq!(status, 200, "{patched}");
    upload(&app, &page_out(A_HZ, B_HZ)).await;

    let id = channel(&app).await;
    let (status, listed) = app
        .admin_get(&format!("/api/admin/talkgroups/{id}/tones"))
        .await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(
        listed["results"][0]["disabled"], true,
        "still there: {listed}"
    );
    assert_eq!(
        newest(&app).await.tone,
        ToneState::NONE,
        "and nothing to look for, so nothing was offered"
    );
}

/// The pages a profile already caught survive it being deleted: each row
/// snapshotted the label when it fired, so the Archive keeps saying who was
/// paged after the profile that noticed is gone.
#[tokio::test]
async fn deleting_a_profile_leaves_the_pages_it_already_caught() {
    let app = an_instance().await;
    let profile = write_profile(&app, "Station 12", quick_call()).await;
    let id = upload(&app, &page_out(A_HZ, B_HZ)).await;

    let (status, removed) = app
        .admin_delete(&format!("/api/admin/tones/{profile}"))
        .await;
    assert_eq!(status, 204, "{removed}");

    let call = app.get_json(&format!("/api/call/{id}")).await;
    assert_eq!(call["tone"], true, "{call}");
    assert_eq!(call["tones"][0]["label"], "Station 12", "{call}");
}

// ---------------------------------------------------------------------------
// In the backup
// ---------------------------------------------------------------------------

/// **A backup carries the profiles, and a restore watches the same stations
/// again** (#51).
///
/// A Tone profile is something an Operator sat down and worked out from a
/// recording; a restore that dropped it would leave a station silently
/// unwatched, which looks exactly like a station that has not been paged.
/// Asserted by *moving* to a second Instance and paging it, rather than by
/// reading the file back — a document that round-trips can still be leaning on
/// something local.
#[tokio::test]
async fn a_backup_carries_the_profiles_to_another_instance() {
    let source = an_instance().await;
    write_profile(&source, "Station 12", quick_call()).await;
    let (status, document) = source.admin_get("/api/admin/config").await;
    assert_eq!(status, 200, "{document}");
    assert_eq!(
        document["systems"][0]["talkgroups"][0]["tones"][0]["steps"],
        quick_call(),
        "the sequence travels whole, not as the text of a column: {document}"
    );

    let target = an_instance().await;
    let (status, report) = target
        .admin_post("/api/admin/config/import", document.clone())
        .await;
    assert_eq!(status, 200, "{report}");
    assert_eq!(report["tones"]["created"], 1, "{report}");

    upload(&target, &page_out(A_HZ, B_HZ)).await;
    assert_eq!(newest(&target).await.tone, ToneState::MATCHED);

    // ...and restoring the same file again changes nothing, which is what makes
    // a backup something an Operator can apply without thinking about it.
    let (_, again) = target
        .admin_post("/api/admin/config/import", document.clone())
        .await;
    assert_eq!(again["tones"]["unchanged"], 1, "{again}");
    assert_eq!(again["tones"]["created"], 0, "{again}");
    assert_eq!(again["tones"]["updated"], 0, "{again}");
}

/// **A document that differs in any one field updates rather than being called
/// unchanged**, field by field.
///
/// The report is what an Operator reads to know whether a restore did anything,
/// so "unchanged" has to mean it — a comparison that skipped a field would
/// report a no-op while quietly leaving the old value in place, which is the
/// one way a restore can lie about itself.
#[rstest]
#[case(json!({ "steps": [{ "hz": 979.9, "minMs": 800 }] }), "the tones")]
#[case(json!({ "tolerancePct": 3.5 }), "the tolerance")]
#[case(json!({ "gapMaxMs": 500 }), "the gap")]
#[case(json!({ "disabled": true }), "the switch")]
#[tokio::test]
async fn a_document_that_differs_in_any_field_updates(
    #[case] change: serde_json::Value,
    #[case] what: &str,
) {
    let app = an_instance().await;
    write_profile(&app, "Station 12", quick_call()).await;
    let (_, mut document) = app.admin_get("/api/admin/config").await;

    let entry = &mut document["systems"][0]["talkgroups"][0]["tones"][0];
    for (key, value) in change.as_object().expect("an object") {
        entry[key] = value.clone();
    }
    let (status, report) = app
        .admin_post("/api/admin/config/import", document.clone())
        .await;

    assert_eq!(status, 200, "{report}");
    assert_eq!(report["tones"]["updated"], 1, "{what}: {report}");
    assert_eq!(report["tones"]["unchanged"], 0, "{what}: {report}");
    // ...and it really landed, rather than only being counted.
    let (_, after) = app.admin_get("/api/admin/config").await;
    assert_eq!(
        after["systems"][0]["talkgroups"][0]["tones"][0],
        document["systems"][0]["talkgroups"][0]["tones"][0],
        "{what}"
    );
}

/// A profile in a document that could never fire is **reported** rather than
/// refusing the whole file — a county restore must not be lost to one typo — and
/// it is reported with its path into the file, because that is what an Operator
/// with the document open in an editor can act on.
#[tokio::test]
async fn a_document_names_the_tone_profile_it_could_not_apply() {
    let app = an_instance().await;

    let (status, report) = app
        .admin_post(
            "/api/admin/config/import",
            json!({
                "version": 1,
                "systems": [{
                    "ref": 11,
                    "autoPopulate": true,
                    "talkgroups": [{
                        "ref": 54241,
                        "tones": [{
                            "label": "Station 12",
                            "steps": [{ "hz": 40.0, "minMs": 800 }],
                            "tolerancePct": 2.0,
                            "gapMaxMs": 300,
                        }],
                    }],
                }],
            }),
        )
        .await;

    assert_eq!(status, 200, "{report}");
    assert_eq!(report["tones"]["created"], 0, "{report}");
    assert_eq!(
        report["rejected"][0]["at"], "systems[0].talkgroups[0].tones[0]",
        "{report}"
    );
    assert_eq!(
        report["rejected"][0]["reason"], "unusable-tone-profile",
        "{report}"
    );
}

// ---------------------------------------------------------------------------
// What else in the Instance has to know about a page
// ---------------------------------------------------------------------------

/// **A marked Call still ages out of the Archive.**
///
/// The pages on a Call are child rows behind a `RESTRICT` foreign key, so a
/// Retention pass that did not take them first would fail — and, because the
/// sweep walks oldest-first, it would fail *forever* from the moment the oldest
/// marked Call came due, quietly unbounding an Operator's disk. That is the
/// worst failure this feature could have and nothing else would report it.
#[tokio::test]
async fn a_marked_call_is_still_prunable() {
    let app = an_instance().await;
    write_profile(&app, "Station 12", quick_call()).await;
    let id = upload(&app, &page_out(A_HZ, B_HZ)).await;
    assert_eq!(newest(&app).await.tone, ToneState::MATCHED);

    let config = radio_scout::retention::RetentionConfig {
        days: 1,
        max_size_bytes: None,
        ..Default::default()
    };
    // Well past the window, whatever instant the fixture's Calls carry.
    let now_ms = newest(&app).await.created_at_ms + 30 * 24 * 60 * 60 * 1000;
    let report = radio_scout::retention::sweep(&app.db, app.store.as_ref(), &config, now_ms)
        .await
        .expect("the sweep must not fail on a marked Call");

    assert_eq!(report.aged_out, 1, "{report:?}");
    assert!(app.calls().await.is_empty(), "id {id} should be gone");
}

/// ...and an Operator can still delete the channel it was paged on. The
/// profiles go with it; the pages already on Calls are not this delete's
/// business, because it takes the Calls too.
#[tokio::test]
async fn a_channel_carrying_a_profile_can_still_be_deleted() {
    let app = an_instance().await;
    write_profile(&app, "Station 12", quick_call()).await;
    let id = channel(&app).await;

    let (status, removed) = app
        .admin_delete(&format!("/api/admin/talkgroups/{id}"))
        .await;

    assert_eq!(status, 204, "{removed}");
    let (status, listed) = app.admin_get("/api/admin/talkgroups").await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed["results"][0], serde_json::Value::Null, "{listed}");
}
