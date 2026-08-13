//! Channel merge (#45, spec US 15–18): one Talkgroup answering to many Refs,
//! one Unit owning many radio ids and Ranges.
//!
//! The whole feature is a claim about **resolution**, and resolution happens
//! server-side at ingest before the wire shape is built — so these tests drive
//! the real upload endpoints and read the result back through the surfaces a
//! listener actually sees. Nothing here reaches into a `talkgroup_refs` row to
//! check it: the point of the design is that a member Ref is invisible
//! everywhere except the curation screen, and a test that inspected the join
//! table would pass just as well if the panel showed two channels.

use radio_scout::db::entities::{call, talkgroup, unit, unit_ref};
use radio_scout::db::repo;
use radio_scout::merge;

mod common;
use common::{CallUpload, TestApp, next_json, subscribe};

/// The founding case (spec US 15): a patch-minted dynamic TGID arrives, and the
/// archive already knows it belongs to a real channel. It must not become a
/// second channel, and the Call must be stored against the owner.
#[tokio::test]
async fn a_call_arriving_under_a_member_ref_lands_on_the_owning_talkgroup() {
    let app = TestApp::with_key("k").await;
    app.seed_talkgroup(11, 100).await;
    app.seed_member_ref(11, 100, 8123).await;

    app.upload_ok(CallUpload::new().talkgroup(8123)).await;

    assert_eq!(
        app.count::<talkgroup::Entity>().await,
        1,
        "a member Ref must not auto-populate a channel of its own"
    );
    let call = app.the_call().await;
    assert_eq!(
        app.talkgroup_of(&call).await.r#ref,
        100,
        "the Call belongs to the Talkgroup that owns the Ref it arrived under"
    );
}

/// The fanout half of the same claim (spec US 18): a listener who selected the
/// canonical channel — and has never heard of the member Ref — must be sent the
/// Call. Resolution happens before the wire shape is built, so the frame names
/// the owner and the client's Ref-keyed algebra needs no notion of merging.
#[tokio::test]
async fn a_listener_selecting_the_owner_hears_a_call_that_arrived_under_a_member_ref() {
    let app = TestApp::with_key("k").await;
    app.seed_talkgroup(11, 100).await;
    app.seed_member_ref(11, 100, 8123).await;

    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"100":true}}}"#).await;

    app.upload_ok(CallUpload::new().talkgroup(8123)).await;

    let frame = next_json(&mut ws).await;
    assert_eq!(frame["t"], "call");
    assert_eq!(
        frame["call"]["talkgroupRef"], 100,
        "the wire names the channel, never the Ref it arrived under"
    );
}

/// The panel is the other surface a merge must be invisible on (spec US 15):
/// `GET /api/catalog` offers channels, and a member Ref is not one.
#[tokio::test]
async fn the_catalog_offers_the_owner_and_never_its_member_refs() {
    let app = TestApp::with_key("k").await;
    app.seed_talkgroup(11, 100).await;
    app.seed_member_ref(11, 100, 8123).await;

    app.upload_ok(CallUpload::new().talkgroup(8123)).await;

    let catalog = app.get_json("/api/catalog").await;
    let refs: Vec<i64> = catalog["systems"][0]["talkgroups"]
        .as_array()
        .expect("a talkgroups array")
        .iter()
        .map(|talkgroup| talkgroup["ref"].as_i64().expect("a ref"))
        .collect();
    assert_eq!(
        refs,
        vec![100],
        "one channel in the panel where two Refs reached the instance"
    );
}

/// The churn the feature exists for (spec US 15): a console mints a fresh TGID
/// per patch event, so the *patch* array is where duplicate buttons come from.
/// A patched member Ref must fan out as its owner — otherwise the panel is clean
/// and the patch chips still name numbers nobody selected.
#[tokio::test]
async fn a_patch_ref_that_is_a_member_ref_fans_out_as_its_owner() {
    let app = TestApp::with_key("k").await;
    app.seed_talkgroup(11, 100).await;
    app.seed_member_ref(11, 100, 8123).await;

    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, r#"{"t":"sub","sel":{"11":{"100":true}}}"#).await;

    // A Call on some other channel, patched to the member Ref.
    app.seed_talkgroup(11, 200).await;
    app.upload_ok(CallUpload::new().talkgroup(200).set("patches", "[8123]"))
        .await;

    let frame = next_json(&mut ws).await;
    assert_eq!(
        frame["call"]["patches"],
        serde_json::json!([100]),
        "a patched member Ref reaches the listeners of the channel that owns it"
    );
    assert_eq!(
        app.patch_refs(app.the_call().await.id).await,
        vec![100],
        "and is stored as the owner, so the archive says the same thing"
    );
}

/// **Resolution precedence**: a channel's own number always means itself.
///
/// The two sets are kept disjoint on the way in — the importer refuses a member
/// Ref some Talkgroup already holds as its primary — but a database an operator
/// has edited by hand is not bound by that, and the answer that surprises nobody
/// is the one where a Talkgroup keeps its own Calls. Seeded past the importer on
/// purpose: this pins what resolution does when the invariant is already broken.
#[tokio::test]
async fn a_primary_ref_beats_a_member_ref_claiming_the_same_number() {
    let app = TestApp::with_key("k").await;
    app.seed_talkgroup(11, 100).await;
    app.seed_talkgroup(11, 8123).await;
    app.seed_member_ref(11, 100, 8123).await;

    app.upload_ok(CallUpload::new().talkgroup(8123)).await;

    let call = app.the_call().await;
    assert_eq!(
        app.talkgroup_of(&call).await.r#ref,
        8123,
        "a Talkgroup's own Ref is never resolved away to somebody else's member list"
    );
}

/// The same precedence for Units, where the competing claim is a **Range** that
/// happens to cover another Unit's own Ref — the likelier accident, since a
/// fleet block is written once and a spare radio rostered later.
///
/// Driven against `repo::resolve_unit`: which Unit a Ref resolves to has no
/// visible surface of its own until #47 renders unit labels, and asserting on
/// "no new row was created" would pass whichever Unit won.
#[tokio::test]
async fn a_units_own_ref_beats_a_range_that_covers_it() {
    let app = TestApp::spawn().await;
    app.seed_unit(11, 1200, "Engine 1").await;
    app.seed_unit(11, 1250, "Medic 4").await;
    app.seed_unit_range(11, 1200, 1201, 1299).await;
    let system_id = app
        .unit_by_ref(11, 1200)
        .await
        .expect("the seeded Unit")
        .system_id;

    let owner = repo::resolve_unit(&app.db, system_id, 1250)
        .await
        .expect("resolve")
        .expect("some Unit owns 1250");

    assert_eq!(
        owner.label.as_deref(),
        Some("Medic 4"),
        "the radio that is its own Unit is not swallowed by a block around it"
    );
}

/// Blacklisting the channel blacklists every Ref it answers to (spec US 18 —
/// "blacklists treat merged Refs as one channel"). Otherwise merging a Ref into
/// a blacklisted channel would be a way to smuggle traffic past the operator's
/// own policy.
#[tokio::test]
async fn blacklisting_the_owner_drops_calls_arriving_under_its_member_refs() {
    let app = TestApp::with_key("k").await;
    app.seed_system(11, true, Some("100")).await;
    app.seed_talkgroup(11, 100).await;
    app.seed_member_ref(11, 100, 8123).await;

    let (status, body) = app.upload(CallUpload::new().talkgroup(8123)).await;

    // rdio's contract: a dropped Call still answers success so the recorder
    // never retries (ADR-0001). The log line is the only other record, and
    // `tests/instrumentation.rs` is where that is pinned.
    assert_eq!(status, 200, "{body:?}");
    assert_eq!(
        app.count::<call::Entity>().await,
        0,
        "a member Ref must not be a way around the channel's own blacklist"
    );
}

/// The converse, and the reason the check is two Refs rather than one: an
/// operator who blacklisted `8123` before anyone merged it still means it. A
/// merge is a curation decision about *identity*; it is not a licence to start
/// storing traffic that was refused yesterday.
#[tokio::test]
async fn a_blacklisted_ref_stays_blacklisted_after_it_is_folded_in() {
    let app = TestApp::with_key("k").await;
    app.seed_system(11, true, Some("8123")).await;
    app.seed_talkgroup(11, 100).await;
    app.seed_member_ref(11, 100, 8123).await;

    app.upload(CallUpload::new().talkgroup(8123)).await;
    assert_eq!(
        app.count::<call::Entity>().await,
        0,
        "the Ref the recorder actually sent is still judged"
    );

    // ...and the channel it was merged into is untouched: blacklisting a member
    // Ref is not a way to silence the whole channel by accident.
    app.upload_ok(CallUpload::new().talkgroup(100).at(2000))
        .await;
    assert_eq!(app.count::<call::Entity>().await, 1);
}

/// A patches array names the channels a Call also reaches, and reaching one
/// twice is not a fact about anything. The raw duplicate collapses for the same
/// reason two member Refs of one channel do — this pins that, since the
/// behaviour is a widening of what #81 stored ("duplicates and all").
#[tokio::test]
async fn a_patch_ref_the_recorder_sent_twice_is_stored_once() {
    let app = TestApp::with_key("k").await;
    app.seed_talkgroup(11, 300).await;

    app.upload_ok(CallUpload::new().talkgroup(200).set("patches", "[300,300]"))
        .await;

    assert_eq!(app.patch_refs(app.the_call().await.id).await, vec![300]);
}

/// The archive has to be folded too, not just the panel (spec US 15: patch churn
/// "stops flooding the panel as separate channels").
///
/// A Call archived *before* a fold carries the folded number in its own patch
/// rows. Left alone, it keeps serving a chip for a channel the panel no longer
/// offers — the exact duplicate-button problem this feature exists to remove —
/// and a search for the owner's patched traffic never finds it.
#[tokio::test]
async fn folding_re_points_the_patch_rows_of_calls_already_archived() {
    let app = curating_app().await;
    app.seed_talkgroup(11, 8123).await;
    // A Call on another channel, patched to what is still its own Talkgroup.
    app.upload_ok(
        CallUpload::new()
            .talkgroup(200)
            .at(1000)
            .set("patches", "[8123]"),
    )
    .await;
    let archived = app.the_call().await.id;

    import(&app, "ref,memberRefs\n100,8123\n").await;

    assert_eq!(
        app.patch_refs(archived).await,
        vec![100],
        "the archived Call is patched to the channel that now owns the Ref"
    );
}

/// The same, where the Call was patched to *both* — which is what a console
/// re-broadcasting through a minted TGID actually produces. The two rows become
/// one channel, and must become one row: nothing dedups them on the way out, so
/// a listener would otherwise see the same chip twice.
#[tokio::test]
async fn folding_collapses_a_call_patched_to_both_refs_into_one_row() {
    let app = curating_app().await;
    app.seed_talkgroup(11, 100).await;
    app.seed_talkgroup(11, 8123).await;
    app.upload_ok(
        CallUpload::new()
            .talkgroup(200)
            .at(1000)
            .set("patches", "[100,8123]"),
    )
    .await;
    let archived = app.the_call().await.id;
    assert_eq!(app.patch_refs(archived).await, vec![100, 8123]);

    import(&app, "ref,memberRefs\n100,8123\n").await;

    assert_eq!(
        app.patch_refs(archived).await,
        vec![100],
        "one chip, not two"
    );
}

/// Dedup operates on the canonical entity too (spec US 18). A multi-site System
/// uploading one transmission under two Refs is the plainest reason to merge
/// them, and a merge that left the Call playing twice would have collapsed the
/// panel and fixed nothing a listener can hear.
///
/// This is the existing ±window test made canonical, not #46's widened one:
/// same System, same channel, same instant. Recognising the same transmission
/// across *different* channels, and keeping the better copy, is #46.
#[tokio::test]
async fn one_transmission_arriving_under_two_member_refs_is_stored_once() {
    let app = TestApp::with_key("k").await;
    app.seed_talkgroup(11, 100).await;
    app.seed_member_ref(11, 100, 8123).await;

    app.upload_ok(CallUpload::new().talkgroup(100).at(1000))
        .await;
    let (status, body) = app.upload(CallUpload::new().talkgroup(8123).at(1000)).await;

    assert_eq!(status, 200);
    assert_eq!(body, "duplicate call rejected\n", "the recorder's contract");
    assert_eq!(app.count::<call::Entity>().await, 1);
}

// ---------------------------------------------------------------------------
// Fold and unfold, through the operator's own path (spec US 17)
// ---------------------------------------------------------------------------

/// An app with an admin session open and a key registered, so a test can both
/// upload Calls and curate them.
async fn curating_app() -> TestApp {
    let app = TestApp::with_key("k").await;
    app.login().await;
    app
}

/// POST a CSV and insist it applied.
async fn import(app: &TestApp, csv: &str) -> serde_json::Value {
    let (status, body) = app
        .post_admin_bytes(
            "/api/admin/talkgroups/import?system=11",
            "text/csv",
            csv.as_bytes().to_vec(),
        )
        .await;
    assert_eq!(status, 200, "import failed: {body}");
    serde_json::from_str(&body).expect("a JSON report")
}

/// The Refs the archive holds Calls against, in the order they arrived —
/// asserted through `GET /api/calls` rather than the rows, because a merge that
/// tidied the database and left the archive showing two channels would have
/// missed the point.
async fn searched_refs(app: &TestApp) -> Vec<i64> {
    let page = app.get_json("/api/calls?sort=oldest").await;
    page["results"]
        .as_array()
        .expect("a results array")
        .iter()
        .map(|call| call["talkgroupRef"].as_i64().expect("a talkgroupRef"))
        .collect()
}

/// The heart of spec US 17: an operator discovers, after the fact, that an
/// auto-populated Ref was the same channel all along. Folding it must carry its
/// history across — an archive that loses the Calls is not a merge, it is a
/// delete — and must leave a member Ref behind so the traffic keeps arriving.
#[tokio::test]
async fn folding_a_talkgroup_carries_its_calls_across_and_leaves_a_member_ref() {
    let app = curating_app().await;
    app.upload_ok(CallUpload::new().talkgroup(100).at(1000))
        .await;
    app.upload_ok(CallUpload::new().talkgroup(8123).at(2000))
        .await;
    assert_eq!(app.count::<talkgroup::Entity>().await, 2, "two channels");

    import(&app, "ref,memberRefs\n100,8123\n").await;

    assert_eq!(
        app.count::<talkgroup::Entity>().await,
        1,
        "the folded channel is gone from the panel"
    );
    assert_eq!(app.member_refs(11, 100).await, vec![8123]);
    assert_eq!(
        searched_refs(&app).await,
        vec![100, 100],
        "both Calls now read as the channel that owns them"
    );

    // And the traffic keeps arriving, under the Ref the recorder still sends.
    app.upload_ok(CallUpload::new().talkgroup(8123).at(3000))
        .await;
    assert_eq!(searched_refs(&app).await, vec![100, 100, 100]);
}

/// The round trip (spec US 17): unfolding gives back the channel *and* the Calls
/// that were its own — not the owner's, and not all of them. This is the whole
/// reason a Call records the Ref it arrived under.
#[tokio::test]
async fn unfolding_restores_the_channel_with_exactly_its_own_calls() {
    let app = curating_app().await;
    app.upload_ok(CallUpload::new().talkgroup(100).at(1000))
        .await;
    app.upload_ok(CallUpload::new().talkgroup(8123).at(2000))
        .await;
    import(&app, "ref,memberRefs\n100,8123\n").await;

    // The cell is the whole set for the row it names, so `-` is how an operator
    // writes down the empty one — a blank cell says nothing at all.
    import(&app, "ref,memberRefs\n100,-\n").await;

    assert_eq!(
        app.count::<talkgroup::Entity>().await,
        2,
        "the channel is back"
    );
    assert!(app.member_refs(11, 100).await.is_empty());
    assert_eq!(
        searched_refs(&app).await,
        vec![100, 8123],
        "each Call went home to the channel it arrived under"
    );
}

/// A fold is a curation decision, and an operator gets to see it before it
/// happens: the importer's dry run walks the identical path and rolls back
/// (#18), so a merge previews as exactly what it will do.
#[tokio::test]
async fn a_dry_run_reports_the_fold_and_changes_nothing() {
    let app = curating_app().await;
    app.upload_ok(CallUpload::new().talkgroup(100).at(1000))
        .await;
    app.upload_ok(CallUpload::new().talkgroup(8123).at(2000))
        .await;

    let (status, body) = app
        .post_admin_bytes(
            "/api/admin/talkgroups/import?system=11&dryRun",
            "text/csv",
            b"ref,memberRefs\n100,8123\n".to_vec(),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let report: serde_json::Value = serde_json::from_str(&body).expect("a JSON report");
    assert_eq!(report["dryRun"], true);
    assert_eq!(
        report["callsRepointed"], 1,
        "the preview says how much history would move"
    );

    assert_eq!(
        app.count::<talkgroup::Entity>().await,
        2,
        "nothing was folded"
    );
    assert_eq!(searched_refs(&app).await, vec![100, 8123]);
}

/// Re-importing the same file must be a no-op — the property that makes a CSV
/// safe to keep in version control and re-apply (#18). A merge that folded again
/// on every run would delete a channel it had already absorbed.
#[tokio::test]
async fn re_importing_a_merge_changes_nothing_the_second_time() {
    let app = curating_app().await;
    app.upload_ok(CallUpload::new().talkgroup(100).at(1000))
        .await;
    app.upload_ok(CallUpload::new().talkgroup(8123).at(2000))
        .await;

    import(&app, "ref,memberRefs\n100,8123\n").await;
    let again = import(&app, "ref,memberRefs\n100,8123\n").await;

    assert_eq!(
        again["talkgroupsUnchanged"], 1,
        "the second run recognised there was nothing to do"
    );
    assert_eq!(again["callsRepointed"], 0);
    assert_eq!(app.member_refs(11, 100).await, vec![8123]);
    assert_eq!(searched_refs(&app).await, vec![100, 100]);
}

/// The set is *ordered* — one Ref is the primary and the rest have a sequence
/// an operator chose (#50 renders it). The CSV is the authority on that order
/// too, so re-ordering the cell re-orders the list. It is not a *change*,
/// though: order is presentation, and a file whose merges have all already
/// applied should still report itself as having done nothing.
#[tokio::test]
async fn re_ordering_the_cell_re_orders_the_members_without_counting_as_a_change() {
    let app = curating_app().await;
    app.seed_talkgroup(11, 100).await;

    import(&app, "ref,memberRefs\n100,8123;8124\n").await;
    assert_eq!(app.member_refs(11, 100).await, vec![8123, 8124]);

    let report = import(&app, "ref,memberRefs\n100,8124;8123\n").await;

    assert_eq!(app.member_refs(11, 100).await, vec![8124, 8123]);
    assert_eq!(
        report["talkgroupsUnchanged"], 1,
        "nothing was folded or unfolded"
    );
}

/// Two ways of writing a list that means the same thing. A trailing separator
/// and a doubled one are typing, not data — the `group` column has read them
/// that way since #18, and a merge cell that rejected the row over one would be
/// gratuitous.
#[tokio::test]
async fn empty_entries_in_the_cell_are_punctuation_not_refs() {
    let app = curating_app().await;
    app.seed_talkgroup(11, 100).await;

    import(&app, "ref,memberRefs\n100,8123;;8124; \n").await;

    assert_eq!(app.member_refs(11, 100).await, vec![8123, 8124]);
}

/// A channel listing its own number among its members is redundant, not wrong —
/// it *does* answer to it. Rejecting the row would fail a file that is merely
/// repetitive, and the exported shape #51 will produce is exactly the kind of
/// file that might include it.
#[tokio::test]
async fn a_row_listing_its_own_ref_among_the_members_ignores_it() {
    let app = curating_app().await;
    app.seed_talkgroup(11, 100).await;

    import(&app, "ref,memberRefs\n100,100;8123\n").await;

    assert_eq!(
        app.member_refs(11, 100).await,
        vec![8123],
        "the primary Ref is not one of the members"
    );
    assert_eq!(app.count::<talkgroup::Entity>().await, 1);
}

/// A cell that is not a list of numbers rejects the row rather than applying
/// half of it — the importer's standing promise (#18). Half a merge is the worst
/// outcome available: the operator would have to work out which Refs landed.
#[tokio::test]
async fn a_member_ref_that_is_not_a_number_rejects_the_row() {
    let app = curating_app().await;
    app.seed_talkgroup(11, 100).await;

    let report = import(&app, "ref,memberRefs\n100,8123;Fire Tac\n").await;

    assert_eq!(report["rejected"][0]["reason"], "member-ref-not-a-number");
    assert_eq!(report["rejected"][0]["detail"], "Fire Tac");
    assert!(
        app.member_refs(11, 100).await.is_empty(),
        "not even the Ref that parsed"
    );
}

/// A Ref belongs to one channel. Claiming one that another Talkgroup already
/// holds is rejected with its line, rather than silently stolen — the importer's
/// standing promise (#18), and here the alternative is worse than a bad row: it
/// would break a second operator's channel to fix the first one's.
#[tokio::test]
async fn a_member_ref_another_talkgroup_already_owns_is_rejected() {
    let app = curating_app().await;
    app.seed_talkgroup(11, 100).await;
    app.seed_talkgroup(11, 200).await;
    app.seed_member_ref(11, 100, 8123).await;

    let report = import(&app, "ref,memberRefs\n200,8123\n").await;

    assert_eq!(report["rejected"][0]["line"], 2);
    assert_eq!(
        report["rejected"][0]["reason"],
        "member-ref-owned-elsewhere"
    );
    assert_eq!(
        app.member_refs(11, 100).await,
        vec![8123],
        "the Talkgroup that had it, keeps it"
    );
    assert!(app.member_refs(11, 200).await.is_empty());
}

/// Folding a channel that itself owns member Refs is refused unless the cell
/// names them too.
///
/// Silently carrying them across would make the file stop describing the
/// archive: the cell would say `100` while the channel answered to `100` and
/// `8123`, and the very next re-import of that same file would read the extra
/// member as one the operator had removed — unfolding it. A merge tool whose
/// output re-imports into something else is worse than one that refuses, so it
/// refuses, and names what is missing.
#[tokio::test]
async fn folding_a_channel_that_owns_members_is_refused_until_they_are_named() {
    let app = curating_app().await;
    app.seed_talkgroup(11, 100).await;
    app.seed_talkgroup(11, 200).await;
    import(&app, "ref,memberRefs\n100,8123\n").await;

    let report = import(&app, "ref,memberRefs\n200,100\n").await;

    assert_eq!(report["rejected"][0]["reason"], "member-ref-owns-members");
    assert!(
        report["rejected"][0]["detail"]
            .as_str()
            .expect("a detail")
            .contains("8123"),
        "the operator is told which Ref is missing: {}",
        report["rejected"][0]["detail"]
    );
    assert_eq!(app.member_refs(11, 200).await, Vec::<i64>::new());
    assert_eq!(
        app.member_refs(11, 100).await,
        vec![8123],
        "100 is untouched"
    );
}

/// ...and naming them works, leaving a file that describes what it made — so
/// re-importing it is the no-op the whole importer promises (#18).
#[tokio::test]
async fn a_chain_fold_that_names_every_ref_applies_and_re_imports_clean() {
    let app = curating_app().await;
    app.seed_talkgroup(11, 100).await;
    app.seed_talkgroup(11, 200).await;
    import(&app, "ref,memberRefs\n100,8123\n").await;

    import(&app, "ref,memberRefs\n200,100;8123\n").await;

    assert_eq!(app.member_refs(11, 200).await, vec![100, 8123]);
    assert_eq!(
        app.count::<talkgroup::Entity>().await,
        1,
        "one channel where three Refs arrive"
    );

    let again = import(&app, "ref,memberRefs\n200,100;8123\n").await;
    assert_eq!(again["talkgroupsUnchanged"], 1);
    assert_eq!(app.member_refs(11, 200).await, vec![100, 8123]);
}

/// The stale-export footgun: a county CSV still listing a Ref an operator folded
/// away must not silently unfold it. Naming a member Ref in the `ref` column is
/// rejected, pointing at the owner's row — where an unmerge is something the
/// operator wrote on purpose.
#[tokio::test]
async fn a_row_whose_ref_is_a_member_ref_is_rejected_rather_than_unfolding_it() {
    let app = curating_app().await;
    app.seed_talkgroup(11, 100).await;
    app.seed_member_ref(11, 100, 8123).await;

    let report = import(&app, "ref,label\n8123,Fire Tac 3\n").await;

    assert_eq!(report["rejected"][0]["reason"], "ref-is-a-member-ref");
    assert_eq!(
        app.count::<talkgroup::Entity>().await,
        1,
        "re-importing an old export must not undo a merge"
    );
}

/// Archive search's own filter list (#13) is derived from the Calls rather than
/// from the catalog, so it is a second place a merge could leak: a folded
/// channel whose Calls still remembered it would offer a filter option nobody
/// can see in the panel.
#[tokio::test]
async fn the_search_filters_offer_the_owner_and_never_a_folded_channel() {
    let app = curating_app().await;
    app.upload_ok(CallUpload::new().talkgroup(100).at(1000))
        .await;
    app.upload_ok(CallUpload::new().talkgroup(8123).at(2000))
        .await;

    import(&app, "ref,memberRefs\n100,8123\n").await;

    let filters = app.get_json("/api/calls/filters").await;
    let refs: Vec<i64> = filters["talkgroups"]
        .as_array()
        .expect("a talkgroups array")
        .iter()
        .map(|talkgroup| talkgroup["ref"].as_i64().expect("a ref"))
        .collect();
    assert_eq!(refs, vec![100], "one option for one channel");
}

/// A radio inside a fleet's numbered block (spec US 16). The Range is the Unit's
/// own, so the block's hundred radios are one apparatus rather than a hundred
/// rows nobody named.
#[tokio::test]
async fn a_unit_ref_inside_a_range_belongs_to_the_unit_that_owns_the_range() {
    let app = TestApp::with_key("k").await;
    app.seed_unit(11, 1200, "Engine 1").await;
    app.seed_unit_range(11, 1200, 1201, 1299).await;

    app.upload_ok(
        CallUpload::new()
            .set("unit", 1250)
            .set("talkerAlias", "E1 Portable"),
    )
    .await;

    assert_eq!(
        app.count::<unit::Entity>().await,
        1,
        "a Ref inside an owned Range must not roster a Unit of its own"
    );
}

/// The other half of the same table: a fleet's odd spare radio, owned as a lone
/// member Ref. Stored as a Range of one, which is why Units need only the one
/// table and one lookup.
#[tokio::test]
async fn a_lone_member_ref_belongs_to_the_unit_that_owns_it() {
    let app = TestApp::with_key("k").await;
    app.seed_unit(11, 1200, "Engine 1").await;
    app.seed_unit_range(11, 1200, 4471, 4471).await;

    app.upload_ok(
        CallUpload::new()
            .set("unit", 4471)
            .set("talkerAlias", "Spare"),
    )
    .await;

    assert_eq!(app.count::<unit::Entity>().await, 1);
}

/// Two Ranges claiming one Ref would make ownership depend on which row the
/// database returned first — the same radio attributing to two apparatus
/// depending on the day. The write is refused, and it names the Range in the
/// way, because "that overlaps something" is not actionable on a fleet with
/// forty of them.
///
/// Driven against `repo` rather than HTTP: Unit Ranges have no operator-facing
/// surface until #47's unit CSV, and inventing one here would be a route that
/// ticket then has to restructure.
#[tokio::test]
async fn a_range_overlapping_one_already_owned_is_refused_and_says_which() {
    let app = TestApp::spawn().await;
    app.seed_unit(11, 1200, "Engine 1").await;
    app.seed_unit_range(11, 1200, 1200, 1299).await;
    let unit = app.unit_by_ref(11, 1200).await.expect("the seeded Unit");

    let refused = repo::add_unit_range(&app.db, &unit, merge::Range::new(1250, 1350), 1, 0)
        .await
        .expect("the query itself succeeds");

    assert_eq!(
        refused,
        repo::RangeAdded::Overlaps(merge::Range::new(1200, 1299)),
        "refused, naming the Range in the way"
    );
    assert_eq!(
        app.count::<unit_ref::Entity>().await,
        1,
        "and nothing was written"
    );
}

// ---------------------------------------------------------------------------
// Curated from a browser (#50, spec US 17)
// ---------------------------------------------------------------------------
//
// The same folds as above, reached the other way. #45 gave merging exactly one
// way in — a CSV cell — and #50 is the screen an Operator does it from. What is
// asserted here is that the two paths *agree*: the archive reads back the same
// whichever one moved it, because both go through `repo::set_member_refs` and
// neither has a fold of its own.
//
// The wire is a delta rather than the whole set, for the reason
// `curate::members` gives: a form is submitted by a tab that read the list some
// minutes ago, and inferring an unmerge from absence is the rdio failure this
// surface exists to not repeat.

/// The id of the Talkgroup answering to `primary_ref`, for a path segment.
async fn talkgroup_id(app: &TestApp, primary_ref: i64) -> i64 {
    app.talkgroup_by_ref(11, primary_ref)
        .await
        .expect("the Talkgroup")
        .id
}

/// Fold and unfold from the browser, optionally as a preview.
async fn curate_members(
    app: &TestApp,
    id: i64,
    fold: &[i64],
    unfold: &[i64],
    preview: bool,
) -> (u16, serde_json::Value) {
    let path = match preview {
        true => format!("/api/admin/talkgroups/{id}/members?dryRun"),
        false => format!("/api/admin/talkgroups/{id}/members"),
    };
    app.admin_post(&path, serde_json::json!({ "fold": fold, "unfold": unfold }))
        .await
}

/// The founding case of #50, and the CSV test above rewritten as a form: an
/// Operator discovers churn after the fact and folds it from the screen. The
/// archive has to follow, exactly as it does for the file.
#[tokio::test]
async fn folding_from_the_browser_carries_the_calls_across() {
    let app = curating_app().await;
    app.upload_ok(CallUpload::new().talkgroup(100).at(1000))
        .await;
    app.upload_ok(CallUpload::new().talkgroup(8123).at(2000))
        .await;
    let owner = talkgroup_id(&app, 100).await;

    let (status, report) = curate_members(&app, owner, &[8123], &[], false).await;

    assert_eq!(status, 200, "{report}");
    assert_eq!(report["folded"], 1);
    assert_eq!(report["callsRepointed"], 1);
    assert_eq!(report["dryRun"], false);
    assert_eq!(
        app.count::<talkgroup::Entity>().await,
        1,
        "the folded channel is gone from the panel"
    );
    assert_eq!(app.member_refs(11, 100).await, vec![8123]);
    assert_eq!(searched_refs(&app).await, vec![100, 100]);
}

/// **The preview is the real transaction, rolled back** — #18's dry run, for
/// #18's reason. So the numbers it reports are a promise about the run that
/// follows rather than a separate estimate of it, and this asserts exactly
/// that: the same report twice, and nothing moved in between.
#[tokio::test]
async fn a_preview_reports_what_the_real_fold_then_does_and_changes_nothing() {
    let app = curating_app().await;
    app.upload_ok(CallUpload::new().talkgroup(100).at(1000))
        .await;
    for at in [2000, 3000] {
        app.upload_ok(CallUpload::new().talkgroup(8123).at(at))
            .await;
    }
    let owner = talkgroup_id(&app, 100).await;

    let (status, preview) = curate_members(&app, owner, &[8123], &[], true).await;

    assert_eq!(status, 200, "{preview}");
    assert_eq!(preview["dryRun"], true);
    assert_eq!(preview["callsRepointed"], 2);
    assert_eq!(
        app.count::<talkgroup::Entity>().await,
        2,
        "a preview writes nothing"
    );
    assert!(app.member_refs(11, 100).await.is_empty());
    assert_eq!(searched_refs(&app).await, vec![100, 8123, 8123]);

    // ...and the run it described does exactly that.
    let (_, real) = curate_members(&app, owner, &[8123], &[], false).await;
    assert_eq!(real["folded"], preview["folded"]);
    assert_eq!(real["callsRepointed"], preview["callsRepointed"]);
    assert_eq!(real["moved"], preview["moved"]);
}

/// **A preview names what it is about to destroy.** The counts alone cannot:
/// "1 folded, 412 calls" is the same sentence whichever channel went, and the
/// confirmation step exists so an Operator recognises the one in front of them.
#[tokio::test]
async fn a_preview_names_the_channel_and_the_calls_it_would_take() {
    let app = curating_app().await;
    app.upload_ok(CallUpload::new().talkgroup(100).at(1000))
        .await;
    app.upload_ok(CallUpload::new().talkgroup(8123).at(2000))
        .await;
    let churn = talkgroup_id(&app, 8123).await;
    app.admin_patch(
        &format!("/api/admin/talkgroups/{churn}"),
        serde_json::json!({"label": "TAC 3"}),
    )
    .await;
    let owner = talkgroup_id(&app, 100).await;

    let (_, preview) = curate_members(&app, owner, &[8123], &[], true).await;

    assert_eq!(
        preview["moved"],
        serde_json::json!([{
            "ref": 8123,
            "movement": "folded",
            "label": "TAC 3",
            "calls": 1,
            "carried": [],
        }])
    );
}

/// **A Ref nothing answers to is `recorded`, and the preview says so.**
///
/// This is also what a Ref belonging to *another System* looks like from here —
/// a Ref is unique only within one, so folding another System's number resolves
/// to no channel and simply writes a member Ref down. The counts cannot tell
/// that from a fold; the per-Ref detail can, which is why it is what the
/// confirmation renders.
#[tokio::test]
async fn a_ref_no_channel_answers_to_previews_as_recorded() {
    let app = curating_app().await;
    app.upload_ok(CallUpload::new().talkgroup(100).at(1000))
        .await;
    let owner = talkgroup_id(&app, 100).await;

    let (_, preview) = curate_members(&app, owner, &[8123], &[], true).await;

    assert_eq!(preview["folded"], 0, "nothing was absorbed");
    assert_eq!(preview["callsRepointed"], 0);
    assert_eq!(preview["moved"][0]["movement"], "recorded");
    assert_eq!(preview["moved"][0]["calls"], 0);
    assert_eq!(preview["moved"][0]["label"], serde_json::Value::Null);
}

/// The round trip from the screen: unfolding gives back the channel and exactly
/// the Calls that arrived under it.
#[tokio::test]
async fn unfolding_from_the_browser_sends_the_calls_home() {
    let app = curating_app().await;
    app.upload_ok(CallUpload::new().talkgroup(100).at(1000))
        .await;
    app.upload_ok(CallUpload::new().talkgroup(8123).at(2000))
        .await;
    let owner = talkgroup_id(&app, 100).await;
    curate_members(&app, owner, &[8123], &[], false).await;

    let (status, report) = curate_members(&app, owner, &[], &[8123], false).await;

    assert_eq!(status, 200, "{report}");
    assert_eq!(report["unfolded"], 1);
    assert_eq!(report["callsRepointed"], 1);
    assert_eq!(report["moved"][0]["movement"], "unfolded");
    assert_eq!(app.count::<talkgroup::Entity>().await, 2);
    assert_eq!(searched_refs(&app).await, vec![100, 8123]);
}

/// **A delta says nothing about what it did not name.** The whole reason the
/// wire is not the finished set: a tab that read the members list before
/// somebody else added one must not unfold it by staying silent.
#[tokio::test]
async fn a_fold_leaves_the_members_it_never_mentioned_alone() {
    let app = curating_app().await;
    app.seed_talkgroup(11, 100).await;
    app.seed_member_ref(11, 100, 8123).await;
    let owner = talkgroup_id(&app, 100).await;

    curate_members(&app, owner, &[8124], &[], false).await;

    assert_eq!(
        app.member_refs(11, 100).await,
        vec![8123, 8124],
        "the Ref this request never mentioned is still a member"
    );
}

/// **Bulk folding is the same request.** An Operator selects the churn rows and
/// the real channel, says which survives, and the other Refs are that one
/// request's `fold` list — so spec US 46's "categorizing a county doesn't take
/// an afternoon" holds for merges too, and there is still exactly one writer.
#[tokio::test]
async fn a_county_of_churn_folds_into_one_channel_in_one_request() {
    let app = curating_app().await;
    app.upload_ok(CallUpload::new().talkgroup(100).at(1000))
        .await;
    let churn: Vec<i64> = (8000..8010).collect();
    for (n, ref_) in churn.iter().enumerate() {
        app.upload_ok(CallUpload::new().talkgroup(*ref_).at(2000 + n as i64 * 10))
            .await;
    }
    assert_eq!(app.count::<talkgroup::Entity>().await, 11);
    let owner = talkgroup_id(&app, 100).await;

    let (status, report) = curate_members(&app, owner, &churn, &[], false).await;

    assert_eq!(status, 200, "{report}");
    assert_eq!(report["folded"], 10);
    assert_eq!(report["callsRepointed"], 10);
    assert_eq!(
        report["moved"].as_array().expect("moved").len(),
        10,
        "every Ref is accounted for, one row at a time"
    );
    assert_eq!(
        app.count::<talkgroup::Entity>().await,
        1,
        "the panel is one channel"
    );
    assert_eq!(app.member_refs(11, 100).await, churn);
    assert_eq!(searched_refs(&app).await, vec![100; 11]);
}

/// **A chain fold applies and reports what came with it.**
///
/// The CSV importer refuses this until the cell names every carried Ref, so the
/// file keeps describing what it made and re-importing it does not read them as
/// members the operator removed. A form has no file to round-trip, so the
/// better answer is to apply it and say what arrived — the improvement the
/// delta shape buys.
#[tokio::test]
async fn folding_a_channel_that_owns_members_carries_them_and_says_so() {
    let app = curating_app().await;
    app.seed_talkgroup(11, 100).await;
    app.seed_talkgroup(11, 8123).await;
    app.seed_member_ref(11, 8123, 9000).await;
    let owner = talkgroup_id(&app, 100).await;

    let (status, report) = curate_members(&app, owner, &[8123], &[], false).await;

    assert_eq!(status, 200, "{report}");
    assert_eq!(report["moved"][0]["carried"], serde_json::json!([9000]));
    assert_eq!(
        app.member_refs(11, 100).await,
        vec![8123, 9000],
        "the carried Ref keeps resolving to the surviving channel"
    );
}

/// A Ref another channel holds as a member belongs to that channel's merge, and
/// stealing it would silently break the other one to fix this one — the
/// importer's `member-ref-owned-elsewhere`, as a form's refusal.
#[tokio::test]
async fn a_ref_another_channel_holds_as_a_member_is_refused() {
    let app = curating_app().await;
    app.seed_talkgroup(11, 100).await;
    app.seed_talkgroup(11, 200).await;
    app.seed_member_ref(11, 200, 8123).await;
    let owner = talkgroup_id(&app, 100).await;

    let (status, refused) = curate_members(&app, owner, &[8123], &[], false).await;

    assert_eq!(status, 409, "{refused}");
    assert_eq!(refused["error"], "talkgroup-ref-taken");
    assert_eq!(
        app.member_refs(11, 200).await,
        vec![8123],
        "the other channel's merge is untouched"
    );
}

/// The members list is a read of its own, because the Talkgroup listing
/// deliberately does not carry it (#49) — one query per row on a page of five
/// hundred, for a column that page cannot edit.
#[tokio::test]
async fn the_members_of_a_channel_are_readable_on_their_own() {
    let app = curating_app().await;
    app.seed_talkgroup(11, 100).await;
    app.seed_talkgroup(11, 8123).await;
    let owner = talkgroup_id(&app, 100).await;
    curate_members(&app, owner, &[8123, 9000], &[], false).await;

    let (status, listing) = app
        .admin_get(&format!("/api/admin/talkgroups/{owner}/members"))
        .await;

    assert_eq!(status, 200, "{listing}");
    assert_eq!(
        listing["results"],
        serde_json::json!([
            {"ref": 8123, "label": null},
            {"ref": 9000, "label": null},
        ])
    );
}

// -- A Unit's Ranges, from the browser --------------------------------------

/// Ranges have no preview and need none: a Call names the radios it heard by
/// **Ref**, never by a Unit's Id, so editing a Unit's spans moves no Call. What
/// changes is what an apparatus is *called*, which is exactly what this asserts
/// — a radio inside the new span reads as the fleet.
#[tokio::test]
async fn giving_a_unit_a_range_from_the_browser_names_the_radios_in_it() {
    let app = curating_app().await;
    app.seed_unit(11, 1200, "Engine 1").await;
    let unit = app.unit_by_ref(11, 1200).await.expect("the Unit").id;

    let (status, report) = app
        .admin_post(
            &format!("/api/admin/units/{unit}/ranges"),
            serde_json::json!({"add": [{"from": 1201, "to": 1299}]}),
        )
        .await;

    assert_eq!(status, 200, "{report}");
    assert_eq!(report["added"], 1);
    app.upload_ok(CallUpload::new().talkgroup(100).set("source", 1250))
        .await;
    let call = app.get_json("/api/calls").await;
    assert_eq!(
        call["results"][0]["unitLabel"], "Engine 1",
        "a radio inside the span reads as the apparatus that owns it"
    );
}

/// **A Range edit applies wholly or not at all.** `repo::set_unit_ranges`
/// reports a refused span and applies the rest, which is right for a CSV — the
/// remainder of the file should still land. A form that half-applied is a form
/// whose screen now lies, so this surface rolls back and names the collision.
#[tokio::test]
async fn an_overlapping_range_is_refused_and_takes_the_rest_of_the_edit_with_it() {
    let app = curating_app().await;
    app.seed_unit(11, 1200, "Engine 1").await;
    app.seed_unit(11, 4400, "Ladder 2").await;
    app.seed_unit_range(11, 4400, 4400, 4499).await;
    let unit = app.unit_by_ref(11, 1200).await.expect("the Unit").id;

    let (status, refused) = app
        .admin_post(
            &format!("/api/admin/units/{unit}/ranges"),
            serde_json::json!({"add": [
                {"from": 1, "to": 99},
                {"from": 4450, "to": 4550},
            ]}),
        )
        .await;

    assert_eq!(status, 409, "{refused}");
    assert_eq!(refused["error"], "range-overlaps");
    assert!(
        refused["detail"]
            .as_str()
            .expect("a sentence")
            .contains("4400"),
        "it names the Range in the way: {refused}"
    );
    assert_eq!(
        app.count::<unit_ref::Entity>().await,
        1,
        "the span that would have applied did not"
    );
}

/// Removing a Range un-names the radios it covered, which is the reversible
/// half of the same edit — and the reason this surface needs no `force`.
#[tokio::test]
async fn removing_a_range_from_the_browser_un_names_its_radios() {
    let app = curating_app().await;
    app.seed_unit(11, 1200, "Engine 1").await;
    app.seed_unit_range(11, 1200, 1201, 1299).await;
    let unit = app.unit_by_ref(11, 1200).await.expect("the Unit").id;

    let (status, report) = app
        .admin_post(
            &format!("/api/admin/units/{unit}/ranges"),
            serde_json::json!({"remove": [{"from": 1201, "to": 1299}]}),
        )
        .await;

    assert_eq!(status, 200, "{report}");
    assert_eq!(report["removed"], 1);
    assert_eq!(
        app.admin_get(&format!("/api/admin/units/{unit}/ranges"))
            .await
            .1["results"],
        serde_json::json!([])
    );
}

/// **Naming both halves of a chain fold is not a conflict.** Folding a channel
/// *and* one of the member Refs it holds is one coherent request — the Ref
/// arrives with the channel — and refusing it would make the safest way to
/// write a chain fold down (say everything you mean) the one way that fails.
#[tokio::test]
async fn naming_a_channel_and_the_member_it_holds_is_one_fold() {
    let app = curating_app().await;
    app.seed_talkgroup(11, 100).await;
    app.seed_talkgroup(11, 8123).await;
    app.seed_member_ref(11, 8123, 9000).await;
    let owner = talkgroup_id(&app, 100).await;

    let (status, report) = curate_members(&app, owner, &[8123, 9000], &[], false).await;

    assert_eq!(status, 200, "{report}");
    assert_eq!(app.member_refs(11, 100).await, vec![8123, 9000]);
}

/// The Ranges listing answers with the spans themselves, which is what the
/// editor renders — asserted with one in hand, since an empty list would pass
/// whatever the row shape was.
#[tokio::test]
async fn the_ranges_of_an_apparatus_are_readable_on_their_own() {
    let app = curating_app().await;
    app.seed_unit(11, 1200, "Engine 1").await;
    app.seed_unit_range(11, 1200, 1201, 1299).await;
    let unit = app.unit_by_ref(11, 1200).await.expect("the Unit").id;

    let (status, listing) = app
        .admin_get(&format!("/api/admin/units/{unit}/ranges"))
        .await;

    assert_eq!(status, 200, "{listing}");
    assert_eq!(
        listing["results"],
        serde_json::json!([{"from": 1201, "to": 1299}])
    );
}

/// A Range delta naming nothing is a legitimate request — a form submitted with
/// nothing changed — and changes nothing rather than refusing.
#[tokio::test]
async fn a_range_delta_that_names_nothing_changes_nothing() {
    let app = curating_app().await;
    app.seed_unit(11, 1200, "Engine 1").await;
    app.seed_unit_range(11, 1200, 1201, 1299).await;
    let unit = app.unit_by_ref(11, 1200).await.expect("the Unit").id;

    let (status, report) = app
        .admin_post(
            &format!("/api/admin/units/{unit}/ranges"),
            serde_json::json!({}),
        )
        .await;

    assert_eq!(status, 200, "{report}");
    assert_eq!(report["added"], 0);
    assert_eq!(report["removed"], 0);
    assert_eq!(app.count::<unit_ref::Entity>().await, 1);
}

/// **Writing a Ref down is a change, and the report says so.**
///
/// A `memberRefs` cell naming a Ref no channel answers to inserts a member Ref
/// row — the channel now answers to a number it did not before — so the row is
/// `updated`. It read as `unchanged` until #50 gave [`repo::MergeChange`] its
/// per-Ref detail, which is what made "nothing happened" tell a recorded Ref
/// from a re-import that really did nothing.
///
/// The second half is the one that must not move: re-importing the same file is
/// still a no-op, which is the property the count exists for.
#[tokio::test]
async fn recording_a_ref_no_channel_answers_to_is_reported_as_a_change() {
    let app = curating_app().await;
    app.seed_talkgroup(11, 100).await;

    let first = import(&app, "ref,memberRefs\n100,8123\n").await;

    assert_eq!(first["talkgroupsUpdated"], 1, "{first}");
    assert_eq!(first["talkgroupsUnchanged"], 0, "{first}");
    assert_eq!(first["talkgroupsFolded"], 0, "nothing was absorbed");
    assert_eq!(app.member_refs(11, 100).await, vec![8123]);

    let again = import(&app, "ref,memberRefs\n100,8123\n").await;

    assert_eq!(
        again["talkgroupsUnchanged"], 1,
        "re-importing a curated file is still a no-op: {again}"
    );
    assert_eq!(again["talkgroupsUpdated"], 0, "{again}");
}
