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
// What a read costs (#98)
// ---------------------------------------------------------------------------

/// **A search page costs a fixed handful of database round-trips however many
/// Calls it carries** — the property an N+1 hides, because the page it returns
/// is correct either way and only the cost behind it is wrong (#86, #98).
///
/// This is the whole of rdio-scanner's worst archive behaviour: it answers a
/// search with bare ids and has the client fetch every row it wants to show,
/// one request at a time, over its own WebSocket. A page of fifty Calls is
/// fifty-one round trips there and a constant here — which on a Pi is the
/// difference between a Search screen and a stalled one.
///
/// Two sizes and not a pinned number, deliberately: what must hold is that the
/// cost does not grow per Call, and a pinned constant would make a legitimately
/// added query look like this regression. The Backfill's own version of this
/// lives in `tests/live.rs`; since #98 both read the same module.
#[tokio::test]
async fn a_search_page_costs_the_same_queries_however_many_calls_it_carries() {
    let few = search_statements(2).await;
    let many = search_statements(20).await;

    // A search costs *something*, or the two below are equal because nothing is
    // being counted rather than because nothing grows.
    assert!(few > 0, "a search reads the archive");
    assert_eq!(
        few, many,
        "a page of 20 Calls must cost what a page of 2 does"
    );
}

/// Serve one search page over `calls` stored Calls, and answer with the number
/// of statements the Instance issued doing it.
async fn search_statements(calls: i64) -> u64 {
    let app = TestApp::spawn().await;
    // Distinct Systems, Talkgroups, Tags, Groups **and radios**, so every
    // batched lookup in the denormalizer has more than one row to resolve — a
    // per-Call query would otherwise be hidden behind a page that all resolves
    // to one of everything. The radios matter twice over since #47: each is
    // owned by a **Unit** of its own, and by a **Range** on top of that, so the
    // page cannot be resolved by one lucky lookup that happens to answer for
    // every Call.
    for n in 0..calls {
        app.seed_unit(100 + n, 1200, "Engine 1").await;
        app.seed_unit_range(100 + n, 1200, 1201, 1299).await;
        app.seed_call(
            NewCall {
                system_label: Some("Alpha".into()),
                talkgroup_tag: Some("Fire".into()),
                talkgroup_groups: vec!["Emergency".into()],
                audio_mime: Some("audio/x-wav".into()),
                units: vec![radio_scout::db::repo::NewCallUnit {
                    unit_ref: 1250,
                    tag_ota: Some("E1 PORTABLE".into()),
                    ..Default::default()
                }],
                ..NewCall::new(100 + n, n, 1000 + n)
            },
            common::audio_at(format!("k/{n}.wav")),
        )
        .await;
    }
    // Every Worker owes nothing (#93), so what is counted below is the read's
    // own and not a seed still being finished behind it.
    app.settle().await;

    let before = app.statements_issued();
    let page = app.get_json("/api/calls").await;
    assert_eq!(
        ids_of(&page).len() as i64,
        calls,
        "the whole page came back"
    );
    app.statements_issued() - before
}

/// One Call read whole is the same shape of promise: opening a Call with a long
/// signal history must not cost a query per frequency or per unit heard.
#[tokio::test]
async fn one_call_costs_the_same_queries_however_much_detail_it_carries() {
    let brief = detail_statements(1).await;
    let long = detail_statements(12).await;

    assert!(brief > 0, "reading one Call reads the archive");
    assert_eq!(
        brief, long,
        "a Call's detail must not cost a query per frequency or per unit heard"
    );
}

/// Read one Call's detail over an upload carrying `entries` frequencies and
/// `entries` units, and answer with the number of statements it took.
async fn detail_statements(entries: usize) -> u64 {
    let app = TestApp::with_key("k").await;
    let listed =
        |body: &dyn Fn(usize) -> String| (0..entries).map(body).collect::<Vec<_>>().join(",");
    let meta = format!(
        r#"{{"short_name":"butco","talkgroup":54241,"start_time":1669740338,
            "freqList":[{}],"srcList":[{}]}}"#,
        listed(&|n| format!(
            r#"{{"freq":{},"time":1669740338,"pos":{n},"len":1}}"#,
            774_031_250 + n
        )),
        listed(&|n| format!(
            r#"{{"src":{},"time":1669740339,"pos":{n},"tag":"Engine {n}"}}"#,
            4_424_000 + n
        )),
    );
    app.upload_tr(common::CallUpload::tr(&meta)).await;
    let id = app.the_call().await.id;
    // Every Worker owes nothing (#93), so what is counted below is the read's
    // own and not an ingest still being finished behind it.
    app.settle().await;

    let before = app.statements_issued();
    let call = app.get_json(&format!("/api/call/{id}")).await;
    assert_eq!(
        call["frequencies"].as_array().expect("frequencies").len(),
        entries,
        "the whole signal history came back"
    );
    assert_eq!(call["units"].as_array().expect("units").len(), entries);
    app.statements_issued() - before
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

// ---------------------------------------------------------------------------
// Searching by radio (#47, spec US 44)
// ---------------------------------------------------------------------------

/// Seed three Calls on System 11: two from radio 1250 and one from radio 9999.
async fn seed_by_radio(app: &TestApp) -> (i64, i64, i64) {
    let mut at = 1000;
    let mut heard = |unit_ref: i64| {
        at += 1000;
        app.seed_call(
            NewCall {
                units: vec![radio_scout::db::repo::NewCallUnit {
                    unit_ref,
                    ..Default::default()
                }],
                ..NewCall::new(11, 54241, at)
            },
            common::audio_at(format!("k/{unit_ref}-{at}.wav")),
        )
    };
    (heard(1250).await, heard(9999).await, heard(1250).await)
}

/// "Who said that, and where else" is one query (spec US 44). rdio-scanner
/// cannot answer it at all: it stores the unit rows and offers no way to search
/// them.
#[tokio::test]
async fn search_filters_by_the_radio_that_was_heard() {
    let app = TestApp::spawn().await;
    let (first, other, second) = seed_by_radio(&app).await;

    assert_eq!(search_ids(&app, "?unit=1250").await, vec![second, first]);
    assert_eq!(search_ids(&app, "?unit=9999").await, vec![other]);
    assert!(search_ids(&app, "?unit=4242").await.is_empty());
    // ...and it is a filter like the others: it narrows, and it combines.
    assert_eq!(
        search_ids(&app, "?unit=1250&after=3000").await,
        vec![second]
    );
}

/// The filter reaches the **apparatus**, not one radio id (#45, spec US 16): a
/// Range's owner finds the Calls its portables keyed, and a portable finds the
/// Calls the mobile keyed. Anything less would make merging Units a curation
/// that quietly loses history.
#[tokio::test]
async fn a_unit_filter_reaches_every_ref_the_apparatus_answers_to() {
    let app = TestApp::spawn().await;
    app.seed_unit(11, 1200, "Engine 1").await;
    app.seed_unit_range(11, 1200, 1201, 1299).await;
    app.seed_unit_range(11, 1200, 4471, 4471).await;
    let (portable, _, _) = seed_by_radio(&app).await;
    let spare = app
        .seed_call(
            NewCall {
                units: vec![radio_scout::db::repo::NewCallUnit {
                    unit_ref: 4471,
                    ..Default::default()
                }],
                ..NewCall::new(11, 54241, 9000)
            },
            common::audio_at("k/spare.wav"),
        )
        .await;

    // Asked for by its own Ref, by the Range it owns, and by the lone member
    // Ref beside it — one apparatus, one answer, three spellings.
    for asked in ["1200", "1250", "4471"] {
        let found = search_ids(&app, &format!("?unit={asked}")).await;
        assert!(
            found.contains(&portable) && found.contains(&spare),
            "unit={asked} found {found:?}"
        );
    }
}

/// A radio nobody owns is searched for as itself — which is every uncurated
/// archive, and must not silently return nothing.
#[tokio::test]
async fn a_unit_filter_for_an_unowned_ref_finds_exactly_that_ref() {
    let app = TestApp::spawn().await;
    app.seed_unit(11, 1200, "Engine 1").await;
    app.seed_unit_range(11, 1200, 1201, 1299).await;
    let (_, other, _) = seed_by_radio(&app).await;

    assert_eq!(search_ids(&app, "?unit=9999").await, vec![other]);
}

/// **Ranges are System-scoped, and so is the filter.** Two Systems number their
/// radios independently, so one System's fleet block must never widen a search
/// pinned to another's — that would answer with Calls from radios the apparatus
/// has nothing to do with.
#[tokio::test]
async fn one_systems_range_never_widens_a_search_pinned_to_another() {
    let app = TestApp::spawn().await;
    app.seed_unit(11, 1200, "Engine 1").await;
    app.seed_unit_range(11, 1200, 1201, 1299).await;
    let unrelated = app
        .seed_call(
            NewCall {
                units: vec![radio_scout::db::repo::NewCallUnit {
                    unit_ref: 1210,
                    ..Default::default()
                }],
                ..NewCall::new(200, 1, 1000)
            },
            common::audio_at("k/unrelated.wav"),
        )
        .await;

    assert!(
        search_ids(&app, "?system=200&unit=1250").await.is_empty(),
        "System 200 knows no Range covering 1250"
    );
    assert_eq!(
        search_ids(&app, "?system=200&unit=1210").await,
        vec![unrelated],
        "...but the radio itself is still findable there"
    );
}

/// ...and **with no System pinned at all**, which is what the Search screen's
/// unit box sends: a Ref is unique only within its System, so System 11's fleet
/// block must not reach System 200's radio 1210 just because nobody said whose
/// fleet was meant.
///
/// The Ref *asked about* still matches everywhere, because that number is what
/// somebody typed and a Call carrying it is one they meant. What must not
/// happen is the rest of an apparatus leaking across.
#[tokio::test]
async fn an_unpinned_unit_search_never_leaks_one_systems_fleet_into_another() {
    let app = TestApp::spawn().await;
    app.seed_unit(11, 1200, "Engine 1").await;
    app.seed_unit_range(11, 1200, 1201, 1299).await;
    let (portable, _, _) = seed_by_radio(&app).await;
    let heard_elsewhere = |system_ref: i64, unit_ref: i64, at: i64| {
        app.seed_call(
            NewCall {
                units: vec![radio_scout::db::repo::NewCallUnit {
                    unit_ref,
                    ..Default::default()
                }],
                ..NewCall::new(system_ref, 1, at)
            },
            common::audio_at(format!("k/{system_ref}-{unit_ref}.wav")),
        )
    };
    let inside_the_block = heard_elsewhere(200, 1210, 5000).await;
    let the_same_radio = heard_elsewhere(200, 1250, 6000).await;

    let found = search_ids(&app, "?unit=1250").await;

    assert!(
        found.contains(&portable) && found.contains(&the_same_radio),
        "the radio asked about is found on every System: {found:?}"
    );
    assert!(
        !found.contains(&inside_the_block),
        "System 200's radio 1210 is not in System 11's fleet: {found:?}"
    );
}

/// A Call whose Recorder listed the same radio twice — which Trunk Recorder does
/// on every transmission a radio keys more than once — is **one** Call in the
/// results and one in the count.
///
/// This is the first filter that reaches a genuinely to-many table, which is
/// exactly the case `CallQuery::rows`'s comment says would owe a `DISTINCT`. It
/// is written as a subquery instead: nothing is joined, so nothing can multiply.
#[tokio::test]
async fn a_unit_filter_cannot_multiply_a_call() {
    let app = TestApp::with_key("k").await;
    app.upload_tr(common::CallUpload::tr(
        r#"{"short_name":"butco","talkgroup":54241,"start_time":1000,
            "srcList":[{"src":4424000,"pos":0},{"src":4424000,"pos":4}]}"#,
    ))
    .await;

    let page = app.get_json("/api/calls?unit=4424000").await;

    assert_eq!(ids_of(&page).len(), 1, "one Call: {page}");
    assert_eq!(page["count"], 1, "and the total agrees: {page}");
}

/// The filter options are the *other* dimensions', narrowed by this one — so
/// picking a radio narrows the Talkgroup list to where that radio was heard.
#[tokio::test]
async fn a_unit_filter_narrows_the_filter_options() {
    let app = TestApp::spawn().await;
    seed_by_radio(&app).await;
    app.seed_call(
        NewCall {
            units: vec![radio_scout::db::repo::NewCallUnit {
                unit_ref: 9999,
                ..Default::default()
            }],
            ..NewCall::new(11, 900, 9000)
        },
        common::audio_at("k/other-tg.wav"),
    )
    .await;

    let options = app.get_json("/api/calls/filters?unit=1250").await;

    let talkgroups: Vec<i64> = options["talkgroups"]
        .as_array()
        .expect("talkgroups")
        .iter()
        .map(|t| t["ref"].as_i64().expect("ref"))
        .collect();
    assert_eq!(talkgroups, vec![54241], "only where 1250 was heard");
}

// ---------------------------------------------------------------------------
// One radio's history (#47, spec US 44)
// ---------------------------------------------------------------------------

/// "Who said that, and where else" — the Talkgroups a radio has been heard on,
/// when it was first and last heard, and how much of the Archive is its.
///
/// rdio-scanner has no equivalent: it stores unit rows and never shows one.
#[tokio::test]
async fn a_unit_history_says_where_and_when_the_radio_was_heard() {
    let app = TestApp::spawn().await;
    app.seed_unit(11, 1200, "Engine 1").await;
    app.seed_unit_range(11, 1200, 1201, 1299).await;
    // Two Calls on Fire Dispatch and one on Tac 2, the last of them keyed by a
    // portable inside the apparatus's Range.
    for (talkgroup, unit_ref, at) in [(54241, 1200, 1000), (54241, 1200, 2000), (900, 1250, 3000)] {
        app.seed_call(
            NewCall {
                talkgroup_label: Some(format!("TG {talkgroup}")),
                units: vec![radio_scout::db::repo::NewCallUnit {
                    unit_ref,
                    ..Default::default()
                }],
                ..NewCall::new(11, talkgroup, at)
            },
            common::audio_at(format!("k/{talkgroup}-{at}.wav")),
        )
        .await;
    }

    let unit = app.get_json("/api/unit/11/1200").await;

    assert_eq!(unit["ref"], 1200);
    assert_eq!(unit["label"], "Engine 1");
    assert_eq!(unit["systemRef"], 11);
    assert_eq!(
        unit["callCount"], 3,
        "the portable's Call is the apparatus's"
    );
    assert_eq!(unit["firstHeardMs"], 1000);
    assert_eq!(unit["lastHeardMs"], 3000);
    assert_eq!(
        unit["memberRefs"],
        serde_json::json!([{ "from": 1201, "to": 1299 }])
    );
    // Busiest first — which channel this radio lives on is the question, and an
    // alphabetical or Ref order buries the answer under whatever it touched once.
    assert_eq!(
        unit["talkgroups"],
        serde_json::json!([
            { "ref": 54241, "label": "TG 54241", "calls": 2, "lastHeardMs": 2000 },
            { "ref": 900, "label": "TG 900", "calls": 1, "lastHeardMs": 3000 },
        ])
    );
}

/// A radio nobody has curated still has a history — which is every radio in an
/// uncurated archive, and the whole point of reaching this view by tapping a
/// bare number.
#[tokio::test]
async fn an_unnamed_radio_still_has_a_history() {
    let app = TestApp::with_key("k").await;
    app.upload_ok(common::CallUpload::new().set("unit", "4242"))
        .await;

    let unit = app.get_json("/api/unit/11/4242").await;

    assert_eq!(unit["ref"], 4242);
    assert!(unit.get("label").is_none(), "{unit}");
    assert!(unit.get("memberRefs").is_none(), "owns nothing: {unit}");
    assert_eq!(unit["callCount"], 1);
}

/// A radio that has neither been heard nor curated is a 404 — not an empty
/// history, which would make any typo look like a radio that had gone quiet.
#[tokio::test]
async fn a_radio_that_was_never_heard_is_not_found() {
    let app = TestApp::with_key("k").await;
    app.upload_ok(common::CallUpload::new().set("unit", "4242"))
        .await;

    for missing in ["/api/unit/11/9999", "/api/unit/99/4242"] {
        assert_eq!(app.get(missing).await.status().as_u16(), 404, "{missing}");
    }
}

/// ...but a curated **Unit** that has never keyed is a history with nothing in
/// it, because an Operator who wrote that apparatus down should be able to open
/// it and see that it has been quiet.
#[tokio::test]
async fn a_curated_unit_that_has_never_keyed_is_an_empty_history() {
    let app = TestApp::spawn().await;
    app.seed_unit(11, 1200, "Engine 1").await;

    let unit = app.get_json("/api/unit/11/1200").await;

    assert_eq!(unit["label"], "Engine 1");
    assert_eq!(unit["callCount"], 0);
    assert_eq!(unit["talkgroups"], serde_json::json!([]));
    assert!(unit.get("firstHeardMs").is_none(), "{unit}");
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

// ---------------------------------------------------------------------------
// How busy the Archive was (#62, spec US 34–35)
// ---------------------------------------------------------------------------

/// The ribbon over a search is a picture of *that* search, so the bars have to
/// add up to the number of results above them — the one claim a density chart
/// makes and the only one that can be wrong without looking wrong.
#[tokio::test]
async fn the_bars_of_a_density_series_add_up_to_the_searchs_own_total() {
    let app = TestApp::spawn().await;
    seed(&app).await;

    for query in ["", "?tag=Fire", "?system=100", "?group=Emergency"] {
        let page = app.get_json(&format!("/api/calls{query}")).await;
        let series = app.get_json(&format!("/api/calls/activity{query}")).await;

        let total: i64 = series["values"]
            .as_array()
            .expect("values")
            .iter()
            .map(|v| v.as_i64().expect("a count"))
            .sum();
        assert_eq!(
            total,
            page["count"].as_i64().expect("count"),
            "GET /api/calls/activity{query}"
        );
    }
}

/// A named width is honoured exactly, because the heatmap folds these buckets
/// into local hours and that is only right when a bucket really is an hour.
#[tokio::test]
async fn a_named_bucket_width_is_what_comes_back() {
    let app = TestApp::spawn().await;
    seed(&app).await;

    let series = app
        .get_json("/api/calls/activity?after=1000&before=4999&bucketMs=1000")
        .await;

    assert_eq!(series["fromMs"], 1000);
    assert_eq!(series["toMs"], 5000);
    assert_eq!(series["bucketMs"], 1000);
    assert_eq!(series["values"], serde_json::json!([1, 1, 1, 1]));
}

/// **A dated search does not pay to be told where it is.** The extra statement
/// exists to discover an axis, so a search that named both bounds — a preset, a
/// heatmap, a tapped bucket — must not issue it.
#[tokio::test]
async fn a_dated_series_costs_one_statement_fewer_than_an_undated_one() {
    let app = TestApp::spawn().await;
    seed(&app).await;
    app.settle().await;

    let before = app.statements_issued();
    app.get_json("/api/calls/activity?after=1000&before=4999")
        .await;
    let dated = app.statements_issued() - before;

    let before = app.statements_issued();
    app.get_json("/api/calls/activity").await;
    let undated = app.statements_issued() - before;

    assert!(dated > 0, "a series reads the archive");
    assert_eq!(
        undated,
        dated + 1,
        "an undated series pays exactly one statement to find its own extent"
    );
}

/// The cost does not grow with the archive: a grouped count is one statement
/// whether it folds four Calls or four hundred.
#[tokio::test]
async fn a_density_series_costs_the_same_however_many_calls_it_folds() {
    assert_eq!(
        density_statements(3).await,
        density_statements(40).await,
        "a series is grouped in the database, never walked in Rust"
    );
}

async fn density_statements(calls: i64) -> u64 {
    let app = TestApp::spawn().await;
    for n in 0..calls {
        seed_searchable_call(&app, 100, "Alpha", 1, "Fire", &["Emergency"], 1000 + n).await;
    }
    app.settle().await;

    let before = app.statements_issued();
    let series = app.get_json("/api/calls/activity").await;
    assert!(!series["values"].as_array().expect("values").is_empty());
    app.statements_issued() - before
}

/// An empty archive answers with an axis carrying nothing, rather than with an
/// empty array every consumer would need an arm for.
#[tokio::test]
async fn a_density_series_over_an_empty_archive_is_a_flat_one() {
    let app = TestApp::spawn().await;

    let series = app.get_json("/api/calls/activity").await;

    let values = series["values"].as_array().expect("values");
    assert!(!values.is_empty(), "an axis, even with nothing on it");
    assert!(values.iter().all(|v| v == 0));
    assert!(series["toMs"].as_i64().expect("toMs") > series["fromMs"].as_i64().expect("fromMs"));
}

/// A bar count is what a ribbon really asks for — it knows how wide it is and
/// not how long the search's range is — and it is honoured.
#[tokio::test]
async fn a_named_bar_count_is_what_comes_back() {
    let app = TestApp::spawn().await;
    seed(&app).await;

    let series = app.get_json("/api/calls/activity?buckets=8").await;

    let values = series["values"].as_array().expect("values");
    assert!(
        !values.is_empty() && values.len() <= 8,
        "{} bars",
        values.len()
    );
}

/// ...and a count nobody could draw is bounded rather than refused, the same
/// bargain a page size gets.
#[tokio::test]
async fn an_unreasonable_bar_count_is_bounded() {
    let app = TestApp::spawn().await;
    seed(&app).await;

    let series = app
        .get_json("/api/calls/activity?after=0&before=100000000&buckets=99999")
        .await;

    let buckets = series["values"].as_array().expect("values").len();
    assert!(buckets <= radio_scout::activity::MAX_BUCKETS, "{buckets}");
}

/// A grain finer than the response may carry is widened, and the response says
/// which width it used — so a client draws bars it can label rather than being
/// refused or, worse, mislabeling them.
#[tokio::test]
async fn an_impossibly_fine_grain_is_widened_and_reported() {
    let app = TestApp::spawn().await;
    seed(&app).await;

    let series = app
        .get_json("/api/calls/activity?after=0&before=100000000&bucketMs=1")
        .await;

    let buckets = series["values"].as_array().expect("values").len();
    assert!(buckets <= radio_scout::activity::MAX_BUCKETS, "{buckets}");
    assert!(
        series["bucketMs"].as_i64().expect("bucketMs") > 1,
        "widened, and the response says so"
    );
}

/// The same parser as the search page, so the same typo is refused the same way
/// — and the grain gets the same treatment.
#[tokio::test]
async fn malformed_activity_parameters_are_rejected_with_a_reason() {
    let app = TestApp::spawn().await;

    for (query, expect) in [
        ("?system=abc", "system"),
        ("?after=not-a-date", "after"),
        ("?bucketMs=wide", "bucketMs"),
        ("?buckets=-4", "buckets"),
    ] {
        let resp = app.get(&format!("/api/calls/activity{query}")).await;
        assert_eq!(resp.status(), 400, "GET /api/calls/activity{query}");
        let body = resp.text().await.unwrap();
        assert!(
            body.contains(expect),
            "GET /api/calls/activity{query} -> {body:?} should name {expect:?}"
        );
    }
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
            // The **Unit** this radio belongs to (#47) — rostered from the
            // recorder's own tag a moment ago, since nothing had named it yet.
            "unitLabel": "Engine 1",
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

// ---------------------------------------------------------------------------
// The radio a Call is shown under (#47, spec US 42)
// ---------------------------------------------------------------------------

/// Every row, every frame and every display shows *one* radio — the first one
/// heard, in the order its Recorder listed them, which is the order a Call's
/// timeline is read out in.
///
/// One rather than the whole `srcList`, for the reason the detail endpoint above
/// exists: a Call reaches a phone fifty to a page and once per live frame, and
/// an array of radios on each of those is a payload nobody is reading.
#[tokio::test]
async fn a_search_row_names_the_first_radio_heard() {
    let app = TestApp::with_key("k").await;
    app.upload_tr(common::CallUpload::tr(
        r#"{"short_name":"butco","talkgroup":54241,"start_time":1000,
            "srcList":[{"src":4424000,"pos":0,"tag":"Engine 1"},
                       {"src":4424009,"pos":2,"tag":"Ladder 3"}]}"#,
    ))
    .await;

    let row = &app.get_json("/api/calls").await["results"][0];

    assert_eq!(row["unitRef"], 4424000);
    assert_eq!(row["unitLabel"], "Engine 1");
}

/// A radio nobody has named is still a radio: the Ref rides, and the label
/// simply is not there. This is every uncurated archive, and the reason the
/// label is a separate key rather than a rendered string.
#[tokio::test]
async fn an_unnamed_radio_rides_as_a_ref_with_no_label() {
    let app = TestApp::with_key("k").await;
    app.upload_ok(common::CallUpload::new().set("unit", "4424000"))
        .await;

    let row = &app.get_json("/api/calls").await["results"][0];

    assert_eq!(row["unitRef"], 4424000);
    assert!(row.get("unitLabel").is_none(), "{row}");
}

/// ...and a Call nobody was heard on carries neither key, rather than a null
/// pair every client would have to check.
#[tokio::test]
async fn a_call_with_no_radio_at_all_carries_neither_key() {
    let app = TestApp::with_key("k").await;
    app.upload_ok(common::CallUpload::new()).await;

    let row = &app.get_json("/api/calls").await["results"][0];

    assert!(row.get("unitRef").is_none(), "{row}");
    assert!(row.get("unitLabel").is_none(), "{row}");
}

/// A portable inside a fleet's **Range** is shown as the apparatus that owns it
/// (#45, spec US 16) — the canonical Ref, so the label a Listener taps and the
/// history it opens are the whole apparatus rather than whichever radio keyed.
///
/// The arriving Ref is not lost: `GET /api/call/{id}`'s `units[]` still records
/// exactly which radio was heard, which is where somebody asking that question
/// is looking.
#[tokio::test]
async fn a_radio_inside_a_range_is_shown_as_the_apparatus_that_owns_it() {
    let app = TestApp::with_key("k").await;
    app.seed_unit(11, 1200, "Engine 1").await;
    app.seed_unit_range(11, 1200, 1201, 1299).await;

    app.upload_ok(common::CallUpload::new().set("unit", "1250"))
        .await;

    let row = &app.get_json("/api/calls").await["results"][0];
    assert_eq!(row["unitRef"], 1200, "the apparatus, not the portable");
    assert_eq!(row["unitLabel"], "Engine 1");

    let id = app.the_call().await.id;
    let detail = app.get_json(&format!("/api/call/{id}")).await;
    assert_eq!(detail["units"][0]["ref"], 1250, "which radio keyed");
}

/// A **Unit**'s own alias outranks whatever a Call happened to arrive with —
/// that is what curating one is *for*. Auto-populate never rewrites it (#8), so
/// without this a name an Operator corrected would go on being wrong in every
/// row until the Archive aged out.
#[tokio::test]
async fn a_curated_unit_alias_outranks_the_name_a_call_arrived_with() {
    let app = TestApp::with_key("k").await;
    app.seed_unit(11, 4424000, "MEDIC 7").await;

    app.upload_ok(
        common::CallUpload::new().set("sources", r#"[{"src":4424000,"pos":0,"tag":"Engine 1"}]"#),
    )
    .await;

    let row = &app.get_json("/api/calls").await["results"][0];
    assert_eq!(row["unitLabel"], "MEDIC 7");
    let id = app.the_call().await.id;
    assert_eq!(
        app.get_json(&format!("/api/call/{id}")).await["units"][0]["label"],
        "Engine 1",
        "what the Recorder said is still on the Call"
    );
}

/// **Every radio on a Call's timeline carries the curated name**, not only the
/// first (#47, spec US 42). `StoredCall` shows one radio because a list and a
/// live frame want one; this endpoint exists to show every radio heard, and a
/// name an Operator wrote in the unit CSV has to reach all of them or the
/// curation is invisible exactly where the detail is.
///
/// Three names, kept apart on purpose: what the Operator curated, what the
/// Recorder had configured, and what the radio said about itself.
#[tokio::test]
async fn a_call_detail_names_every_radio_it_heard() {
    let app = TestApp::with_key("k").await;
    app.seed_unit(11, 1200, "Engine 1").await;
    app.seed_unit_range(11, 1200, 1201, 1299).await;
    app.upload_ok(common::CallUpload::new().set(
        "sources",
        r#"[{"src":1250,"pos":0,"tag":"E1 Portable"},{"src":9999,"pos":2}]"#,
    ))
    .await;

    let id = app.the_call().await.id;
    let units = &app.get_json(&format!("/api/call/{id}")).await["units"];

    assert_eq!(units[0]["ref"], 1250, "which radio keyed");
    assert_eq!(
        units[0]["unitLabel"], "Engine 1",
        "the apparatus it belongs to"
    );
    assert_eq!(
        units[0]["label"], "E1 Portable",
        "what the Recorder called it"
    );
    assert!(
        units[1].get("unitLabel").is_none(),
        "a radio nobody owns has no curated name: {units}"
    );
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
