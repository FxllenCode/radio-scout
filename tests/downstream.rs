//! **Downstream** forwarding, driven end to end (#52, spec US 1–2).
//!
//! The peer is a real HTTP server ([`common::Peer`]), so what is asserted here
//! is the multipart that actually left the process, and the queue is the real
//! table, so an outage is an outage. The pure halves — scope, backoff, the retry
//! verdict, the dialect's own bytes — are unit-tested in `src/downstream/`; this
//! file is for the wiring, the durability and the ordering.
//!
//! The headline test is [`a_call_forwarded_to_a_second_instance_arrives_whole`]:
//! two Radio-Scouts, one forwarding to the other, compared row against row. A
//! committed fixture claiming to be what we emit stays green when the *reading*
//! side changes underneath it, which is the failure mode `tests/trplugin.rs`
//! exists to close on the recorder side.

mod common;

use common::{CallUpload, Peer, TestApp, unreachable_url};
use radio_scout::db::entities::call;
use rstest::rstest;
use serde_json::json;

/// Trunk Recorder's meta for an encrypted transmission — a Call that is stored
/// as a flagged, metadata-only row with no audio object at all (spec US 9).
const TR_ENCRYPTED_META: &str = r#"{
  "short_name":"butco","talkgroup":54241,
  "start_time":1669740338,"call_length_ms":4000,
  "emergency":0,"encrypted":1
}"#;

/// Everything on System 11.
fn whole_system() -> serde_json::Value {
    json!({ "sel": { "11": { "*": true } } })
}

/// An app whose retries are quick enough to watch, and a peer for it.
///
/// The backoff is a `Duration` in code and only its *serde* spelling is
/// `_secs`, so a test sets milliseconds directly through the same `Config` an
/// Operator edits — no separate test knob, and no sleeping out a real five
/// seconds to prove a retry happened.
async fn recovering_app() -> (TestApp, Peer) {
    let app = TestApp::builder()
        .config(|config| {
            config.downstream.retry_initial = std::time::Duration::from_millis(5);
            config.downstream.retry_max = std::time::Duration::from_millis(20);
        })
        .spawn()
        .await;
    app.create_api_key("k").await;
    let peer = Peer::start().await;
    (app, peer)
}

// ---------------------------------------------------------------------------
// The dialect, over the wire
// ---------------------------------------------------------------------------

/// A matching Call reaches the peer, in the rdio dialect, with the audio the
/// recorder sent.
#[tokio::test]
async fn a_matching_call_arrives_at_the_peer_in_the_rdio_dialect() {
    let app = TestApp::with_key("k").await;
    let peer = Peer::start().await;
    app.login().await;
    app.add_downstream_as(&peer.url(), "the-peers-key", whole_system())
        .await;

    app.upload_ok(CallUpload::new().set("systemLabel", "Fulton"))
        .await;
    app.settle().await;

    let received = peer.received();
    assert_eq!(received.len(), 1, "one Call, one delivery");
    let call = &received[0];
    assert_eq!(call.field("key"), Some("the-peers-key"), "the peer's key");
    assert_eq!(call.field("system"), Some("11"));
    assert_eq!(call.field("talkgroup"), Some("54241"));
    assert_eq!(call.field("systemLabel"), Some("Fulton"));
    assert_eq!(call.field("timestamp"), Some("1000"));
    assert_eq!(
        call.audio,
        CallUpload::DEFAULT_AUDIO,
        "the recorder's own bytes, unchanged"
    );
}

/// **The move between two Instances** — the test a fixture cannot replace.
///
/// A committed `.multipart` claiming to be what we send stays green forever when
/// the side that *reads* it changes; running both halves is what makes the
/// dialect a contract rather than a snapshot. Everything a Call carries is
/// compared, including the two arrays rdio's own forwarder serializes with Go's
/// struct-field names and therefore loses on every hop.
#[tokio::test]
async fn a_call_forwarded_to_a_second_instance_arrives_whole() {
    let peer = TestApp::with_key("peer-key").await;
    let app = TestApp::with_key("k").await;
    app.login().await;
    app.add_downstream_as(&peer.addr_url(), "peer-key", whole_system())
        .await;

    app.upload_ok(
        CallUpload::new()
            .set("systemLabel", "Fulton County")
            .set("talkgroupLabel", "FD Disp")
            .set("talkgroupTag", "Fire Dispatch")
            .set("talkgroupGroups", "Fire,EMS")
            .set("frequency", "853712500")
            .set("site", "3")
            .set("patches", "[54242]")
            .set(
                "units",
                r#"[{"id":4424000,"label":"MEDIC 7","offset":0},{"id":4424001,"offset":1.5}]"#,
            )
            .set(
                "frequencies",
                r#"[{"freq":853712500,"pos":0,"len":4.25,"errorCount":2}]"#,
            ),
    )
    .await;
    app.settle().await;

    let here = app.the_call().await;
    let there = peer.the_call().await;
    assert_eq!(
        app.system_of(&here).await.r#ref,
        peer.system_of(&there).await.r#ref
    );
    assert_eq!(
        app.talkgroup_of(&here).await.r#ref,
        peer.talkgroup_of(&there).await.r#ref
    );
    assert_eq!(here.call_at_ms, there.call_at_ms, "the same instant");
    assert_eq!(here.frequency, there.frequency);
    assert_eq!(
        app.patch_refs(here.id).await,
        peer.patch_refs(there.id).await
    );
    assert_eq!(
        app.tag_of(&app.talkgroup_of(&here).await).await.name,
        peer.tag_of(&peer.talkgroup_of(&there).await).await.name
    );
    assert_eq!(
        app.units_of(here.id).await,
        peer.units_of(there.id).await,
        "every radio, with its offset and its name — the array rdio's own \
         forwarder emits with Go's field names and its own parser cannot read"
    );
    assert_eq!(
        app.frequencies_of(here.id).await,
        peer.frequencies_of(there.id).await,
        "and the per-frequency signal detail, for the same reason"
    );
    assert_eq!(
        app.site_refs(11).await,
        peer.site_refs(11).await,
        "the tower, which rdio's forwarder omits entirely"
    );
    assert_eq!(
        app.object_bytes(&here.object_key).await,
        peer.object_bytes(&there.object_key).await,
        "the audio, byte for byte"
    );
}

// ---------------------------------------------------------------------------
// Scoping
// ---------------------------------------------------------------------------

/// A peer receives what it is scoped to and nothing else — the same
/// **Selection** algebra the live feed applies, including the exception that
/// lets an Operator forward a System *except* one sensitive channel.
#[rstest]
#[case(json!({ "all": true }), true, "everything")]
#[case(json!({ "sel": { "11": { "*": true } } }), true, "this System")]
#[case(json!({ "sel": { "22": { "*": true } } }), false, "another System")]
#[case(json!({ "sel": { "11": { "54241": true } } }), true, "this Talkgroup")]
#[case(json!({ "sel": { "11": { "999": true } } }), false, "another Talkgroup")]
#[case(json!({}), false, "a peer nobody scoped")]
#[case(
    json!({ "all": true, "sel": { "11": { "54241": false } } }),
    false,
    "an exception to everything"
)]
#[tokio::test]
async fn a_peer_receives_exactly_what_it_is_scoped_to(
    #[case] scope: serde_json::Value,
    #[case] expected: bool,
    #[case] what: &str,
) {
    let app = TestApp::with_key("k").await;
    let peer = Peer::start().await;
    app.login().await;
    app.add_downstream(&peer.url(), scope).await;

    app.upload_ok(CallUpload::new()).await;
    app.settle().await;

    assert_eq!(peer.count() == 1, expected, "{what}");
}

/// **A patched Call reaches a peer scoped to the patched channel** — the defect
/// rdio has and cannot see, because its `HasAccess` compares the Call's own
/// Talkgroup Ref and stops (`downstream.go:99`).
#[tokio::test]
async fn a_patched_call_reaches_a_peer_scoped_to_the_channel_it_was_patched_onto() {
    let app = TestApp::with_key("k").await;
    let peer = Peer::start().await;
    app.login().await;
    // Scoped to 54242 only, which is *not* the channel this Call is on.
    app.add_downstream(&peer.url(), json!({ "sel": { "11": { "54242": true } } }))
        .await;
    // The patched Ref has to be a Talkgroup of this System to be a member at
    // all (#81), so it is seeded before the Call names it.
    app.seed_talkgroup(11, 54242).await;

    app.upload_ok(CallUpload::new().set("patches", "[54242]"))
        .await;
    app.settle().await;

    assert_eq!(peer.count(), 1, "the patch carried it to the peer");
}

/// A disabled peer receives nothing and is owed nothing — switching one off is
/// not a way to accumulate a backlog.
#[tokio::test]
async fn a_disabled_peer_is_neither_sent_to_nor_queued_for() {
    let app = TestApp::with_key("k").await;
    let peer = Peer::start().await;
    app.login().await;
    let id = app.add_downstream(&peer.url(), whole_system()).await;
    let (status, _) = app
        .admin_patch(
            &format!("/api/admin/downstreams/{id}"),
            json!({ "disabled": true }),
        )
        .await;
    assert_eq!(status, 200);

    app.upload_ok(CallUpload::new()).await;
    app.settle().await;

    assert_eq!(peer.count(), 0);
    assert_eq!(
        app.queued_for(id).await,
        0,
        "nothing was written down either"
    );
}

/// An **Encrypted Call** is never queued: it has no audio object at all (spec US
/// 9) and the rdio dialect requires one, so a peer could only ever refuse it.
#[tokio::test]
async fn an_encrypted_call_is_never_forwarded() {
    let app = TestApp::with_key("k").await;
    let peer = Peer::start().await;
    app.login().await;
    let id = app.add_downstream(&peer.url(), whole_system()).await;

    app.upload_tr(CallUpload::tr(TR_ENCRYPTED_META)).await;
    app.settle().await;

    assert!(app.the_call().await.encrypted, "the Call was stored");
    assert_eq!(peer.count(), 0);
    assert_eq!(app.queued_for(id).await, 0);
}

// ---------------------------------------------------------------------------
// Outage, durability and order
// ---------------------------------------------------------------------------

/// **A peer's outage costs delay, not Calls** — and the backlog comes back in
/// the order it arrived.
///
/// This is the whole ticket in one test, and the thing rdio does not do: its
/// forwarder logs the error and drops the Call (`downstream.go:416`).
#[tokio::test]
async fn a_peer_outage_queues_calls_and_drains_them_in_order() {
    let (app, peer) = recovering_app().await;
    app.login().await;
    let id = app.add_downstream(&peer.url(), whole_system()).await;
    peer.answer_with(503);

    for (n, talkgroup) in [101, 102, 103].into_iter().enumerate() {
        app.upload_ok(
            CallUpload::new()
                .talkgroup(talkgroup)
                .at(1_000 + n as i64 * 60_000),
        )
        .await;
    }
    app.settle().await;

    assert_eq!(peer.count(), 0, "the peer took none of them");
    assert_eq!(app.queued_for(id).await, 3, "all three are written down");

    peer.come_back();
    app.deliveries_settled(3).await;

    assert_eq!(
        peer.talkgroups(),
        vec![101, 102, 103],
        "drained in the order they arrived"
    );
    assert_eq!(app.queued_for(id).await, 0);
}

/// **The queue survives a restart of this end**, which is the half the process
/// itself has to carry: the rows are in the database, so the next boot picks up
/// exactly where the last one stopped.
#[tokio::test]
async fn a_restart_forwards_what_the_last_process_could_not() {
    let (mut app, peer) = recovering_app().await;
    app.login().await;
    let id = app.add_downstream(&peer.url(), whole_system()).await;
    peer.answer_with(503);

    app.upload_ok(CallUpload::new()).await;
    app.settle().await;
    assert_eq!(app.queued_for(id).await, 1);

    peer.come_back();
    app.restart().await;
    app.deliveries_settled(1).await;

    assert_eq!(peer.count(), 1, "the Call the last process could not send");
    assert_eq!(app.queued_for(id).await, 0);
}

/// A peer that is not merely refusing but **absent** is the other arm of the
/// retry policy — a transport error with no status to read — and it queues
/// exactly the same way.
#[tokio::test]
async fn an_unreachable_peer_queues_rather_than_losing_the_call() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    let id = app.add_downstream(&unreachable_url(), whole_system()).await;

    app.upload_ok(CallUpload::new()).await;
    app.settle().await;

    assert_eq!(app.queued_for(id).await, 1);
}

/// **A Call the peer will never take is dropped rather than wedging the queue.**
///
/// `417` is rdio's own "Incomplete call data": the same bytes will be refused
/// forever, and retrying them would hold every Call behind this one for as long
/// as the peer exists. The Call itself is untouched — this is a forward that was
/// abandoned, not a Call that was lost.
#[tokio::test]
async fn a_call_the_peer_will_never_accept_is_abandoned_and_the_queue_moves_on() {
    let (app, peer) = recovering_app().await;
    app.login().await;
    let id = app.add_downstream(&peer.url(), whole_system()).await;
    peer.answer_with(417);

    app.upload_ok(CallUpload::new().talkgroup(101)).await;
    app.settle().await;

    assert_eq!(app.queued_for(id).await, 0, "not retried forever");
    assert_eq!(
        app.count::<call::Entity>().await,
        1,
        "the Call is still ours"
    );

    // ...and the peer's queue is not wedged: the next Call goes.
    peer.come_back();
    app.upload_ok(CallUpload::new().talkgroup(102).at(120_000))
        .await;
    app.settle().await;

    assert_eq!(peer.talkgroups(), vec![102]);
}

/// A mistyped key is a `401`, and a `401` **retries**: an Operator fixes it, and
/// the backlog is what makes fixing it worth doing. Abandoning here would empty
/// a county's forwarding queue over a typo.
#[tokio::test]
async fn a_refused_key_keeps_the_backlog_until_the_operator_fixes_it() {
    let (app, peer) = recovering_app().await;
    // **The peer checks the key itself**, which is what a real one does and what
    // makes this deterministic. Switching a peer between statuses cannot: an
    // attempt started before the key was corrected can arrive *after* the peer
    // came back, so the backlog lands under the old key and the assertion below
    // is a coin toss — which is how this test failed once in nine runs, found by
    // `cargo mutants` refusing to proceed on an unmutated tree that would not
    // stay green.
    peer.expect_key("right");
    app.login().await;
    let id = app
        .add_downstream_as(&peer.url(), "wrong", whole_system())
        .await;

    app.upload_ok(CallUpload::new()).await;
    app.settle().await;
    assert_eq!(app.queued_for(id).await, 1, "still owed");
    assert_eq!(peer.count(), 0, "and the peer took none of it");

    let (status, _) = app
        .admin_patch(
            &format!("/api/admin/downstreams/{id}"),
            json!({ "apiKey": "right" }),
        )
        .await;
    assert_eq!(status, 200);
    app.deliveries_settled(1).await;

    let received = peer.received();
    assert_eq!(received.len(), 1, "the backlog survived the typo");
    assert_eq!(received[0].field("key"), Some("right"), "the corrected key");
}

/// **A keep-best replacement is forwarded again** (#46 × #52). The peer is
/// holding the copy this Instance has just decided was the worse one; a peer
/// running Radio-Scout upgrades, and an rdio peer answers `duplicate call
/// rejected` and keeps what it had.
#[tokio::test]
async fn a_better_copy_is_forwarded_to_the_peer_too() {
    let app = TestApp::with_key("k").await;
    let peer = Peer::start().await;
    app.login().await;
    app.add_downstream(&peer.url(), whole_system()).await;

    // A short copy, then a longer one of the same transmission.
    app.upload_ok(CallUpload::new().audio(&common::silence_ms(200)))
        .await;
    app.settle().await;
    app.upload_ok(CallUpload::new().audio(&common::silence_ms(2_000)))
        .await;
    app.settle().await;

    let received = peer.received();
    assert_eq!(received.len(), 2, "the first copy, then the better one");
    assert!(
        received[1].audio.len() > received[0].audio.len(),
        "and the second delivery carried the copy that won"
    );
    assert_eq!(
        app.count::<call::Entity>().await,
        1,
        "one transmission is still one Call here"
    );
}

// ---------------------------------------------------------------------------
// Health, and the cost
// ---------------------------------------------------------------------------

/// The reading an Operator acts on, since #70's status page does not exist yet:
/// queue depth, when it last worked, and how many attempts have failed since.
#[tokio::test]
async fn the_admin_listing_shows_a_peers_health() {
    let (app, peer) = recovering_app().await;
    app.login().await;
    let id = app.add_downstream(&peer.url(), whole_system()).await;
    peer.answer_with(503);

    app.upload_ok(CallUpload::new()).await;
    app.settle().await;

    let (_, body) = app.admin_get("/api/admin/downstreams").await;
    let row = &body["results"][0];
    assert_eq!(row["id"], id);
    assert_eq!(row["queued"], 1, "the durable depth, not the meter's");
    assert!(row["consecutiveFailures"].as_i64().unwrap_or_default() >= 1);
    assert_eq!(row["lastFailure"], "peer-refused (503)");
    assert!(row["lastSuccessMs"].is_null(), "it has never worked");
    assert_eq!(row["hasKey"], true);

    peer.come_back();
    app.deliveries_settled(1).await;

    let (_, body) = app.admin_get("/api/admin/downstreams").await;
    let row = &body["results"][0];
    assert_eq!(row["queued"], 0);
    assert_eq!(row["consecutiveFailures"], 0, "reset by a success");
    assert!(row["lastSuccessMs"].as_i64().is_some());
}

/// **The peer's key never leaves.** Asserted over the whole serialized listing
/// rather than one field, because what leaks a secret is a field somebody adds.
#[tokio::test]
async fn a_peers_key_is_never_returned_by_the_surface() {
    let app = TestApp::spawn().await;
    app.login().await;
    app.add_downstream_as("https://peer.example", "s3cret-peer-key", whole_system())
        .await;

    let (_, body) = app.admin_get("/api/admin/downstreams").await;

    assert!(!body.to_string().contains("s3cret-peer-key"), "{body}");
}

// ---------------------------------------------------------------------------
// The document (#51 × #52)
// ---------------------------------------------------------------------------

/// A backup carries the peer, and **never its key**. The restored row arrives
/// disabled, because a peer with no credential would answer `401` on every Call
/// and look like a network problem for an afternoon.
#[tokio::test]
async fn a_backup_carries_a_peers_shape_and_not_its_key() {
    let app = TestApp::spawn().await;
    app.login().await;
    app.add_downstream_as("https://peer.example", "s3cret-peer-key", whole_system())
        .await;

    let (_, document) = app.admin_get("/api/admin/config").await;
    assert!(
        !document.to_string().contains("s3cret-peer-key"),
        "{document}"
    );
    assert_eq!(document["downstreams"][0]["url"], "https://peer.example");

    // Onto a second Instance, the way a move is really done.
    let restored = TestApp::spawn().await;
    restored.login().await;
    let (status, report) = restored
        .admin_post("/api/admin/config/import", document.clone())
        .await;
    assert_eq!(status, 200, "{report}");
    assert_eq!(report["downstreamsToKey"], 1);

    let (_, body) = restored.admin_get("/api/admin/downstreams").await;
    let row = &body["results"][0];
    assert_eq!(row["url"], "https://peer.example");
    assert_eq!(row["scope"]["sel"], whole_system()["sel"]);
    assert_eq!(row["disabled"], true, "it has no credential yet");
    assert_eq!(row["hasKey"], false, "and the screen says so");
}

/// Importing the same document twice adds nothing, and — the part that would be
/// easy to get wrong — does not switch off a peer that is already working.
#[tokio::test]
async fn re_importing_a_backup_leaves_a_working_peer_alone() {
    let app = TestApp::spawn().await;
    app.login().await;
    let id = app
        .add_downstream_as("https://peer.example", "still-good", whole_system())
        .await;

    let (_, document) = app.admin_get("/api/admin/config").await;
    let (status, report) = app.admin_post("/api/admin/config/import", document).await;
    assert_eq!(status, 200, "{report}");
    assert_eq!(report["downstreamsToKey"], 0, "nothing to re-key");

    let (_, body) = app.admin_get("/api/admin/downstreams").await;
    assert_eq!(body["results"].as_array().expect("rows").len(), 1);
    assert_eq!(body["results"][0]["id"], id, "the same row");
    assert_eq!(body["results"][0]["disabled"], false, "still forwarding");
    assert_eq!(body["results"][0]["hasKey"], true, "still holding its key");
}

// ---------------------------------------------------------------------------
// Curation
// ---------------------------------------------------------------------------

/// Deleting a peer takes its queue with it — and leaves the **Archive** alone,
/// which is the difference between this delete and a System's.
#[tokio::test]
async fn deleting_a_peer_takes_its_queue_and_not_the_archive() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    let id = app.add_downstream(&unreachable_url(), whole_system()).await;

    app.upload_ok(CallUpload::new()).await;
    app.settle().await;
    assert_eq!(app.queued_for(id).await, 1);

    let (status, _) = app
        .admin_delete(&format!("/api/admin/downstreams/{id}"))
        .await;
    assert_eq!(status, 204);

    assert_eq!(app.queued_for(id).await, 0);
    assert_eq!(app.count::<call::Entity>().await, 1, "the Call stays");
}

/// Switching a peer off empties its queue, so switching it back on a week later
/// does not replay the week.
#[tokio::test]
async fn switching_a_peer_off_forgets_what_it_was_owed() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    let id = app.add_downstream(&unreachable_url(), whole_system()).await;

    app.upload_ok(CallUpload::new()).await;
    app.settle().await;
    assert_eq!(app.queued_for(id).await, 1);

    let (status, body) = app
        .admin_patch(
            &format!("/api/admin/downstreams/{id}"),
            json!({ "disabled": true }),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["queued"], 0);
    assert_eq!(app.queued_for(id).await, 0);
}

/// The curation refusals an Operator can actually provoke from the form.
#[rstest]
#[case(json!({ "url": "", "apiKey": "k" }), "field-required")]
#[case(json!({ "url": "https://peer.example", "apiKey": "  " }), "field-required")]
#[tokio::test]
async fn a_peer_missing_something_required_is_refused_by_field(
    #[case] body: serde_json::Value,
    #[case] slug: &str,
) {
    let app = TestApp::spawn().await;
    app.login().await;

    let (status, refused) = app.admin_post("/api/admin/downstreams", body).await;

    assert_eq!(status, 400);
    assert_eq!(refused["error"], slug);
}

/// A peer that is not there is a 404, not a 500.
#[tokio::test]
async fn editing_a_peer_that_is_not_there_is_a_404() {
    let app = TestApp::spawn().await;
    app.login().await;

    let (status, body) = app
        .admin_patch("/api/admin/downstreams/999", json!({ "disabled": true }))
        .await;

    assert_eq!(status, 404);
    assert_eq!(body["error"], "downstream-not-found");
}

/// Relabelling and re-pointing a peer — the two edits that are not the scope or
/// the key. A blank label clears it, the `null` convention every other curation
/// form takes; a new address is where the next delivery goes, with no restart.
#[tokio::test]
async fn a_peer_can_be_relabelled_and_re_pointed() {
    let app = TestApp::with_key("k").await;
    let peer = Peer::start().await;
    app.login().await;
    let id = app
        .add_downstream_as(&unreachable_url(), "the-peers-key", whole_system())
        .await;

    let (status, body) = app
        .admin_patch(
            &format!("/api/admin/downstreams/{id}"),
            json!({ "label": "the mirror", "url": peer.url() }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["label"], "the mirror");
    assert_eq!(body["url"], peer.url());

    app.upload_ok(CallUpload::new()).await;
    app.settle().await;
    assert_eq!(
        peer.count(),
        1,
        "the new address took effect with no restart"
    );

    let (status, body) = app
        .admin_patch(
            &format!("/api/admin/downstreams/{id}"),
            json!({ "label": null }),
        )
        .await;
    assert_eq!(status, 200);
    assert!(body["label"].is_null(), "a cleared label is cleared");
}

/// An address cannot be blanked out from under a working peer — the same
/// field-level refusal a create gives, on the edit that would otherwise leave a
/// peer pointed at nothing.
#[tokio::test]
async fn a_peer_cannot_be_re_pointed_at_nothing() {
    let app = TestApp::spawn().await;
    app.login().await;
    let id = app.add_downstream(&unreachable_url(), whole_system()).await;

    let (status, body) = app
        .admin_patch(
            &format!("/api/admin/downstreams/{id}"),
            json!({ "url": "  " }),
        )
        .await;

    assert_eq!(status, 400);
    assert_eq!(body["error"], "field-required");
}

/// An edit that does not mention the key leaves it alone — which is what makes
/// re-scoping a peer possible without re-typing a credential the screen can
/// never show again.
#[tokio::test]
async fn re_scoping_a_peer_keeps_the_key_it_already_had() {
    let app = TestApp::with_key("k").await;
    let peer = Peer::start().await;
    app.login().await;
    let id = app
        .add_downstream_as(&peer.url(), "the-peers-key", json!({}))
        .await;

    let (status, _) = app
        .admin_patch(
            &format!("/api/admin/downstreams/{id}"),
            json!({ "scope": whole_system() }),
        )
        .await;
    assert_eq!(status, 200);

    app.upload_ok(CallUpload::new()).await;
    app.settle().await;

    let received = peer.received();
    assert_eq!(
        received.len(),
        1,
        "the new scope took effect with no restart"
    );
    assert_eq!(received[0].field("key"), Some("the-peers-key"));
}
