//! **Quiet spans and the window Catch-up pulls**, driven end to end (#59,
//! spec US 23).
//!
//! The signal processing is unit-tested where it belongs, in
//! `src/quiet/detect.rs`, against synthesized keyups and the two failure shapes
//! that matter — a squelch pop, and a Call that is mostly gap. The failure arms
//! are unit-tested in `src/quiet/worker.rs` over a substituted Archive. What is
//! here is everything that can only be said about a **running Instance**:
//!
//! - a real upload, over real HTTP, with real audio, that is really decoded
//! - that the `200` did not wait for any of it
//! - that an Instance with `[quiet] enabled = false` pays nothing per Call
//! - that the Archive carries the spans on the Call itself
//! - and that `GET /api/calls/quiet` answers for a window of queued Calls,
//!   which is the only way a Call already pushed to a Listener can get them

mod common;

use common::{CallUpload, TWO_KEYUPS_GAPS, TestApp, routine_traffic, two_keyups};
use radio_scout::db::entities::call::QuietState;
use rstest::rstest;

/// A transmission instant nothing else in this file shares — `tests/tone.rs`'s
/// rule and for its reason: two uploads on one channel a few hundred
/// milliseconds apart are one transmission as far as **Ingest** is concerned
/// (#46), so a fixed instant would make the second of any pair a duplicate that
/// was never stored, which reads exactly like scanning having failed.
fn a_fresh_instant() -> i64 {
    static NEXT: std::sync::atomic::AtomicI64 =
        std::sync::atomic::AtomicI64::new(1_669_740_338_000);
    NEXT.fetch_add(600_000, std::sync::atomic::Ordering::Relaxed)
}

async fn an_instance() -> TestApp {
    let app = TestApp::with_key("k").await;
    app.seed_system(11, true, None).await;
    app.seed_talkgroup(11, 54241).await;
    app
}

async fn upload(app: &TestApp, audio: &[u8]) -> i64 {
    let (status, body) = app
        .upload(
            CallUpload::new()
                .system(11)
                .talkgroup(54241)
                .at(a_fresh_instant())
                .audio(audio),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    app.settle().await;
    app.calls().await.last().expect("the Call just stored").id
}

/// The tolerance every edge assertion here uses. The exact edge belongs to
/// `src/quiet/detect.rs`, which owns the analysis window and the guard; what
/// this file is entitled to say is that the gap the fixture *has* is the gap the
/// Instance found.
const SLACK_MS: i64 = 250;

fn near(found: i64, expected: i64) -> bool {
    (found - expected).abs() <= SLACK_MS
}

/// **The whole errand.** A Trunk Recorder call file with two keyups in it goes
/// in over HTTP, and the gap between them comes back on the Call.
#[tokio::test]
async fn a_calls_gaps_are_found_and_carried_on_the_call() {
    let app = an_instance().await;

    let id = upload(&app, &two_keyups()).await;

    let call = app.the_call().await;
    assert_eq!(call.quiet_state, QuietState::DONE);

    let page = app.get_json("/api/calls").await;
    let spans = page["results"][0]["quiet"]
        .as_array()
        .unwrap_or_else(|| panic!("no spans on {page}"))
        .clone();
    assert_eq!(spans.len(), TWO_KEYUPS_GAPS.len(), "{page}");
    for (found, (start_ms, end_ms)) in spans.iter().zip(TWO_KEYUPS_GAPS) {
        let (from, to) = (
            found[0].as_i64().expect("a start"),
            found[1].as_i64().expect("an end"),
        );
        assert!(near(from, start_ms), "{found} should start near {start_ms}");
        assert!(near(to, end_ms), "{found} should end near {end_ms}");
    }
    assert_eq!(page["results"][0]["id"].as_i64(), Some(id));
}

/// Somebody saying one thing has nothing worth trimming, and **carries no key
/// at all** — which is what makes this affordable on every live frame and every
/// search row. Looked at, and found nothing, is `done` all the same: the state
/// says whether it was read, not whether it said anything.
#[tokio::test]
async fn a_call_with_nothing_to_trim_carries_no_key() {
    let app = an_instance().await;

    upload(&app, &routine_traffic()).await;

    assert_eq!(app.the_call().await.quiet_state, QuietState::DONE);
    let page = app.get_json("/api/calls").await;
    assert!(page["results"][0].get("quiet").is_none(), "{page}");
}

/// **The upload never waits for the scan.** Ingest stores, answers and puts the
/// Call on the live feed; the audio is decoded behind that. Asserted as the
/// state the row is in *before* anything settles, which is the only moment the
/// difference is visible from outside.
#[tokio::test]
async fn the_two_hundred_does_not_wait_for_the_scan() {
    let app = an_instance().await;

    let (status, body) = app
        .upload(
            CallUpload::new()
                .system(11)
                .talkgroup(54241)
                .at(a_fresh_instant())
                .audio(&two_keyups()),
        )
        .await;

    assert_eq!(status, 200, "{body}");
    assert!(
        matches!(
            app.the_call().await.quiet_state.as_str(),
            QuietState::PENDING | QuietState::DONE
        ),
        "the Call is queued or already scanned, and either way the recorder has its answer"
    );
}

/// **An Instance that does not scan pays nothing per Call**, and says so in the
/// only place it can: the row is never even marked. Asserted as a *statement
/// count* rather than a state, because "no column was written" and "no work was
/// done" are different claims and only the second is the one being made.
#[tokio::test]
async fn a_disabled_instance_spends_nothing_on_a_call() {
    let app = TestApp::builder()
        .config(|config| config.quiet.enabled = false)
        .spawn()
        .await;
    app.create_api_key("k").await;
    app.seed_system(11, true, None).await;
    app.seed_talkgroup(11, 54241).await;

    upload(&app, &two_keyups()).await;

    let call = app.the_call().await;
    assert_eq!(call.quiet_state, QuietState::NONE);
    assert_eq!(call.quiet, None);
    let page = app.get_json("/api/calls").await;
    assert!(page["results"][0].get("quiet").is_none(), "{page}");
}

/// **The window endpoint, which exists because a live frame cannot carry the
/// answer.** The frame goes out at ingest, before anything has looked at the
/// audio, and nothing republishes one — so this is the only way a Call already
/// sitting in a Listener's queue gets its spans.
///
/// Sparse on purpose: the Call with nothing to trim is absent rather than
/// present and empty, which is what keeps a forty-id question a small reply.
#[tokio::test]
async fn the_window_answers_for_the_calls_that_have_gaps_and_no_others() {
    let app = an_instance().await;
    let with_gaps = upload(&app, &two_keyups()).await;
    let without = upload(&app, &routine_traffic()).await;

    let window = app
        .get_json(&format!(
            "/api/calls/quiet?ids={with_gaps},{without},999999"
        ))
        .await;

    assert_eq!(window.as_object().expect("an object").len(), 1, "{window}");
    let spans = window[with_gaps.to_string()]
        .as_array()
        .unwrap_or_else(|| panic!("no spans on {window}"));
    assert_eq!(spans.len(), TWO_KEYUPS_GAPS.len(), "{window}");
}

/// **One statement for the whole window**, whatever its size — the N+1 #86
/// deleted, on the one request whose whole purpose is to be fast. Asserted as a
/// cost held *equal across two window sizes* rather than as a pinned number,
/// which is `tests/archive.rs`'s rule: the number moves with unrelated work and
/// the property does not.
#[tokio::test]
async fn the_window_costs_the_same_whatever_its_size() {
    let app = an_instance().await;
    let mut ids = Vec::new();
    for _ in 0..4 {
        ids.push(upload(&app, &two_keyups()).await);
    }

    async fn cost(app: &TestApp, ids: &[i64]) -> u64 {
        let query = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let before = app.statements_issued();
        app.get_json(&format!("/api/calls/quiet?ids={query}")).await;
        app.statements_issued() - before
    }

    assert_eq!(cost(&app, &ids[..1]).await, cost(&app, &ids).await);
}

/// A window nobody could have meant is refused, and says which parameter and
/// why — never answered `{}`, which would be a Catch-up that had silently
/// stopped trimming and looked exactly like an Archive of continuous speech.
#[rstest]
#[case::missing("/api/calls/quiet")]
#[case::empty("/api/calls/quiet?ids=")]
#[case::not_ids("/api/calls/quiet?ids=1,banana")]
#[tokio::test]
async fn a_window_nobody_could_have_meant_is_refused(#[case] path: &str) {
    let app = an_instance().await;

    let response = app.get(path).await;

    assert_eq!(response.status(), 400);
    assert!(response.text().await.expect("a body").contains("ids"));
}

/// The ceiling on a list an anonymous caller writes — a bound on a mistake, not
/// a limit anything legitimate reaches.
#[tokio::test]
async fn a_window_larger_than_the_ceiling_is_refused() {
    let app = an_instance().await;
    let ids = (1..=radio_scout::quiet::MAX_WINDOW as i64 + 1)
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");

    let response = app.get(&format!("/api/calls/quiet?ids={ids}")).await;

    assert_eq!(response.status(), 400);
}

/// **A Call whose scan could not happen is still a playable Call.** Catch-up
/// falls back to raising the rate, which is the whole of what "the fallback path
/// works for unenhanced audio" has to mean once the audio cannot be read.
#[tokio::test]
async fn a_call_that_could_not_be_scanned_is_left_playable() {
    let app = an_instance().await;

    upload(&app, b"not audio at all").await;

    let call = app.the_call().await;
    assert_eq!(call.quiet_state, QuietState::SKIPPED);
    assert_eq!(call.quiet, None);
    let page = app.get_json("/api/calls").await;
    assert!(page["results"][0]["audioUrl"].is_string(), "{page}");
}
