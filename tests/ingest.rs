//! Ingest integration tests (ticket #5): hashed per-system API-key auth,
//! duplicate detection, and full-field persistence — driven over the real HTTP
//! boundary against a DB-backed app.

use radio_scout::IngestConfig;
use radio_scout::db::entities::{
    api_key, call, call_frequency, call_patch, call_unit, system, talkgroup, unit,
};
use radio_scout::db::repo;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

mod common;
use common::{CallUpload, TestApp};

#[tokio::test]
async fn ingest_requires_a_valid_api_key() {
    let app = TestApp::with_key("good-key").await;

    let (status, body) = app.upload(CallUpload::new().key("good-key")).await;
    assert_eq!(status, 200);
    assert!(body.contains("Call imported successfully."), "{body:?}");

    let (status, body) = app
        .upload(CallUpload::new().key("wrong-key").at(2000))
        .await;
    assert_eq!(status, 401);
    assert!(
        body.contains("Invalid API key for system 11 talkgroup 54241."),
        "{body:?}"
    );

    // No key field at all -> rejected.
    let (status, _) = app.upload(CallUpload::new().remove("key")).await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn api_key_is_scoped_to_its_system() {
    let app = TestApp::spawn().await;
    app.create_api_key_for_system("sys11-key", 11).await;

    let (status, _) = app.upload(CallUpload::new().key("sys11-key")).await;
    assert_eq!(status, 200, "key grants its own system");

    let (status, body) = app
        .upload(CallUpload::new().key("sys11-key").system(22))
        .await;
    assert_eq!(status, 401, "key denied for another system");
    assert!(body.contains("Invalid API key for system 22"), "{body:?}");
}

#[tokio::test]
async fn duplicate_calls_within_the_window_are_rejected() {
    let app = TestApp::with_key("k").await;

    let (status, body) = app.upload(CallUpload::new()).await;
    assert_eq!(status, 200);
    assert!(body.contains("Call imported successfully."));

    // Same system+talkgroup at the same time -> duplicate (still HTTP 200).
    let (status, body) = app.upload(CallUpload::new()).await;
    assert_eq!(status, 200);
    assert!(body.contains("duplicate call rejected"), "{body:?}");

    // A different talkgroup is not a duplicate.
    let (status, body) = app.upload(CallUpload::new().talkgroup(99999)).await;
    assert_eq!(status, 200);
    assert!(body.contains("Call imported successfully."));

    // The same talkgroup well outside the ~500ms window is not a duplicate.
    let (status, body) = app.upload(CallUpload::new().at(1000 + 5000)).await;
    assert_eq!(status, 200);
    assert!(body.contains("Call imported successfully."), "{body:?}");
}

/// The window is symmetric, and **both** sides of it have to be tested.
///
/// A recorder catching up after a network blip uploads older Calls after newer
/// ones, so the Call already stored is routinely *later* than the one arriving —
/// which is why the query bounds `call_at_ms` on both sides rather than only
/// looking backwards. Only the backward side was covered, and the nightly
/// mutation sweep found it: widening the forward bound (`call_at_ms + window`
/// becoming `call_at_ms * window`) changed nothing any test could see, because no
/// stored Call was ever ahead of an incoming one.
///
/// Under that mutation an out-of-order upload is swallowed as a duplicate of a
/// Call minutes later, and the recorder is told "imported successfully" — the
/// worst shape of bug this project has, since nothing retries and nothing warns.
#[tokio::test]
async fn a_call_arriving_out_of_order_is_not_a_duplicate_of_a_much_later_one() {
    let app = TestApp::with_key("k").await;

    // The recorder is caught up: a Call 100 seconds ahead lands first. The
    // distance is chosen so it is outside `call_at_ms + window` (1.5 s) while
    // being *inside* `call_at_ms * window` (500 s) — which is exactly the gap the
    // surviving mutant lived in, and why a merely "much later" Call did not
    // close it.
    app.upload_ok(CallUpload::new().at(100_000)).await;

    // Then it backfills one from earlier — same System and Talkgroup, far
    // outside the window on the *forward* side.
    let (status, body) = app.upload(CallUpload::new().at(1_000)).await;

    assert_eq!(status, 200);
    assert!(
        body.contains("Call imported successfully."),
        "an earlier Call is not a duplicate of a later one: {body:?}"
    );
    assert_eq!(app.calls().await.len(), 2, "both Calls stored");
}

#[tokio::test]
async fn ingest_persists_the_full_field_set() {
    let app = TestApp::with_key("k").await;
    // Patch refs persist only for Talkgroups the System knows (#81).
    app.seed_talkgroup(11, 100).await;
    app.seed_talkgroup(11, 200).await;

    app.upload_ok(
        CallUpload::new()
            .audio_named(b"audio-bytes", "call.m4a", "audio/mp4")
            .set("systemLabel", "RSP25MTL")
            .set("talkgroupLabel", "TDB A1")
            .set("talkgroupTag", "Fire dispatch")
            .set("talkgroupGroup", "Fire")
            .set(
                "frequencies",
                r#"[{"freq":774031250,"pos":0,"len":1.5,"dbm":-50,"errorCount":1,"spikeCount":0}]"#,
            )
            .set("sources", r#"[{"src":4424000,"pos":0,"tag":"Engine 1"}]"#)
            .set("patches", "[100, 200]"),
    )
    .await;

    assert_eq!(app.count::<call_frequency::Entity>().await, 1);
    assert_eq!(app.count::<call_unit::Entity>().await, 1);
    assert_eq!(app.count::<call_patch::Entity>().await, 2);
}

/// The stored row carries the audio's byte length (#10). Retention's size cap
/// sums this column instead of stat-ing every object on each sweep — an O(1)
/// query on a Pi, and no per-object network round-trip on an S3 backend.
#[tokio::test]
async fn ingest_records_the_audio_byte_size() {
    let app = TestApp::with_key("k").await;

    app.upload_ok(CallUpload::new().audio_named(&[7u8; 4096], "call.wav", "audio/x-wav"))
        .await;

    assert_eq!(app.the_call().await.audio_size, Some(4096));
}

#[tokio::test]
async fn trunk_recorder_upload_persists_call_and_maps_meta() {
    let app = TestApp::with_key("k").await;
    // Native TR finds its System by matching `short_name` against the label, so
    // the patched Talkgroups (#81) are seeded under that System.
    repo::resolve_or_create_system(&app.db, 1, Some("butco".into()), 0)
        .await
        .expect("seed the short_name's system");
    app.seed_talkgroup(1, 100).await;
    app.seed_talkgroup(1, 200).await;

    let meta = r#"{
      "short_name":"butco","talkgroup":54241,
      "talkgroup_tag":"TDB A1","talkgroup_description":"Fire Dispatch A1",
      "talkgroup_group":"Fire","talkgroup_group_tag":"Fire Dispatch",
      "start_time":1669740338,"freq":774031250,
      "freqList":[{"freq":774031250,"pos":0.25,"len":1.5,"error_count":1,"spike_count":0}],
      "srcList":[{"src":4424000,"pos":0.75,"tag":"Engine 1"},{"src":0,"pos":1.5,"tag":""}],
      "patched_talkgroups":[100,200]
    }"#;

    let (status, body) = app.upload_tr(CallUpload::tr(meta)).await;
    assert_eq!(status, 200, "{body:?}");
    assert!(body.contains("Call imported successfully."));

    // The timestamp is start_time (NOT now) — the bug this ticket guards against.
    let stored = app.the_call().await;
    assert_eq!(
        stored.call_at_ms, 1669740338000,
        "start_time used, not now()"
    );
    // The native TR endpoint records the audio size too (#10).
    assert_eq!(
        stored.audio_size,
        Some(CallUpload::DEFAULT_AUDIO.len() as i64)
    );

    // rdio field mapping: talkgroup_tag->label, description->name, group_tag->tag.
    let tg = app.talkgroup_of(&stored).await;
    assert_eq!(tg.r#ref, 54241);
    assert_eq!(tg.label.as_deref(), Some("TDB A1"));
    assert_eq!(tg.name.as_deref(), Some("Fire Dispatch A1"));
    assert_eq!(app.tag_of(&tg).await.name, "Fire Dispatch");

    // System resolved from short_name.
    assert_eq!(app.system_of(&stored).await.label.as_deref(), Some("butco"));

    // Child rows from freqList / srcList / patched_talkgroups. TR reports
    // `pos`/`len` in **seconds**; the columns are milliseconds, and the values
    // are chosen so the conversion is the only arithmetic that reaches them
    // (#83) — the timeline the Archive draws is wrong by three orders of
    // magnitude if this drifts.
    assert_eq!(app.count::<call_frequency::Entity>().await, 1);
    let freq = call_frequency::Entity::find()
        .one(&app.db)
        .await
        .unwrap()
        .expect("the freqList row");
    assert_eq!(freq.pos_ms, Some(250), "pos 0.25 s -> ms");
    assert_eq!(freq.len_ms, Some(1500), "len 1.5 s -> ms");

    assert_eq!(app.count::<call_patch::Entity>().await, 2);
    // `src: 0` is TR's "no source known for this slice", not a radio: it must
    // not become a Unit on the Call.
    let units = units_of(&app, stored.id).await;
    assert_eq!(units.len(), 1, "a zero src is padding, not a radio");
    assert_eq!(units[0].unit_ref, 4424000);
    assert_eq!(units[0].label.as_deref(), Some("Engine 1"));
    assert_eq!(units[0].offset_ms, Some(750), "pos 0.75 s -> ms");
}

/// The Unit rows recorded against one Call, in a file that now asserts them from
/// three directions (the TR roster and each of the two generic aliases).
async fn units_of(app: &TestApp, call_id: i64) -> Vec<call_unit::Model> {
    call_unit::Entity::find()
        .filter(call_unit::Column::CallId.eq(call_id))
        .all(&app.db)
        .await
        .expect("units")
}

#[tokio::test]
async fn trunk_recorder_missing_talkgroup_is_incomplete() {
    let app = TestApp::with_key("k").await;

    let (status, body) = app
        .upload_tr(CallUpload::tr(
            r#"{"short_name":"butco","start_time":1000}"#,
        ))
        .await;

    assert_eq!(status, 417);
    assert!(
        body.to_lowercase()
            .starts_with("incomplete call data: no talkgroup"),
        "{body:?}"
    );
}

#[tokio::test]
async fn trunk_recorder_converges_with_generic_upload_by_label() {
    let app = TestApp::with_key("k").await;

    // A generic upload creates System ref=11 with label "butco".
    app.upload_ok(
        CallUpload::new()
            .talkgroup(100)
            .set("systemLabel", "butco")
            .audio(b"x"),
    )
    .await;

    // A TR upload with the matching short_name lands on that same System.
    let (status, _) = app
        .upload_tr(
            CallUpload::tr(r#"{"short_name":"butco","talkgroup":200,"start_time":2}"#).audio(b"y"),
        )
        .await;
    assert_eq!(status, 200);

    assert_eq!(
        app.count::<system::Entity>().await,
        1,
        "TR + generic uploads for the same label share one System"
    );
    let sys = system::Entity::find().one(&app.db).await.unwrap().unwrap();
    assert_eq!(sys.r#ref, 11, "TR reuses the generic upload's Ref");
    let calls = app.calls().await;
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|c| c.system_id == sys.id));
}

#[tokio::test]
async fn trunk_recorder_same_short_name_reuses_one_system() {
    let app = TestApp::with_key("k").await;

    // First upload synthesizes a Ref for "newsys"; the second finds it by label.
    app.upload_tr(CallUpload::tr(
        r#"{"short_name":"newsys","talkgroup":1,"start_time":1}"#,
    ))
    .await;
    app.upload_tr(CallUpload::tr(
        r#"{"short_name":"newsys","talkgroup":2,"start_time":2}"#,
    ))
    .await;

    assert_eq!(
        app.count::<system::Entity>().await,
        1,
        "the same short_name maps to one System (stable synthetic Ref)"
    );
}

/// A **disabled** API key is denied even though its hash matches (ADR-0008). This
/// is the load-bearing security branch `authorize_ingest` guards — a revoked key
/// must not ingest.
#[tokio::test]
async fn disabled_api_key_is_rejected() {
    let app = TestApp::spawn().await;
    api_key::ActiveModel {
        key_hash: Set(repo::hash_key("revoked-key")),
        label: Set(None),
        system_ref: Set(None),
        disabled: Set(true),
        created_at_ms: Set(0),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .unwrap();

    let (status, body) = app.upload(CallUpload::new().key("revoked-key")).await;
    assert_eq!(status, 401, "disabled key is denied");
    assert!(
        body.contains("Invalid API key for system 11 talkgroup 54241."),
        "{body:?}"
    );
    // And nothing was stored.
    assert_eq!(app.count::<call::Entity>().await, 0);
}

/// A call with a talkgroup but no audio part is incomplete (417) — checked before
/// auth, so it never touches the DB.
#[tokio::test]
async fn upload_without_audio_is_incomplete() {
    let app = TestApp::with_key("k").await;

    let (status, body) = app.upload(CallUpload::new().no_audio()).await;

    assert_eq!(status, 417);
    assert!(
        body.to_lowercase()
            .starts_with("incomplete call data: no audio"),
        "{body:?}"
    );
}

/// An empty audio part is treated as no audio (417).
#[tokio::test]
async fn empty_audio_is_incomplete() {
    let app = TestApp::with_key("k").await;

    let (status, body) = app.upload(CallUpload::new().audio(b"")).await;

    assert_eq!(status, 417);
    assert!(
        body.to_lowercase()
            .starts_with("incomplete call data: no audio"),
        "{body:?}"
    );
}

/// Trunk Recorder native upload with unparseable `meta` JSON → 417 `Invalid call
/// data` (the exact rdio string), before auth.
#[tokio::test]
async fn trunk_recorder_invalid_meta_json_is_rejected() {
    let app = TestApp::spawn().await;

    let (status, body) = app.upload_tr(CallUpload::tr("this is not json {")).await;

    assert_eq!(status, 417);
    assert_eq!(body, "Invalid call data\n");
}

/// Trunk Recorder native upload with no `meta` part at all → incomplete (417).
#[tokio::test]
async fn trunk_recorder_without_meta_is_incomplete() {
    let app = TestApp::spawn().await;

    let (status, body) = app
        .upload_tr(CallUpload::tr("{}").remove("meta").audio_named(
            b"audio",
            "call.wav",
            "audio/x-wav",
        ))
        .await;

    assert_eq!(status, 417);
    assert!(
        body.to_lowercase()
            .starts_with("incomplete call data: no meta"),
        "{body:?}"
    );
}

/// A patch ref the System has no Talkgroup for is dropped, never stored (#81).
///
/// SDRTrunk builds one unseparated `patches` array as
/// `[<patchgroup>, <talkgroup>…, <radio>…]` — the patched **radio IDs** ride
/// behind the talkgroups with no marker between them
/// (`RdioScannerBroadcaster.java:546-574`), and it sends them under the same
/// field name Trunk Recorder's uploader uses, so neither the field nor the
/// values can say where the talkgroups end. rdio-scanner resolves every patch
/// ref against that System's Talkgroups and skips what does not resolve
/// (`call.go:572-582`); we do the same, so a radio ID is never recorded as a
/// Talkgroup Ref and never routes a Call to a listener subscribed to that
/// number.
#[tokio::test]
async fn patch_refs_the_system_has_no_talkgroup_for_are_dropped() {
    let app = TestApp::with_key("k").await;
    // The System knows 54241 (this Call's own talkgroup, auto-populated on
    // insert) and 54242; 1610051 and 1610092 are radios it has never rostered.
    app.seed_talkgroup(11, 54242).await;

    app.upload_ok(CallUpload::new().set("patches", "[54241, 54242, 1610051, 1610092]"))
        .await;

    let call = app.the_call().await;
    assert_eq!(
        app.patch_refs(call.id).await,
        vec![54241, 54242],
        "the radio IDs trailing the talkgroups are not patch members"
    );
}

/// The honest cost of resolving membership against what the System knows: a
/// patched Talkgroup that has never carried a Call of its own is not yet a
/// Talkgroup, so it is dropped too — rdio-scanner drops it for the same reason
/// (`call.go:572-582`). Auto-populate closes the gap the first time that
/// Talkgroup is heard on its own.
#[tokio::test]
async fn a_patched_talkgroup_the_system_has_never_heard_is_dropped_until_it_is() {
    let app = TestApp::with_key("k").await;

    app.upload_ok(CallUpload::new().set("patches", "[54242]"))
        .await;
    assert!(
        app.patch_refs(app.the_call().await.id).await.is_empty(),
        "an unheard Talkgroup is indistinguishable from a radio id"
    );

    // 54242 carries its own Call, so the System now has a Talkgroup for it...
    app.upload_ok(CallUpload::new().talkgroup(54242).at(2000))
        .await;
    // ...and the next patched Call keeps it.
    app.upload_ok(CallUpload::new().at(3000).set("patches", "[54242]"))
        .await;

    let latest = app.calls().await.pop().expect("the third Call");
    assert_eq!(app.patch_refs(latest.id).await, vec![54242]);
}

/// Membership resolution is one rule for every dialect, and this pins it on
/// Trunk Recorder's side (#81).
///
/// TR's `patched_talkgroups` really are all talkgroups, so it would be possible
/// to trust that list and only classify SDRTrunk's. We deliberately do not:
/// TR's own uploader puts the array in a field literally named `patches`
/// (`rdioscanner_uploader.cc:364`), so on the generic endpoint the two
/// recorders are indistinguishable, and rdio-scanner does not branch by
/// recorder either (`call.go:572-582`). One rule is the only one that can be
/// applied consistently — and the cost lands only on a patched Talkgroup the
/// instance has never heard, which auto-populate then fixes for good.
#[tokio::test]
async fn the_trunk_recorder_patched_talkgroups_path_follows_the_same_rule() {
    let app = TestApp::with_key("k").await;
    repo::resolve_or_create_system(&app.db, 1, Some("butco".into()), 0)
        .await
        .expect("seed the short_name's system");
    app.seed_talkgroup(1, 100).await;

    let (status, body) = app
        .upload_tr(CallUpload::tr(
            r#"{"short_name":"butco","talkgroup":54241,"start_time":1669740338,
                "patched_talkgroups":[100,200]}"#,
        ))
        .await;

    assert_eq!(status, 200, "{body:?}");
    assert_eq!(
        body, "Call imported successfully.\n",
        "the wire contract is untouched"
    );
    assert_eq!(
        app.patch_refs(app.the_call().await.id).await,
        vec![100],
        "100 is a Talkgroup this System has; 200 is not, on this dialect too"
    );
}

/// A zero in Trunk Recorder's `patched_talkgroups` is padding, and stays
/// padding even where the System happens to have a Talkgroup it would resolve
/// against (#83).
///
/// Membership resolution would hide this on any realistic System — nobody has a
/// Talkgroup 0 — which is exactly why the guard needs its own test: the filter
/// says a non-positive entry is *not a Talkgroup Ref at all*, independently of
/// what the System knows, and that is the only rule under which a recorder's
/// padding can never route a Call to a listener.
#[tokio::test]
async fn a_zero_patch_ref_is_padding_even_when_a_talkgroup_would_match_it() {
    let app = TestApp::with_key("k").await;
    repo::resolve_or_create_system(&app.db, 1, Some("butco".into()), 0)
        .await
        .expect("seed the short_name's system");
    app.seed_talkgroup(1, 100).await;
    app.seed_talkgroup(1, 0).await;

    let (status, body) = app
        .upload_tr(CallUpload::tr(
            r#"{"short_name":"butco","talkgroup":54241,"start_time":1669740338,
                "patched_talkgroups":[100,0]}"#,
        ))
        .await;

    assert_eq!(status, 200, "{body:?}");
    assert_eq!(
        app.patch_refs(app.the_call().await.id).await,
        vec![100],
        "0 never becomes a patch member"
    );
}

/// The rdio-compatible field aliases are honored: `patched_talkgroups` (== the
/// `patches` array) and `audioType`/`audioName` (the MIME + filename carried as
/// form fields rather than on the audio part).
#[tokio::test]
async fn field_aliases_are_accepted() {
    let app = TestApp::with_key("k").await;
    app.seed_talkgroup(11, 100).await;
    app.seed_talkgroup(11, 200).await;

    // Audio part carries neither filename nor MIME; the fields supply both.
    // Order matches real recorders (Trunk Recorder): the audio part comes first,
    // then the metadata fields, so the fields win (last-write).
    app.upload_ok(
        CallUpload::new()
            .audio_first()
            .audio_unlabelled(b"audio-bytes")
            .set("patched_talkgroups", "[100, 200]")
            .set("audioType", "audio/mpeg")
            .set("audioName", "clip.mp3"),
    )
    .await;

    assert_eq!(
        app.count::<call_patch::Entity>().await,
        2,
        "patched_talkgroups is the patches alias"
    );
    let stored = app.the_call().await;
    assert_eq!(
        stored.audio_mime.as_deref(),
        Some("audio/mpeg"),
        "audioType"
    );
    assert_eq!(stored.audio_name.as_deref(), Some("clip.mp3"), "audioName");
}

/// `talkgroupGroups` — the comma-separated plural — is a field of its own, not
/// a spelling of `talkgroupGroup` (#83).
///
/// rdio accepts both and a recorder may send either; the plural is the one no
/// other test exercises, so dropping it would show up as Talkgroups that
/// quietly stop appearing under their category in the Talkgroups panel.
#[tokio::test]
async fn the_plural_talkgroup_groups_field_lands() {
    let app = TestApp::with_key("k").await;

    app.upload_ok(
        CallUpload::new()
            .remove("talkgroupGroup")
            .set("talkgroupGroups", "Fire, Law"),
    )
    .await;

    let tg = app.talkgroup_of(&app.the_call().await).await;
    let mut groups = repo::groups_for_talkgroup(&app.db, tg.id)
        .await
        .expect("groups");
    groups.sort();
    assert_eq!(groups, vec!["Fire".to_string(), "Law".to_string()]);
}

/// `units` — the JSON roster — carries a Ref, a label and an offset per Unit.
#[tokio::test]
async fn the_units_roster_alias_lands_with_labels_and_offsets() {
    let app = TestApp::with_key("k").await;

    app.upload_ok(CallUpload::new().set(
        "units",
        r#"[{"id":4424000,"label":"Engine 1","offset":1.25}]"#,
    ))
    .await;

    let units = units_of(&app, app.the_call().await.id).await;
    assert_eq!(units.len(), 1, "the units roster is read");
    assert_eq!(units[0].unit_ref, 4424000);
    assert_eq!(units[0].label.as_deref(), Some("Engine 1"));
    assert_eq!(units[0].offset_ms, Some(1250), "offset 1.25 s -> ms");
}

/// `unit` — the singular Ref a recorder sends when it knows who keyed up but
/// nothing else — is the last of the three roster spellings.
#[tokio::test]
async fn the_singular_unit_alias_lands() {
    let app = TestApp::with_key("k").await;

    app.upload_ok(CallUpload::new().set("unit", "4424001"))
        .await;

    let units = units_of(&app, app.the_call().await.id).await;
    assert_eq!(units.len(), 1, "the singular unit is read");
    assert_eq!(units[0].unit_ref, 4424001);
    assert_eq!(units[0].offset_ms, None, "no offset was sent");
}

// ---------------------------------------------------------------------------
// Auto-populate + blacklist over the real HTTP boundary (#8).
// ---------------------------------------------------------------------------

/// A blacklisted Talkgroup is dropped — but the recorder still gets HTTP 200
/// `Call imported successfully.` so it never retries.
#[tokio::test]
async fn blacklisted_talkgroup_is_dropped_but_reports_success() {
    let app = TestApp::with_key("k").await;
    app.seed_system(11, false, Some("54241")).await;

    app.upload_ok(CallUpload::new()).await;
    assert_eq!(
        app.count::<call::Entity>().await,
        0,
        "blacklisted call not stored"
    );

    // A different talkgroup on the same system is ingested normally.
    app.upload_ok(CallUpload::new().talkgroup(99999)).await;
    assert_eq!(app.count::<call::Entity>().await, 1);
}

/// With the global toggle off, a Call for an unknown System is dropped (nothing to
/// attach it to) — still HTTP 200, nothing stored.
#[tokio::test]
async fn auto_populate_off_drops_unknown_system() {
    let app = auto_populate_off().await;

    app.upload_ok(CallUpload::new()).await;

    assert_eq!(app.count::<system::Entity>().await, 0);
    assert_eq!(app.count::<call::Entity>().await, 0);
}

/// With the global toggle off, a System that opts in per-system still auto-creates
/// unknown Talkgroups under it.
#[tokio::test]
async fn per_system_auto_populate_overrides_global_off() {
    let app = auto_populate_off().await;
    app.seed_system(11, true, None).await; // opts in

    app.upload_ok(CallUpload::new()).await;

    assert_eq!(
        app.count::<call::Entity>().await,
        1,
        "opted-in system auto-creates the talkgroup"
    );
    let tg = talkgroup::Entity::find()
        .filter(talkgroup::Column::Ref.eq(54241))
        .one(&app.db)
        .await
        .unwrap();
    assert!(tg.is_some(), "unknown talkgroup was auto-populated");
}

/// An app with the global auto-populate toggle off and the `k` key registered.
async fn auto_populate_off() -> TestApp {
    let app = TestApp::builder()
        .ingest(IngestConfig {
            auto_populate: false,
            ..Default::default()
        })
        .spawn()
        .await;
    app.create_api_key("k").await;
    app
}

/// End-to-end auto-populate defaults over the generic upload: a bare Call with a
/// heard source yields rdio's default labels/tag/group and rosters the Unit.
#[tokio::test]
async fn auto_populate_defaults_persist_over_http() {
    let app = TestApp::with_key("k").await;

    app.upload_ok(CallUpload::new().set("sources", r#"[{"src":4242,"pos":0,"tag":"Medic 7"}]"#))
        .await;

    let stored = app.the_call().await;
    assert_eq!(
        app.system_of(&stored).await.label.as_deref(),
        Some("System 11")
    );
    let tg = app.talkgroup_of(&stored).await;
    assert_eq!(tg.label.as_deref(), Some("54241"));
    assert_eq!(tg.name.as_deref(), Some("Talkgroup 54241"));
    assert_eq!(app.tag_of(&tg).await.name, "Untagged");
    assert_eq!(
        repo::groups_for_talkgroup(&app.db, tg.id).await.unwrap(),
        vec!["Unknown".to_string()]
    );
    let rostered = unit::Entity::find().one(&app.db).await.unwrap().unwrap();
    assert_eq!(rostered.r#ref, 4242);
    assert_eq!(rostered.label.as_deref(), Some("Medic 7"));
}

/// A generic upload with no numeric `system` (absent, or a non-positive value)
/// gets the lowest-free System Ref (#8), not a bogus Ref 0.
#[rstest::rstest]
#[case::absent(None)]
#[case::zero(Some("0"))]
#[case::negative(Some("-4"))]
#[tokio::test]
async fn generic_upload_without_positive_system_gets_lowest_free_ref(
    #[case] system_field: Option<&str>,
) {
    let app = TestApp::with_key("k").await;
    let upload = match system_field {
        Some(system) => CallUpload::new().set("system", system),
        None => CallUpload::new().remove("system"),
    };

    app.upload_ok(upload).await;

    let sys = system::Entity::find().one(&app.db).await.unwrap().unwrap();
    assert_eq!(sys.r#ref, 1, "first System gets the lowest-free Ref 1");
}
