//! Archive read surface (#13, spec US 24–27): `GET /api/calls` search,
//! `GET /api/calls/filters` cascading options, and `GET /api/call/{id}/download`.
//!
//! Driven over the real HTTP boundary via the integration harness (ADR-0009).

mod common;
use common::logs::LogCapture;
use common::s3::unreachable_store;
use common::{TestApp, header_of, request_id_of};

use bytes::Bytes;
use radio_scout::db::repo::NewCall;
use serde_json::Value;

/// The dataset every search assertion below reads:
/// - system 100 "Alpha": tg1 tag Fire {Emergency}, tg2 tag Law {Emergency,Public}
/// - system 200 "Beta":  tg1 tag Fire {Public}
async fn seed(app: &TestApp) -> (i64, i64, i64, i64) {
    let a = seed_searchable_call(app, 100, "Alpha", 1, "Fire", &["Emergency"], 1000).await;
    let b = seed_searchable_call(app, 100, "Alpha", 2, "Law", &["Emergency", "Public"], 2000).await;
    let c = seed_searchable_call(app, 200, "Beta", 1, "Fire", &["Public"], 3000).await;
    let d = seed_searchable_call(app, 100, "Alpha", 1, "Fire", &["Emergency"], 4000).await;
    (a, b, c, d)
}

async fn seed_searchable_call(
    app: &TestApp,
    system_ref: i64,
    system_label: &str,
    talkgroup_ref: i64,
    tag: &str,
    groups: &[&str],
    at_ms: i64,
) -> i64 {
    app.seed_call(
        NewCall {
            system_label: Some(system_label.into()),
            talkgroup_tag: Some(tag.into()),
            talkgroup_groups: groups.iter().map(|g| (*g).to_string()).collect(),
            audio_mime: Some("audio/x-wav".into()),
            ..NewCall::new(system_ref, talkgroup_ref, at_ms)
        },
        common::audio_at(format!("k/{system_ref}-{talkgroup_ref}-{at_ms}.wav")),
    )
    .await
}

/// The result ids on a page, in response order.
fn ids_of(page: &Value) -> Vec<i64> {
    page["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|c| c["id"].as_i64().expect("id"))
        .collect()
}

/// The result ids of a search, in response order.
async fn search_ids(app: &TestApp, query: &str) -> Vec<i64> {
    ids_of(&app.get_json(&format!("/api/calls{query}")).await)
}

// ---------------------------------------------------------------------------
// GET /api/calls — search
// ---------------------------------------------------------------------------

/// A page arrives ready to render *and* play: the same denormalized Call the
/// live feed delivers, not rdio-scanner's bare `{id, system, talkgroup}` rows
/// that force the client to re-fetch every result one at a time.
#[tokio::test]
async fn search_returns_denormalized_calls_newest_first() {
    let app = TestApp::spawn().await;
    let (a, b, c, d) = seed(&app).await;

    let page = app.get_json("/api/calls").await;
    assert_eq!(ids_of(&page), vec![d, c, b, a]);
    assert_eq!(page["count"], 4);
    assert_eq!(page["offset"], 0);
    assert_eq!(page["hasMore"], false);

    let newest = &page["results"][0];
    assert_eq!(newest["systemRef"], 100);
    assert_eq!(newest["systemLabel"], "Alpha");
    assert_eq!(newest["talkgroupRef"], 1);
    assert_eq!(newest["talkgroupTag"], "Fire");
    assert_eq!(newest["talkgroupGroup"], "Emergency");
    assert_eq!(newest["timestamp"], 4000);
    assert_eq!(newest["audioUrl"], format!("/api/call/{d}/audio"));
    // Internal storage detail never leaves the server (ADR-0004).
    assert!(newest.get("objectKey").is_none());
}

#[tokio::test]
async fn search_filters_by_every_dimension() {
    let app = TestApp::spawn().await;
    let (a, b, c, d) = seed(&app).await;

    assert_eq!(search_ids(&app, "?system=100").await, vec![d, b, a]);
    assert_eq!(
        search_ids(&app, "?system=100&talkgroup=1").await,
        vec![d, a]
    );
    assert_eq!(search_ids(&app, "?group=Public").await, vec![c, b]);
    assert_eq!(search_ids(&app, "?tag=Law").await, vec![b]);
    assert_eq!(
        search_ids(&app, "?after=2000&before=3000").await,
        vec![c, b]
    );
    // Filters combine with AND.
    assert_eq!(search_ids(&app, "?tag=Fire&system=200").await, vec![c]);
    // A group name with a space survives URL encoding.
    assert!(
        search_ids(&app, "?group=No%20Such%20Group")
            .await
            .is_empty()
    );
}

/// Dates may be unix milliseconds or RFC3339 — the latter so a human or a
/// script can hand-write a query. rdio-scanner only accepts a single date and
/// silently searches the surrounding 24 h.
#[tokio::test]
async fn search_accepts_rfc3339_dates() {
    let app = TestApp::spawn().await;
    let epoch_plus_2s =
        seed_searchable_call(&app, 100, "Alpha", 1, "Fire", &["Emergency"], 2000).await;
    seed_searchable_call(&app, 100, "Alpha", 1, "Fire", &["Emergency"], 10_000).await;

    assert_eq!(
        search_ids(
            &app,
            "?after=1970-01-01T00:00:01Z&before=1970-01-01T00:00:05Z"
        )
        .await,
        vec![epoch_plus_2s]
    );
}

#[tokio::test]
async fn search_paginates_and_reports_whether_more_remains() {
    let app = TestApp::spawn().await;
    let (a, b, c, d) = seed(&app).await;

    let first = app.get_json("/api/calls?limit=2").await;
    assert_eq!(ids_of(&first), vec![d, c]);
    assert_eq!(first["count"], 4);
    assert_eq!(first["limit"], 2);
    assert_eq!(first["hasMore"], true);

    let last = app.get_json("/api/calls?limit=2&offset=2").await;
    assert_eq!(ids_of(&last), vec![b, a]);
    assert_eq!(last["offset"], 2);
    assert_eq!(last["hasMore"], false);

    // Past the end: an empty page, still reporting the true total.
    let past = app.get_json("/api/calls?limit=2&offset=99").await;
    assert!(ids_of(&past).is_empty());
    assert_eq!(past["count"], 4);
    assert_eq!(past["hasMore"], false);
}

/// Playback mode catches up on history, so it walks the filtered results
/// forwards in time.
#[tokio::test]
async fn search_sorts_oldest_first_for_playback_mode() {
    let app = TestApp::spawn().await;
    let (a, b, c, d) = seed(&app).await;

    assert_eq!(search_ids(&app, "?sort=oldest").await, vec![a, b, c, d]);
    assert_eq!(search_ids(&app, "?sort=asc").await, vec![a, b, c, d]);
    assert_eq!(search_ids(&app, "?sort=newest").await, vec![d, c, b, a]);
    assert_eq!(search_ids(&app, "?sort=desc").await, vec![d, c, b, a]);
}

/// A client builds this query string from form state, where "no filter" is an
/// empty string — those must read as absent, not as a filter that matches
/// nothing.
#[tokio::test]
async fn blank_filter_values_mean_no_filter() {
    let app = TestApp::spawn().await;
    let (a, b, c, d) = seed(&app).await;

    assert_eq!(
        search_ids(
            &app,
            "?system=&talkgroup=&group=&tag=&after=&before=&sort=&limit=&offset="
        )
        .await,
        vec![d, c, b, a]
    );
}

/// A limit past the ceiling is clamped rather than refused, and the response
/// reports the limit actually applied so the client's paging stays correct.
#[tokio::test]
async fn limit_is_clamped_to_the_ceiling() {
    let app = TestApp::spawn().await;
    seed(&app).await;

    let page = app.get_json("/api/calls?limit=10000").await;
    assert_eq!(page["limit"], 500);
    assert_eq!(page["count"], 4);
}

/// Bad input is refused with a message naming the parameter — rdio-scanner
/// silently ignores anything it can't parse, so a typo just returns the wrong
/// results.
#[tokio::test]
async fn malformed_parameters_are_rejected_with_a_reason() {
    let app = TestApp::spawn().await;

    for (query, expect) in [
        ("?system=abc", "system"),
        ("?talkgroup=1.5", "talkgroup"),
        ("?after=not-a-date", "after"),
        ("?before=2026-13-45", "before"),
        ("?sort=sideways", "sort"),
        ("?limit=-1", "limit"),
        ("?offset=x", "offset"),
    ] {
        let resp = app.get(&format!("/api/calls{query}")).await;
        assert_eq!(resp.status(), 400, "GET /api/calls{query}");
        let body = resp.text().await.unwrap();
        assert!(
            body.contains(expect),
            "GET /api/calls{query} -> {body:?} should name {expect:?}"
        );
    }
}

#[tokio::test]
async fn search_on_an_empty_archive_is_an_empty_page() {
    let app = TestApp::spawn().await;
    let page = app.get_json("/api/calls").await;

    assert!(page["results"].as_array().unwrap().is_empty());
    assert_eq!(page["count"], 0);
    assert_eq!(page["hasMore"], false);
}

// ---------------------------------------------------------------------------
// GET /api/calls/filters — cascading options
// ---------------------------------------------------------------------------

#[tokio::test]
async fn filters_endpoint_offers_only_reachable_values_and_cascades() {
    let app = TestApp::spawn().await;
    seed(&app).await;

    let all = app.get_json("/api/calls/filters").await;
    assert_eq!(
        all["systems"],
        serde_json::json!([
            {"ref": 100, "label": "Alpha"},
            {"ref": 200, "label": "Beta"},
        ])
    );
    assert_eq!(all["groups"], serde_json::json!(["Emergency", "Public"]));
    assert_eq!(all["tags"], serde_json::json!(["Fire", "Law"]));
    assert_eq!(all["dateStartMs"], 1000);
    assert_eq!(all["dateStopMs"], 4000);
    assert_eq!(all["talkgroups"].as_array().unwrap().len(), 3);
    assert_eq!(
        all["talkgroups"][0],
        serde_json::json!({"systemRef": 100, "ref": 1, "label": "1", "tag": "Fire"})
    );

    // Picking a System narrows the Talkgroups but leaves the System list whole.
    let scoped = app.get_json("/api/calls/filters?system=200").await;
    assert_eq!(
        scoped["talkgroups"],
        serde_json::json!([{"systemRef": 200, "ref": 1, "label": "1", "tag": "Fire"}])
    );
    assert_eq!(scoped["systems"].as_array().unwrap().len(), 2);
    assert_eq!(scoped["groups"], serde_json::json!(["Public"]));
}

#[tokio::test]
async fn filters_endpoint_rejects_malformed_parameters() {
    let app = TestApp::spawn().await;
    let resp = app.get("/api/calls/filters?system=abc").await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("system"));
}

/// A fresh install has no Calls; the Search screen must still render.
#[tokio::test]
async fn filters_endpoint_on_an_empty_archive_is_empty() {
    let app = TestApp::spawn().await;
    let options = app.get_json("/api/calls/filters").await;

    assert_eq!(options["systems"], serde_json::json!([]));
    assert_eq!(options["talkgroups"], serde_json::json!([]));
    assert_eq!(options["groups"], serde_json::json!([]));
    assert_eq!(options["tags"], serde_json::json!([]));
    assert!(options.get("dateStartMs").is_none());
    assert!(options.get("dateStopMs").is_none());
}

// ---------------------------------------------------------------------------
// GET /api/call/{id}/download — per-Call audio download (spec US 27)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn download_serves_the_audio_as_a_named_attachment() {
    let app = TestApp::spawn().await;
    let id = seed_searchable_call(&app, 100, "Alpha", 54241, "Fire", &["Emergency"], 1000).await;
    app.put_object("k/100-54241-1000.wav", b"RIFFDATA").await;

    let resp = app.get(&format!("/api/call/{id}/download")).await;
    assert_eq!(resp.status(), 200);
    let disposition = header_of(&resp, "content-disposition").expect("content-disposition");
    let content_type = header_of(&resp, "content-type").expect("content-type");

    assert!(
        disposition.starts_with("attachment; filename=\""),
        "{disposition}"
    );
    // Descriptive by construction: System, Talkgroup, and call time.
    assert!(disposition.contains("Alpha"), "{disposition}");
    assert!(disposition.contains("54241"), "{disposition}");
    assert!(disposition.contains("1000"), "{disposition}");
    assert!(disposition.ends_with(".wav\""), "{disposition}");
    assert_eq!(content_type, "audio/x-wav");
    assert_eq!(resp.bytes().await.unwrap(), Bytes::from_static(b"RIFFDATA"));
}

#[tokio::test]
async fn download_of_an_unknown_call_is_404() {
    let app = TestApp::spawn().await;
    let resp = app.get("/api/call/999999/download").await;
    assert_eq!(resp.status(), 404);
    assert_eq!(resp.text().await.unwrap(), "call not found\n");
}

#[tokio::test]
async fn download_of_a_call_whose_audio_is_gone_is_404() {
    let app = TestApp::spawn().await;
    let id = seed_searchable_call(&app, 100, "Alpha", 1, "Fire", &["Emergency"], 1000).await;
    let resp = app.get(&format!("/api/call/{id}/download")).await;
    assert_eq!(resp.status(), 404);
    assert_eq!(resp.text().await.unwrap(), "audio not found\n");
}

// ---------------------------------------------------------------------------
// Failure paths
// ---------------------------------------------------------------------------

/// A dead database must surface as a 500 — not as an empty archive, which would
/// look to a listener like retention ate their calls. What failed goes to the
/// server's log against the request's ref, never into the response (ADR-0011
/// rule 4).
#[tokio::test]
async fn a_broken_database_is_a_server_error_not_an_empty_archive() {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;
    seed(&app).await;
    app.db.clone().close().await.expect("close the pool");

    for (path, stage) in [
        ("/api/calls", "search-calls"),
        ("/api/calls/filters", "load-filter-options"),
        ("/api/call/1/download", "look-up-call"),
        ("/api/call/1", "load-call-detail"),
    ] {
        let resp = app.get(path).await;
        assert_eq!(resp.status(), 500, "GET {path}");
        let request_id = request_id_of(&resp);
        assert_eq!(
            resp.text().await.unwrap(),
            format!("internal error (request id: {request_id})\n"),
            "GET {path} tells the client the ref and nothing else"
        );

        let line = capture.only_line_containing(&format!("stage={stage}"));
        assert!(line.contains(" ERROR "), "GET {path}: {line}");
        assert!(line.contains(&format!("request_id={request_id}")), "{line}");
        assert!(line.contains("cause="), "GET {path} should say what failed");
    }
}

/// The MIME type comes from the recorder and is never validated on the way in,
/// so a value that can't be a header must not take the download down with it.
#[tokio::test]
async fn download_falls_back_when_the_stored_mime_is_not_header_safe() {
    let app = TestApp::spawn().await;
    let id = app
        .seed_call(
            NewCall {
                audio_mime: Some("audio/\u{7f}broken".into()),
                ..NewCall::new(100, 1, 1000)
            },
            common::audio_at("k/junk-mime.wav"),
        )
        .await;
    app.put_object("k/junk-mime.wav", b"RIFF").await;

    let resp = app.get(&format!("/api/call/{id}/download")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        header_of(&resp, "content-type"),
        Some("application/octet-stream")
    );
}

/// An object store that can't be reached is a 500, distinct from the 404 an
/// object that simply isn't there gets — an operator needs to tell "gone" from
/// "broken", and the log is where that distinction lives now (rule 4).
/// Download always proxies (never a presigned redirect), so the store being down
/// is the download being down.
///
/// Takes about a second rather than milliseconds: a refused connection is still
/// retried with backoff before the store gives up. How *much* backoff is our own
/// decision since #39 — `blob::retry_policy` bounds it, which is why this is a
/// second and not the minute-plus tail it used to be able to draw.
#[tokio::test]
async fn download_reports_an_unreachable_object_store() {
    let capture = LogCapture::start();
    let app = TestApp::builder().store(unreachable_store()).spawn().await;
    let id = seed_searchable_call(&app, 100, "Alpha", 1, "Fire", &["Emergency"], 1000).await;

    let resp = app.get(&format!("/api/call/{id}/download")).await;
    assert_eq!(resp.status(), 500);
    let body = resp.text().await.unwrap();
    assert!(body.starts_with("internal error (request id: "), "{body:?}");

    let line = capture.only_line_containing("stage=read-audio");
    assert!(line.contains(" ERROR "), "{line}");
    assert!(line.contains("cause="), "the store's own words: {line}");
}

// ---------------------------------------------------------------------------
// GET /api/call/{id} — one Call, all of it (#42, spec US 5)
// ---------------------------------------------------------------------------

/// The per-frequency and per-source detail every recorder sends and rdio-scanner
/// throws away has to be *reachable*, or parsing it was pointless.
///
/// It lives here rather than on `StoredCall` on purpose: `StoredCall` is one
/// live-feed frame per Call and fifty rows per search page, and on a Pi serving
/// a county neither should carry arrays nobody is looking at. Asking for one
/// Call is the moment somebody is.
#[tokio::test]
async fn a_call_detail_carries_everything_the_recorder_said() {
    let app = TestApp::with_key("k").await;
    let meta = r#"{
      "short_name":"butco","talkgroup":54241,
      "start_time":1669740338,"stop_time":1669740346,"call_length_ms":8250,
      "emergency":1,"encrypted":0,"priority":3,"audio_type":"digital",
      "freqList":[{"freq":774031250,"time":1669740338,"pos":0.25,"len":1.5,
                   "error_count":2,"spike_count":1}],
      "srcList":[{"src":4424000,"time":1669740339,"pos":0.75,"emergency":1,
                  "signal_system":"P25","tag":"Engine 1","tag_ota":"E1 OTA"}]
    }"#;
    app.upload_tr(common::CallUpload::tr(meta)).await;
    let id = app.the_call().await.id;

    let call = app.get_json(&format!("/api/call/{id}")).await;

    // Everything a search row already knows...
    assert_eq!(call["talkgroupRef"], 54241);
    assert_eq!(call["durationMs"], 8250);
    assert_eq!(call["emergency"], true);
    // ...plus what only this endpoint carries.
    assert_eq!(call["priority"], 3);
    assert_eq!(call["audioType"], "digital");
    assert_eq!(call["stopMs"], 1669740346000i64);
    assert_eq!(
        call["frequencies"],
        serde_json::json!([{
            "freq": 774031250, "posMs": 250, "lenMs": 1500,
            "errorCount": 2, "spikeCount": 1, "atMs": 1669740338000i64
        }])
    );
    assert_eq!(
        call["units"],
        serde_json::json!([{
            "ref": 4424000, "label": "Engine 1", "tagOta": "E1 OTA",
            "offsetMs": 750, "emergency": true, "signalSystem": "P25",
            "atMs": 1669740339000i64
        }])
    );
}

/// A Call the recorder said little about carries little — the detail keys are
/// absent rather than null, the same rule the rest of the wire follows.
#[tokio::test]
async fn a_call_detail_omits_what_was_never_sent() {
    let app = TestApp::with_key("k").await;
    app.upload_ok(common::CallUpload::new()).await;
    let id = app.the_call().await.id;

    let call = app.get_json(&format!("/api/call/{id}")).await;

    assert_eq!(call["id"], id);
    for absent in ["priority", "audioType", "stopMs", "emergency", "encrypted"] {
        assert!(
            call.get(absent).is_none(),
            "{absent} should be absent: {call}"
        );
    }
    assert_eq!(call["frequencies"], serde_json::json!([]));
    assert_eq!(call["units"], serde_json::json!([]));
}

/// A Call that isn't there is a 404, not a 500 and not an empty object.
#[tokio::test]
async fn a_call_detail_for_an_unknown_id_is_not_found() {
    let app = TestApp::spawn().await;

    let resp = app.get("/api/call/999999").await;

    assert_eq!(resp.status().as_u16(), 404);
}

// ---------------------------------------------------------------------------
// The minimum-duration filter (#42, spec US 8)
// ---------------------------------------------------------------------------

/// Seed three Calls one second apart: a kerchunk, a dispatch, and one from
/// before durations were recorded at all.
async fn seed_by_duration(app: &TestApp) {
    for (at_ms, duration_ms) in [(1000, Some(900)), (2000, Some(12_000)), (3000, None)] {
        app.seed_call(
            NewCall {
                duration_ms,
                ..NewCall::new(100, 1, at_ms)
            },
            common::audio_at(format!("k/{at_ms}.wav")),
        )
        .await;
    }
}

/// "Hide the kerchunks" — the filter a listener actually wants, in the unit a
/// listener thinks in.
#[tokio::test]
async fn min_duration_hides_the_short_calls() {
    let app = TestApp::spawn().await;
    seed_by_duration(&app).await;

    let page = app.get_json("/api/calls?minDuration=5").await;

    assert_eq!(page["count"], 1, "only the twelve-second call: {page}");
    assert_eq!(page["results"][0]["durationMs"], 12_000);
}

/// A Call whose length was never measured does not match a length filter.
///
/// It is the only honest answer — a threshold cannot be tested against an
/// unknown — and it costs nothing to the archive an operator can see, because
/// leaving the filter unset still shows every Call there is. The alternative,
/// admitting unknowns, would make the filter quietly not filter the half of an
/// upgraded archive that predates #42.
#[tokio::test]
async fn a_call_with_no_known_duration_does_not_match_a_duration_filter() {
    let app = TestApp::spawn().await;
    seed_by_duration(&app).await;

    let filtered = app.get_json("/api/calls?minDuration=0").await;
    assert_eq!(
        filtered["count"], 2,
        "zero is still a filter, and still excludes the unknown: {filtered}"
    );

    let unfiltered = app.get_json("/api/calls").await;
    assert_eq!(unfiltered["count"], 3, "unset shows everything");
}

/// The filter narrows the cascading filter options too, so the dropdowns keep
/// their promise: every value offered has Calls behind it *given the other
/// filters already chosen*.
#[tokio::test]
async fn min_duration_narrows_the_filter_options() {
    let app = TestApp::spawn().await;
    seed_by_duration(&app).await;
    app.seed_call(
        NewCall {
            duration_ms: Some(500),
            ..NewCall::new(200, 9, 4000)
        },
        common::audio_at("k/short.wav"),
    )
    .await;

    let options = app.get_json("/api/calls/filters?minDuration=5").await;

    let systems: Vec<i64> = options["systems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["ref"].as_i64().unwrap())
        .collect();
    assert_eq!(systems, vec![100], "System 200 has only kerchunks");
}

/// A malformed value names itself, the way every other filter does — rdio
/// silently ignores what it can't parse, so a typo returns plausible wrong
/// results.
#[tokio::test]
async fn a_malformed_min_duration_is_a_named_bad_request() {
    let app = TestApp::spawn().await;

    let resp = app.get("/api/calls?minDuration=ages").await;

    assert_eq!(resp.status().as_u16(), 400);
    assert!(resp.text().await.unwrap().contains("minDuration"));
}
