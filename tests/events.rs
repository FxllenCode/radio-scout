//! **Events** — incidents that outlive **Retention** (#67, spec US 38), driven
//! end to end.
//!
//! What an Event *says* is unit-tested where it is decided — `src/event/` for
//! what freezing comes to and what a snapshot round-trips as, `src/event/page.rs`
//! for the markup a recipient reads. What is here is everything that can only be
//! said about a **running Instance**:
//!
//! - that a frozen member really is a **second object**, so the copy is a copy
//!   rather than a second row pointing at the same bytes
//! - that the members survive their Calls being pruned, which is the whole
//!   feature and the one thing no pure test can claim
//! - that **orphan-GC** spares the frozen copies — the inverse of #55's lesson,
//!   and the failure that would delete every Event one grace period after it was
//!   made, silently
//! - that deleting an Event releases them, and that nothing else does
//! - and that the share page and both exports work over real HTTP with real
//!   bytes

mod common;

use common::{CallUpload, TestApp};
use radio_scout::db::entities::{event, event_call};
use sea_orm::EntityTrait;

/// A transmission instant nothing else in this file shares — `tests/share.rs`'s
/// rule and for its reason: two uploads on one channel a few hundred
/// milliseconds apart are one transmission as far as **Ingest** is concerned
/// (#46).
fn a_fresh_instant() -> i64 {
    static NEXT: std::sync::atomic::AtomicI64 =
        std::sync::atomic::AtomicI64::new(1_669_740_338_000);
    NEXT.fetch_add(600_000, std::sync::atomic::Ordering::Relaxed)
}

/// An Instance, signed in, holding `n` stored Calls — their ids, oldest first.
async fn an_instance_with_calls(n: usize) -> (TestApp, Vec<i64>) {
    let app = TestApp::with_key("k").await;
    app.login().await;
    for _ in 0..n {
        let (status, _) = app.upload(CallUpload::new().at(a_fresh_instant())).await;
        assert_eq!(status, 200);
    }
    let mut ids: Vec<i64> = app.calls().await.iter().map(|call| call.id).collect();
    ids.sort_unstable();
    assert_eq!(ids.len(), n, "one row per upload");
    (app, ids)
}

/// Assemble an Event over these Calls, and hand back its id.
async fn an_event_over(app: &TestApp, name: &str, ids: &[i64]) -> i64 {
    let (status, body) = app
        .admin_post(
            "/api/admin/events",
            serde_json::json!({ "name": name, "callIds": ids }),
        )
        .await;
    assert_eq!(status, 201, "{body}");
    body["id"].as_i64().expect("an event id")
}

/// **The copy is a copy.** A frozen member points at an object of its own
/// holding the same bytes — not at the Call's, which is what makes the sweep
/// able to stay one pass over one archive.
#[tokio::test]
async fn freezing_a_call_writes_a_second_object() {
    let (app, ids) = an_instance_with_calls(1).await;
    let call = app.the_call().await;
    let original = app.object_bytes(&call.object_key).await.expect("audio");

    an_event_over(&app, "House fire", &ids).await;

    let member = event_call::Entity::find()
        .one(&app.db)
        .await
        .expect("read the member")
        .expect("one member");
    assert_ne!(
        member.object_key, call.object_key,
        "a frozen copy under a key of its own"
    );
    assert_eq!(
        app.object_bytes(&member.object_key).await.as_ref(),
        Some(&original),
        "holding the Call's own bytes"
    );
    assert_eq!(member.audio_size as usize, original.len());
    assert_eq!(
        member.call_id, call.id,
        "and remembering where it came from"
    );
}

/// **The headline.** Retention takes the Calls; the Event still plays.
#[tokio::test]
async fn an_event_outlives_the_calls_it_was_made_from() {
    let (mut app, ids) = an_instance_with_calls(2).await;
    let id = an_event_over(&app, "The tornado", &ids).await;
    let frozen: Vec<String> = event_call::Entity::find()
        .all(&app.db)
        .await
        .expect("members")
        .into_iter()
        .map(|member| member.object_key)
        .collect();

    // Every Call in the Archive is now older than the window, so the next sweep
    // takes all of them — and with them every object the *Calls* pointed at.
    for id in &ids {
        app.age_call(*id, 40 * 24 * 60 * 60 * 1_000).await;
    }
    app.restart_with(|config| config.retention.days = 30).await;
    app.settle().await;
    // Server-side state does not survive a restart, which is also true of the
    // thing being modelled.
    app.login().await;

    assert_eq!(app.calls().await.len(), 0, "the Archive aged out");
    assert_eq!(
        event_call::Entity::find()
            .all(&app.db)
            .await
            .expect("members")
            .len(),
        2,
        "and the Event did not"
    );
    for key in &frozen {
        assert!(app.stored(key).await, "{key} was released by the sweep");
    }

    // ...and it is still readable as an Event, not merely as rows.
    let (status, body) = app.admin_get(&format!("/api/admin/events/{id}")).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["members"].as_array().expect("members").len(), 2);
}

/// Adding a Call twice is one member. The unique index is what makes that true
/// under two open tabs rather than under a read-then-write that can race.
#[tokio::test]
async fn a_call_is_frozen_into_an_event_once() {
    let (app, ids) = an_instance_with_calls(1).await;
    let id = an_event_over(&app, "Twice", &ids).await;

    let (status, body) = app
        .admin_post(
            &format!("/api/admin/events/{id}/calls"),
            serde_json::json!({ "callIds": [ids[0], ids[0]] }),
        )
        .await;

    assert_eq!(status, 200, "{body}");
    assert_eq!(body["added"]["frozen"], 0);
    assert_eq!(body["added"]["alreadyHeld"], 2);
    assert_eq!(
        event_call::Entity::find()
            .all(&app.db)
            .await
            .expect("members")
            .len(),
        1
    );
}

/// A Call that is not there is counted and never fatal: an Operator
/// multi-selecting a page of results may be holding one Retention took while
/// they were reading it, and refusing the whole batch over it would lose the
/// nine they meant.
#[tokio::test]
async fn a_call_that_is_gone_is_counted_rather_than_fatal() {
    let (app, ids) = an_instance_with_calls(1).await;

    let (status, body) = app
        .admin_post(
            "/api/admin/events",
            serde_json::json!({ "name": "Mixed", "callIds": [ids[0], 999_999] }),
        )
        .await;

    assert_eq!(status, 201, "{body}");
    assert_eq!(body["added"]["frozen"], 1);
    assert_eq!(body["added"]["missing"], 1);
}

/// Deleting the Event releases the copies — rows first, then objects, so a
/// crash between them leaves an **Orphan** the GC reclaims rather than a member
/// whose audio 404s.
#[tokio::test]
async fn deleting_an_event_releases_its_frozen_copies() {
    let (app, ids) = an_instance_with_calls(2).await;
    let id = an_event_over(&app, "Released", &ids).await;
    let frozen: Vec<String> = event_call::Entity::find()
        .all(&app.db)
        .await
        .expect("members")
        .into_iter()
        .map(|member| member.object_key)
        .collect();
    assert_eq!(frozen.len(), 2);

    let (status, _) = app.admin_delete(&format!("/api/admin/events/{id}")).await;

    assert_eq!(status, 204);
    assert_eq!(
        event::Entity::find()
            .all(&app.db)
            .await
            .expect("events")
            .len(),
        0
    );
    for key in &frozen {
        assert!(!app.stored(key).await, "{key} outlived its Event");
    }
    // ...and the Calls it was made from are untouched: an Event is a copy.
    assert_eq!(app.calls().await.len(), 2);
}

/// **The inverse of #55's lesson.** A frozen copy has no Call row pointing at
/// it, which is exactly what orphan-GC deletes — so the keep-set has to know
/// about it, or every Event is silently emptied one grace period after it was
/// made and nothing fails until somebody opens one.
#[tokio::test]
async fn orphan_gc_spares_the_frozen_copies() {
    let (mut app, ids) = an_instance_with_calls(1).await;
    an_event_over(&app, "Spared", &ids).await;
    let member = event_call::Entity::find()
        .one(&app.db)
        .await
        .expect("read the member")
        .expect("one member");

    // No grace period at all, so the sweep judges every object it lists.
    app.restart_with(|config| {
        config.retention.orphan_grace = std::time::Duration::from_secs(0);
    })
    .await;
    app.settle().await;

    assert!(
        app.stored(&member.object_key).await,
        "orphan-gc reclaimed a frozen copy"
    );
}

/// **The cap counts what an Event is holding.** Frozen copies are stored audio
/// on the same disk as everything else, so a `max_size_gb` that could not see
/// them would stop being true the moment an Operator curated an incident — which
/// is the one failure that setting exists to prevent.
#[tokio::test]
async fn the_size_cap_counts_frozen_bytes() {
    let (app, ids) = an_instance_with_calls(3).await;
    // Freezing the oldest doubles its bytes on disk without adding a Call row.
    an_event_over(&app, "Kept", &ids[..1]).await;
    let stored: i64 = app
        .calls()
        .await
        .iter()
        .filter_map(|call| call.audio_size)
        .sum::<i64>();
    let frozen: i64 = event_call::Entity::find()
        .all(&app.db)
        .await
        .expect("members")
        .iter()
        .map(|member| member.audio_size)
        .sum::<i64>();

    // A cap the three Calls fit under on their own, and do not once the frozen
    // copy is counted beside them.
    let cap = stored as u64 + frozen as u64 / 2;
    let report = radio_scout::retention::sweep(
        &app.db,
        app.store.as_ref(),
        &radio_scout::retention::RetentionConfig {
            days: 0,
            max_size_bytes: Some(cap),
            ..Default::default()
        },
        1_700_000_000_000,
    )
    .await
    .expect("a sweep");

    assert!(
        report.over_cap > 0,
        "the cap ignored {frozen} frozen bytes: {report:?}"
    );
}

/// ...and it never takes one. An Event that is bigger than the whole cap empties
/// the Archive around it and then **stops, saying so** — which is the visible
/// failure rather than the quiet one, and the only honest answer when the thing
/// over the cap is the thing the Operator asked to keep forever.
#[tokio::test]
async fn the_size_cap_never_prunes_a_frozen_copy() {
    let (app, ids) = an_instance_with_calls(2).await;
    an_event_over(&app, "Bigger than the cap", &ids).await;
    let frozen: Vec<String> = event_call::Entity::find()
        .all(&app.db)
        .await
        .expect("members")
        .into_iter()
        .map(|member| member.object_key)
        .collect();
    let capture = app.store_logs();

    let report = radio_scout::retention::sweep(
        &app.db,
        app.store.as_ref(),
        &radio_scout::retention::RetentionConfig {
            days: 0,
            max_size_bytes: Some(1),
            ..Default::default()
        },
        1_700_000_000_000,
    )
    .await
    .expect("a sweep");

    assert_eq!(report.over_cap, 2, "the Archive went");
    assert_eq!(app.calls().await.len(), 0);
    for key in &frozen {
        assert!(app.stored(key).await, "{key} was pruned by the size cap");
    }
    // One line per sweep, not one per Call (ADR-0011 rule 8) — and it names the
    // frozen bytes, because that is the number an Operator has to act on.
    let said = capture.only_line_containing("archive is over its size cap");
    assert!(said.contains("frozen_bytes"), "{said}");
}

// ---------------------------------------------------------------------------
// The share page, and the two downloads behind it
// ---------------------------------------------------------------------------

/// Turn sharing on, and hand back the link the Operator would copy.
async fn shared(app: &TestApp, id: i64) -> String {
    let (status, _) = app
        .admin_patch(
            &format!("/api/admin/events/{id}"),
            serde_json::json!({ "shared": true }),
        )
        .await;
    assert_eq!(status, 200);

    let (status, body) = app
        .admin_get(&format!("/api/admin/events/{id}/share"))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["shared"], true);
    body["url"].as_str().expect("a link").to_string()
}

/// **The whole promise.** A link opens the incident, publicly, with no session
/// and no app — and every member on it plays from the Event's *own* copy.
#[tokio::test]
async fn a_shared_event_lists_and_plays_its_members() {
    let (app, ids) = an_instance_with_calls(2).await;
    let id = an_event_over(&app, "Mill Street fire", &ids).await;

    let link = shared(&app, id).await;

    let page = app.get(&link).await;
    assert_eq!(page.status(), 200);
    assert_eq!(
        common::header_of(&page, "content-type"),
        Some("text/html; charset=utf-8")
    );
    let html = page.text().await.expect("the page");
    assert!(html.contains("Mill Street fire"), "{html}");
    assert_eq!(html.matches("<audio controls").count(), 2, "{html}");

    // Every `<audio>` on it really serves bytes — the half no pure test can make.
    let mut played = 0;
    for src in audio_sources(&html) {
        let audio = app.get(&src).await;
        assert_eq!(audio.status(), 200, "{src}");
        assert!(!audio.bytes().await.expect("bytes").is_empty(), "{src}");
        played += 1;
    }
    assert_eq!(played, 2);
}

/// Every `src` of an `<audio>` element on a rendered page, unescaped.
fn audio_sources(html: &str) -> Vec<String> {
    html.split("<audio")
        .skip(1)
        .filter_map(|tag| tag.split_once("src=\""))
        .filter_map(|(_, rest)| rest.split_once('"'))
        .map(|(src, _)| src.replace("&amp;", "&"))
        .collect()
}

/// **A link opens one incident and reaches nothing else.** A member id belonging
/// to another Event is not a member of this one, which is the same promise a
/// Call's share link makes about one Call.
#[tokio::test]
async fn a_link_reaches_no_other_events_members() {
    let (app, ids) = an_instance_with_calls(2).await;
    let mine = an_event_over(&app, "Mine", &ids[..1]).await;
    let theirs = an_event_over(&app, "Theirs", &ids[1..]).await;
    let link = shared(&app, mine).await;
    let token = link.split("t=").nth(1).expect("a token").to_string();

    let (_, body) = app.admin_get(&format!("/api/admin/events/{theirs}")).await;
    let other = body["members"][0]["memberId"]
        .as_i64()
        .expect("a member id");

    let reached = app.get(&format!("/e/audio?t={token}&i={other}")).await;

    assert_eq!(reached.status(), 404);
}

/// **Turning sharing off is a revoke.** It is the only one an Operator has here,
/// so the URL has to be dead for good — a toggle that merely closed the door
/// would leave a leaked link with no way to kill it at all (#64's rule, which
/// deletes its row for the same reason). Sharing again therefore mints a
/// *different* link.
#[tokio::test]
async fn turning_sharing_off_kills_the_link_for_good() {
    let (app, ids) = an_instance_with_calls(1).await;
    let id = an_event_over(&app, "Toggled", &ids).await;
    let link = shared(&app, id).await;

    let (status, body) = app
        .admin_patch(
            &format!("/api/admin/events/{id}"),
            serde_json::json!({ "shared": false }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["shared"], false);
    assert_eq!(app.get(&link).await.status(), 404, "the link still opened");

    let reshared = shared(&app, id).await;
    assert_ne!(reshared, link, "the revoked URL came back");
    assert_eq!(app.get(&link).await.status(), 404, "...and still opens");
    assert_eq!(app.get(&reshared).await.status(), 200);
}

/// ...and asking for sharing an Event already has changes nothing, so a screen
/// that re-submits its form cannot break a link somebody is holding.
#[tokio::test]
async fn sharing_an_already_shared_event_keeps_its_link() {
    let (app, ids) = an_instance_with_calls(1).await;
    let id = an_event_over(&app, "Idempotent", &ids).await;
    let link = shared(&app, id).await;

    assert_eq!(shared(&app, id).await, link);
}

/// `[share] enabled` closes every door at once — an Operator turning sharing off
/// on the Instance means it, whatever any individual Event says.
#[tokio::test]
async fn an_instance_with_sharing_off_opens_no_event() {
    let (mut app, ids) = an_instance_with_calls(1).await;
    let id = an_event_over(&app, "Closed", &ids).await;
    let link = shared(&app, id).await;
    assert_eq!(app.get(&link).await.status(), 200);

    app.restart_with(|config| config.share.enabled = false)
        .await;

    assert_eq!(app.get(&link).await.status(), 404);
}

/// An Event nobody shared opens nothing, which is what "not shared" has to mean
/// from outside.
#[tokio::test]
async fn an_unshared_event_has_no_page() {
    let (app, ids) = an_instance_with_calls(1).await;
    an_event_over(&app, "Private", &ids).await;

    for path in ["/e", "/e?t=", "/e?t=not-a-token"] {
        assert_eq!(app.get(path).await.status(), 404, "{path}");
    }
}

/// **The token never reaches the log**, which is the whole reason it rides a
/// query string: `http_log` writes a request's path and never its query.
#[tokio::test]
async fn an_event_token_never_reaches_the_log() {
    let (app, ids) = an_instance_with_calls(1).await;
    let id = an_event_over(&app, "Quiet", &ids).await;
    let capture = app.store_logs();
    let link = shared(&app, id).await;
    let token = link.split("t=").nth(1).expect("a token").to_string();

    app.get(&link).await;
    app.get(&format!("/e?t={token}")).await;

    capture.assert_never_logged(&token);
}

// ---------------------------------------------------------------------------
// Both exports
// ---------------------------------------------------------------------------

/// An Event with real audio behind every member — a pure tone per Call, so a
/// stitched file has something to hold and a zip has something to compare.
async fn an_audible_event(app: &TestApp, name: &str, calls: usize) -> i64 {
    let mut ids = Vec::new();
    for n in 0..calls {
        let samples: Vec<f32> = (0..4_000)
            .map(|s| {
                (s as f32 * (400.0 + n as f32 * 100.0) * std::f32::consts::TAU / 8_000.0).sin()
            })
            .collect();
        let audio = common::wav(&samples, 8_000);
        let key = format!("k/{name}-{n}.wav");
        app.put_object(&key, &audio).await;
        ids.push(
            app.seed_call(
                radio_scout::db::repo::NewCall {
                    system_label: Some("Alpha".into()),
                    talkgroup_label: Some(format!("Channel {n}")),
                    duration_ms: Some(500),
                    audio_mime: Some("audio/wav".into()),
                    ..radio_scout::db::repo::NewCall::new(
                        100,
                        1 + n as i64,
                        1_000 + n as i64 * 1_000,
                    )
                },
                Some(radio_scout::blob::StoredAudio::written(key, audio.len())),
            )
            .await,
        );
    }
    an_event_over(app, name, &ids).await
}

/// **A zip of an incident**, named after the incident, with every frozen copy in
/// it and a manifest that says the Event it came from.
#[tokio::test]
async fn an_event_exports_as_a_zip() {
    let app = TestApp::spawn().await;
    app.login().await;
    let id = an_audible_event(&app, "Mill Street fire", 2).await;

    let (status, body, disposition) = admin_download(&app, id, "zip").await;

    assert_eq!(status, 200);
    assert!(
        disposition.contains("radio-scout-Mill-Street-fire.zip"),
        "{disposition}"
    );
    let Some(files) = unzipped(&body) else {
        return;
    };
    assert_eq!(files.len(), 3, "a manifest and two Calls");
    assert_eq!(files[0].0, "manifest.json");

    let manifest: serde_json::Value =
        serde_json::from_slice(&files[0].1).expect("manifest is JSON");
    // **`event`, not `search`** — an Event is not a range, and a manifest handed
    // to a stranger should say which of the two it describes.
    assert_eq!(manifest["event"], "Mill Street fire");
    assert!(manifest.get("search").is_none(), "{manifest}");
    let calls = manifest["calls"].as_array().expect("calls");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["talkgroupLabel"], "Channel 0");
    assert_eq!(calls[0]["file"], files[1].0);

    // The bytes are the **frozen copy's**, which are the Call's.
    let frozen = app
        .object_bytes(
            &event_call::Entity::find()
                .all(&app.db)
                .await
                .expect("members")[0]
                .object_key,
        )
        .await
        .expect("the frozen object");
    assert_eq!(files[1].1, frozen);
}

/// ...and as one stitched file, whose length it declares before reading a byte.
#[tokio::test]
async fn an_event_exports_as_one_stitched_file() {
    let app = TestApp::spawn().await;
    app.login().await;
    let id = an_audible_event(&app, "Stitched", 3).await;

    let response = app
        .admin_verb(
            reqwest::Method::GET,
            &format!("/api/admin/events/{id}/export?format=wav"),
            None,
        )
        .send()
        .await
        .expect("the download");

    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        common::header_of(&response, "content-type"),
        Some("audio/wav")
    );
    // Three half-second Calls at 8 kHz, 16-bit mono, behind a 44-byte header.
    let declared: u64 = common::header_of(&response, "content-length")
        .expect("a declared length")
        .parse()
        .expect("a number");
    assert_eq!(declared, 44 + 3 * 4_000 * 2);
    assert_eq!(
        response.bytes().await.expect("body").len() as u64,
        declared,
        "the body is exactly what the header promised"
    );
}

/// **The download reaches a recipient too.** Somebody was sent an incident;
/// keeping it should not mean an account on the Instance it came from.
#[tokio::test]
async fn a_shared_event_downloads_through_its_link() {
    let app = TestApp::spawn().await;
    app.login().await;
    let id = an_audible_event(&app, "Sent", 2).await;
    let link = shared(&app, id).await;
    let token = link.split("t=").nth(1).expect("a token").to_string();

    // The page offers both, and both are its own links rather than the admin's.
    let html = app.get(&link).await.text().await.expect("the page");
    assert!(
        html.contains(&format!("/e/export?t={token}&amp;format=zip")),
        "{html}"
    );

    let zip = app.get(&format!("/e/export?t={token}&format=zip")).await;
    assert_eq!(zip.status(), 200);
    let bytes = zip.bytes().await.expect("body").to_vec();
    if let Some(files) = unzipped(&bytes) {
        assert_eq!(files.len(), 3);
    }

    let wav = app.get(&format!("/e/export?t={token}&format=wav")).await;
    assert_eq!(wav.status(), 200);
    assert_eq!(common::header_of(&wav, "content-type"), Some("audio/wav"));
}

/// An Event nobody shared downloads to nobody, on the same terms as its page.
#[tokio::test]
async fn an_unshared_event_downloads_to_nobody() {
    let app = TestApp::spawn().await;
    app.login().await;
    an_audible_event(&app, "Private", 1).await;

    assert_eq!(
        app.get("/e/export?t=not-a-token&format=zip").await.status(),
        404
    );
}

/// An Event holding nothing is refused rather than answered with an empty
/// archive — [`Reason::ExportEmpty`]'s rule, which an Event reaches by a
/// different road.
#[tokio::test]
async fn an_empty_event_is_refused_rather_than_downloaded() {
    let (app, _) = an_instance_with_calls(0).await;
    let id = an_event_over(&app, "Nothing yet", &[]).await;

    let (status, ..) = admin_download(&app, id, "zip").await;

    assert_eq!(status, 404);
}

/// One download over the admin session: its status, its bytes and the name the
/// browser would save it as.
async fn admin_download(app: &TestApp, id: i64, format: &str) -> (u16, Vec<u8>, String) {
    let response = app
        .admin_verb(
            reqwest::Method::GET,
            &format!("/api/admin/events/{id}/export?format={format}"),
            None,
        )
        .send()
        .await
        .expect("the download");
    let status = response.status().as_u16();
    let disposition = common::header_of(&response, "content-disposition")
        .unwrap_or_default()
        .to_string();
    (
        status,
        response.bytes().await.expect("body").to_vec(),
        disposition,
    )
}

/// The zip, read back with the **system's own extractor** where the machine has
/// one — `tests/export.rs`'s rule: a container this suite writes and this suite
/// reads proves only that we are consistent with ourselves.
fn unzipped(bytes: &[u8]) -> Option<Vec<(String, Vec<u8>)>> {
    let dir = tempfile::tempdir().expect("tempdir");
    let zip = dir.path().join("event.zip");
    std::fs::write(&zip, bytes).expect("write the zip");
    let out = dir.path().join("out");

    let Ok(unzip) = std::process::Command::new("unzip")
        .arg("-q")
        .arg(&zip)
        .arg("-d")
        .arg(&out)
        .output()
    else {
        // Reported rather than silently passing, `tests/db.rs`'s rule: a skip
        // nobody sees is a test nobody runs.
        #[allow(clippy::print_stderr)]
        {
            eprintln!("SKIP: unzip is not installed");
        }
        return None;
    };
    assert!(
        unzip.status.success(),
        "unzip refused the archive: {}",
        String::from_utf8_lossy(&unzip.stderr)
    );

    let listing = std::process::Command::new("unzip")
        .arg("-Z1")
        .arg(&zip)
        .output()
        .expect("run unzip -Z1");
    Some(
        String::from_utf8_lossy(&listing.stdout)
            .lines()
            .map(|name| {
                let bytes = std::fs::read(out.join(name)).expect("an extracted file");
                (name.to_string(), bytes)
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// The two Calls that freeze differently
// ---------------------------------------------------------------------------

/// **An Encrypted Call freezes as a row and no object**, because that the
/// channel was busy is part of the incident — and an incident is exactly the
/// thing somebody assembles one out of. It reaches the page with a sentence
/// instead of a player, and the zip's manifest with no file beside it.
#[tokio::test]
async fn an_encrypted_call_freezes_as_metadata() {
    let app = TestApp::spawn().await;
    app.login().await;
    let encrypted = app
        .seed_call(
            radio_scout::db::repo::NewCall {
                encrypted: true,
                duration_ms: Some(500),
                talkgroup_label: Some("Tac 4".into()),
                ..radio_scout::db::repo::NewCall::new(100, 4, 5_000)
            },
            None,
        )
        .await;
    let id = an_event_over(&app, "Busy channel", &[encrypted]).await;

    let member = event_call::Entity::find()
        .one(&app.db)
        .await
        .expect("read the member")
        .expect("one member");
    assert_eq!(member.object_key, "", "nothing to copy, and nothing copied");
    assert_eq!(member.audio_size, 0);

    let link = shared(&app, id).await;
    let html = app.get(&link).await.text().await.expect("the page");
    assert!(html.contains("This call has no audio."), "{html}");
    assert!(!html.contains("<audio"), "{html}");

    // ...and it is still in the zip's manifest, where the activity is the point.
    let (status, body, _) = admin_download(&app, id, "zip").await;
    assert_eq!(status, 200);
    if let Some(files) = unzipped(&body) {
        assert_eq!(files.len(), 1, "a manifest and no audio file");
        let manifest: serde_json::Value =
            serde_json::from_slice(&files[0].1).expect("manifest is JSON");
        assert_eq!(manifest["calls"].as_array().expect("calls").len(), 1);
        assert_eq!(manifest["calls"][0]["file"], serde_json::Value::Null);
    }

    // Deleting it releases nothing and fails at nothing: there was no object.
    let (status, _) = app.admin_delete(&format!("/api/admin/events/{id}")).await;
    assert_eq!(status, 204);
}

/// **A Call whose audio will not read is not frozen at all.**
///
/// A member with no object is how an **Encrypted Call** is held, so writing one
/// here would store a lie in the one table built to be trusted years later. It
/// is counted instead — apart from "gone", because an Operator does something
/// different about a store that is unwell — and reported **once per request**
/// rather than once per Call (ADR-0011 rule 8, `crate::export`'s rule one
/// surface along). No row means adding it again once the store is back really
/// does freeze it.
#[tokio::test]
async fn a_call_whose_audio_will_not_read_is_counted_and_not_frozen() {
    let capture = common::logs::LogCapture::start();
    let tmp = tempfile::tempdir().expect("tempdir");
    let (store, faults) = common::faulty_store(tmp.path());
    let app = TestApp::builder().store(store).spawn().await;
    app.login().await;
    app.create_api_key("k").await;
    for _ in 0..3 {
        let (status, _) = app.upload(CallUpload::new().at(a_fresh_instant())).await;
        assert_eq!(status, 200);
    }
    let ids: Vec<i64> = app.calls().await.iter().map(|call| call.id).collect();

    faults.fail_reads();
    let (status, body) = app
        .admin_post(
            "/api/admin/events",
            serde_json::json!({ "name": "Unlucky", "callIds": ids }),
        )
        .await;

    assert_eq!(status, 201, "{body}");
    assert_eq!(body["added"]["unreadable"], 3);
    assert_eq!(body["added"]["frozen"], 0);
    assert_eq!(
        event_call::Entity::find()
            .all(&app.db)
            .await
            .expect("members")
            .len(),
        0,
        "a member with no audio would be a lie"
    );

    let lines = capture.lines_containing("could not be frozen");
    assert_eq!(lines.len(), 1, "one line per request: {lines:?}");
    assert!(
        lines[0].contains("calls=3"),
        "and it says how many: {}",
        lines[0]
    );
}

// ---------------------------------------------------------------------------
// The rest of the Operator's surface
// ---------------------------------------------------------------------------

/// The listing carries **what an Event is holding**, which is the number an
/// Operator is really being shown: these bytes are the one thing on the Instance
/// no policy can take back.
#[tokio::test]
async fn the_listing_counts_each_events_calls_and_bytes() {
    let (app, ids) = an_instance_with_calls(3).await;
    an_event_over(&app, "Small", &ids[..1]).await;
    an_event_over(&app, "Larger", &ids).await;

    let (status, body) = app.admin_get("/api/admin/events?limit=10&offset=0").await;

    assert_eq!(status, 200, "{body}");
    assert_eq!(body["count"], 2);
    // Newest first, so the one made last leads.
    let rows = body["results"].as_array().expect("rows");
    assert_eq!(rows[0]["name"], "Larger");
    assert_eq!(rows[0]["calls"], 3);
    assert_eq!(rows[1]["name"], "Small");
    assert_eq!(rows[1]["calls"], 1);
    assert!(
        rows[0]["bytes"].as_i64().expect("bytes") > rows[1]["bytes"].as_i64().expect("bytes"),
        "{body}"
    );
    // **The token is never in a listing** — the `ShareRow` rule, and the reason
    // it is fetched only when an Operator asks to copy it.
    assert!(rows[0].get("shareToken").is_none(), "{body}");
}

/// An Operator plays an Event on the screen that curates it, through a route of
/// its own — because an Event that is **not shared** still has to be playable,
/// and a screen that reached for the share token would be a screen holding a
/// credential it has no use for.
#[tokio::test]
async fn an_operator_plays_a_frozen_copy_without_sharing_it() {
    let (app, ids) = an_instance_with_calls(1).await;
    let id = an_event_over(&app, "Unshared", &ids).await;
    let (_, body) = app.admin_get(&format!("/api/admin/events/{id}")).await;
    let member = body["members"][0]["memberId"]
        .as_i64()
        .expect("a member id");
    let url = body["members"][0]["audioUrl"]
        .as_str()
        .expect("an audio url");
    assert_eq!(url, format!("/api/admin/events/{id}/calls/{member}/audio"));

    let audio = app
        .admin_verb(reqwest::Method::GET, url, None)
        .send()
        .await
        .expect("the audio");

    assert_eq!(audio.status().as_u16(), 200);
    assert!(!audio.bytes().await.expect("bytes").is_empty());
}

/// ...and a member id belonging to another Event reaches nothing, which is the
/// same promise the share link makes.
#[tokio::test]
async fn an_events_audio_route_reaches_only_its_own_members() {
    let (app, ids) = an_instance_with_calls(2).await;
    let mine = an_event_over(&app, "Mine", &ids[..1]).await;
    let theirs = an_event_over(&app, "Theirs", &ids[1..]).await;
    let (_, body) = app.admin_get(&format!("/api/admin/events/{theirs}")).await;
    let other = body["members"][0]["memberId"]
        .as_i64()
        .expect("a member id");

    let (status, _) = app
        .admin_get(&format!("/api/admin/events/{mine}/calls/{other}/audio"))
        .await;

    assert_eq!(status, 404);
}

/// Dropping one member releases **that** copy and leaves the rest — and stamps
/// the Event, because what it holds moved.
#[tokio::test]
async fn dropping_one_member_releases_only_its_copy() {
    let (app, ids) = an_instance_with_calls(2).await;
    let id = an_event_over(&app, "Trimmed", &ids).await;
    let members = event_call::Entity::find()
        .all(&app.db)
        .await
        .expect("members");
    let (going, staying) = (&members[0], &members[1]);
    let before = event::Entity::find()
        .one(&app.db)
        .await
        .expect("read")
        .expect("the event")
        .updated_at_ms;

    let (status, _) = app
        .admin_delete(&format!("/api/admin/events/{id}/calls/{}", going.id))
        .await;

    assert_eq!(status, 204);
    assert!(!app.stored(&going.object_key).await, "its copy stayed");
    assert!(
        app.stored(&staying.object_key).await,
        "its neighbour's went"
    );
    let after = event::Entity::find()
        .one(&app.db)
        .await
        .expect("read")
        .expect("the event");
    assert!(after.updated_at_ms >= before, "the event was not stamped");
    assert_eq!(
        event_call::Entity::find()
            .all(&app.db)
            .await
            .expect("members")
            .len(),
        1
    );
}

/// Every route refuses an Event that is not there, in the one refusal
/// vocabulary — so a client can tell "you typed the wrong id" from "the server
/// broke".
#[tokio::test]
async fn every_route_refuses_an_event_that_is_not_there() {
    let (app, _) = an_instance_with_calls(0).await;

    for path in [
        "/api/admin/events/404",
        "/api/admin/events/404/share",
        "/api/admin/events/404/calls/1/audio",
        "/api/admin/events/404/export",
    ] {
        let (status, _) = app.admin_get(path).await;
        assert_eq!(status, 404, "GET {path}");
    }
    let (status, _) = app
        .admin_post(
            "/api/admin/events/404/calls",
            serde_json::json!({ "callIds": [] }),
        )
        .await;
    assert_eq!(status, 404);
    let (status, _) = app.admin_delete("/api/admin/events/404").await;
    assert_eq!(status, 404);
    let (status, _) = app.admin_delete("/api/admin/events/404/calls/1").await;
    assert_eq!(status, 404);
}

/// A blank name is refused, because an incident nobody can find again is not
/// one that was kept.
#[tokio::test]
async fn an_event_needs_a_name() {
    let (app, _) = an_instance_with_calls(0).await;

    let (status, body) = app
        .admin_post("/api/admin/events", serde_json::json!({ "name": "   " }))
        .await;

    assert_eq!(status, 400);
    assert_eq!(body["error"], "field-required");
}

/// A `PATCH` tells **absent from null**, so a note can be cleared as well as
/// set — `curate::nullable`'s whole reason for existing.
#[tokio::test]
async fn a_note_is_set_and_cleared() {
    let (app, _) = an_instance_with_calls(0).await;
    let id = an_event_over(&app, "Annotated", &[]).await;

    let (_, body) = app
        .admin_patch(
            &format!("/api/admin/events/{id}"),
            serde_json::json!({ "notes": "Second alarm" }),
        )
        .await;
    assert_eq!(body["notes"], "Second alarm");

    // Absent leaves it alone...
    let (_, body) = app
        .admin_patch(
            &format!("/api/admin/events/{id}"),
            serde_json::json!({ "name": "Renamed" }),
        )
        .await;
    assert_eq!(body["notes"], "Second alarm");
    assert_eq!(body["name"], "Renamed");

    // ...and `null` clears it.
    let (_, body) = app
        .admin_patch(
            &format!("/api/admin/events/{id}"),
            serde_json::json!({ "notes": null }),
        )
        .await;
    assert!(body["notes"].is_null(), "{body}");
}

/// **An object that will not delete is counted, never fatal.** The row is
/// already gone, so the copy is an **Orphan** a later sweep reclaims — and one
/// unhappy object must not wedge a delete forever.
#[tokio::test]
async fn a_copy_that_will_not_delete_does_not_wedge_the_delete() {
    let capture = common::logs::LogCapture::start();
    let tmp = tempfile::tempdir().expect("tempdir");
    let (store, faults) = common::faulty_store(tmp.path());
    let app = TestApp::builder().store(store).spawn().await;
    app.login().await;
    app.create_api_key("k").await;
    let (status, _) = app.upload(CallUpload::new().at(a_fresh_instant())).await;
    assert_eq!(status, 200);
    let id = an_event_over(&app, "Stuck", &[app.the_call().await.id]).await;

    faults.fail_deletes();
    let (status, _) = app.admin_delete(&format!("/api/admin/events/{id}")).await;

    assert_eq!(status, 204, "a broken store must not wedge the delete");
    assert_eq!(
        event::Entity::find()
            .all(&app.db)
            .await
            .expect("events")
            .len(),
        0
    );
    assert_eq!(
        event_call::Entity::find()
            .all(&app.db)
            .await
            .expect("members")
            .len(),
        0
    );
    assert_eq!(
        capture
            .lines_containing("could not delete a released event copy")
            .len(),
        1
    );
}

/// An `/e/audio` link with no token, a wrong one, or no member names nothing —
/// the page's own three answers, reached through the other door.
#[tokio::test]
async fn an_events_public_audio_refuses_what_it_cannot_open() {
    let (app, ids) = an_instance_with_calls(1).await;
    let id = an_event_over(&app, "Guarded", &ids).await;
    let link = shared(&app, id).await;
    let token = link.split("t=").nth(1).expect("a token").to_string();

    for path in [
        String::from("/e/audio"),
        String::from("/e/audio?t=not-a-token&i=1"),
        format!("/e/audio?t={token}"),
        format!("/e/audio?t={token}&i=99999"),
    ] {
        assert_eq!(app.get(&path).await.status(), 404, "{path}");
    }
}

/// **A flattened `StoredCall` carries an `id`, and so did the member around it.**
///
/// Both serialized to the same key and the last one written won, so `id` was the
/// *Call's* — which the client was reading as the member's, and using to address
/// a delete. It is `EventDetail`'s own `members`-not-`calls` note, missed one
/// struct below, and it is why the member's own id is spelled `memberId`.
#[tokio::test]
async fn a_member_says_which_call_it_was_and_which_member_it_is() {
    // **Three Calls, and only the last is frozen**, so the Call's id and the
    // member's cannot coincide — each table numbers from one, and a test over a
    // single Call would have both at `1` and prove nothing about which key the
    // document is carrying.
    let (app, ids) = an_instance_with_calls(3).await;
    let call = *ids.last().expect("a call");
    let id = an_event_over(&app, "Addressed", &[call]).await;

    let (_, body) = app.admin_get(&format!("/api/admin/events/{id}")).await;

    let member = &body["members"][0];
    assert_eq!(member["id"], call, "the Call it was frozen from");
    let member_id = member["memberId"].as_i64().expect("a member id");
    assert_ne!(
        member_id, call,
        "the member's own id, which is a different key"
    );
    // ...and it is the one that addresses this member, not the Call's.
    let (status, _) = app
        .admin_delete(&format!("/api/admin/events/{id}/calls/{member_id}"))
        .await;
    assert_eq!(status, 204);
}

/// **A batch this Instance should not try to copy in one request is refused.**
///
/// Each Call is an object read and an object written, sequentially, inside one
/// HTTP request — so a select-all over a county would be a request that outlives
/// its own timeout while the work went on behind it. The refusal names the count,
/// `[export] max_calls`'s rule, so "add fewer" is actionable.
#[tokio::test]
async fn a_batch_too_big_to_copy_in_one_request_is_refused() {
    let (app, _) = an_instance_with_calls(0).await;
    let too_many: Vec<i64> = (1..=501).collect();

    let (status, body) = app
        .admin_post(
            "/api/admin/events",
            serde_json::json!({ "name": "Everything", "callIds": too_many }),
        )
        .await;

    assert_eq!(status, 413);
    assert_eq!(body["error"], "too-many-calls");
    assert!(
        body["detail"].as_str().expect("a sentence").contains("501"),
        "{body}"
    );
    // ...and nothing was created on the way to refusing.
    assert_eq!(
        event::Entity::find()
            .all(&app.db)
            .await
            .expect("events")
            .len(),
        0
    );

    // The same bound on the other door.
    let id = an_event_over(&app, "Existing", &[]).await;
    let (status, _) = app
        .admin_post(
            &format!("/api/admin/events/{id}/calls"),
            serde_json::json!({ "callIds": (1..=501).collect::<Vec<i64>>() }),
        )
        .await;
    assert_eq!(status, 413);
}

/// **A create that could not be filled is undone.**
///
/// The row and its members are two writes and nothing can make them one —
/// freezing copies *objects*, which no database transaction reaches. So an
/// Event named and left empty by a 500 is one an Operator retries, and a retry
/// that accumulates duplicates is worse than a failure that leaves nothing
/// behind.
#[tokio::test]
async fn an_event_that_could_not_be_filled_is_not_left_behind() {
    let (app, ids) = an_instance_with_calls(2).await;

    // **Refusing the stamp, not the insert**, which is what makes this the
    // interesting case: `freeze` copies both objects and writes both member
    // rows, and only then fails on the `UPDATE` that stamps the Event. So the
    // compensation has real work to do — rows to drop *and* copies to release —
    // where refusing the whole table would only prove that a create with nothing
    // in it leaves nothing behind.
    app.refuse_updates_to("events");
    let (status, _) = app
        .admin_post(
            "/api/admin/events",
            serde_json::json!({ "name": "Doomed", "callIds": ids }),
        )
        .await;

    assert_eq!(status, 500);
    assert_eq!(
        event::Entity::find()
            .all(&app.db)
            .await
            .expect("events")
            .len(),
        0,
        "a named, empty event survived a failed create"
    );
    assert_eq!(
        event_call::Entity::find()
            .all(&app.db)
            .await
            .expect("members")
            .len(),
        0
    );
    // ...and the copies went with them, so a retry does not leave the disk
    // carrying an incident nobody has.
    assert_eq!(
        app.object_keys().await.len(),
        2,
        "the two Calls' own objects, and no frozen copies beside them"
    );
}

/// ...and when the compensation cannot run either, what survives is an **empty
/// Event**, which is visible on the listing and deletable.
///
/// The honest limit rather than a claim of atomicity: the row and the members
/// are two writes and freezing copies *objects*, which no database transaction
/// reaches. A database refusing everything about `event_calls` refuses the
/// undo's own read as well.
#[tokio::test]
async fn an_undo_that_cannot_run_leaves_an_empty_event() {
    let capture = common::logs::LogCapture::start();
    let (app, ids) = an_instance_with_calls(1).await;

    app.refuse_statements_on("event_calls");
    let (status, _) = app
        .admin_post(
            "/api/admin/events",
            serde_json::json!({ "name": "Stranded", "callIds": ids }),
        )
        .await;

    assert_eq!(status, 500);
    let events = event::Entity::find().all(&app.db).await.expect("events");
    assert_eq!(events.len(), 1, "and it is there to be deleted");
    assert_eq!(events[0].name, "Stranded");
    // The line is the only record there is: the `Failure` the caller is told
    // about is the *freeze*, and this one is dropped.
    assert_eq!(
        capture
            .lines_containing("could not undo an event that failed to fill")
            .len(),
        1
    );
}
