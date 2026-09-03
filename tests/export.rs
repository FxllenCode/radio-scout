//! Taking a range of the Archive with you (#65, spec US 33) —
//! `GET /api/calls/export`, in both of its formats.
//!
//! Driven over the real HTTP boundary via the integration harness (ADR-0009),
//! and read back with a **real extractor** where the machine has one: a zip this
//! suite writes and this suite reads proves only that we are consistent with
//! ourselves, which is precisely the failure mode a hand-rolled container format
//! has (`tests/uploadscript.rs`'s argument, one artifact along).

mod common;
use common::TestApp;
use common::logs::LogCapture;
use common::wav;

use radio_scout::db::repo::NewCall;
use rstest::rstest;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Seeding
// ---------------------------------------------------------------------------

/// A Call with real audio behind it: a pure tone at `hz`, `ms` long, stored
/// under its own key, with the length written down the way ingest writes it.
async fn seed_audible_call(app: &TestApp, talkgroup_ref: i64, at_ms: i64, hz: f32, ms: i64) -> i64 {
    let samples: Vec<f32> = (0..(ms as usize * 8_000) / 1000)
        .map(|n| (n as f32 * hz * std::f32::consts::TAU / 8_000.0).sin() * 0.5)
        .collect();
    let audio = wav(&samples, 8_000);
    let key = format!("k/{talkgroup_ref}-{at_ms}.wav");
    app.put_object(&key, &audio).await;
    app.seed_call(
        NewCall {
            system_label: Some("Alpha".into()),
            talkgroup_label: Some(format!("Channel {talkgroup_ref}")),
            duration_ms: Some(ms),
            audio_mime: Some("audio/wav".into()),
            ..NewCall::new(100, talkgroup_ref, at_ms)
        },
        Some(radio_scout::blob::StoredAudio::written(key, audio.len())),
    )
    .await
}

/// The whole body of an export, as bytes.
async fn export(app: &TestApp, query: &str) -> Vec<u8> {
    let response = app.get(&format!("/api/calls/export{query}")).await;
    assert_eq!(response.status().as_u16(), 200, "export refused");
    response.bytes().await.expect("body").to_vec()
}

// ---------------------------------------------------------------------------
// The zip
// ---------------------------------------------------------------------------

/// The headline: a range comes back as an archive a stranger can open, holding
/// every Call's audio byte-for-byte and a manifest describing them.
#[tokio::test]
async fn a_zip_holds_every_calls_audio_and_a_manifest() {
    let app = TestApp::spawn().await;
    seed_audible_call(&app, 1, 1_000, 1000.0, 500).await;
    seed_audible_call(&app, 1, 2_000, 1400.0, 500).await;

    let response = app.get("/api/calls/export?format=zip").await;

    assert_eq!(
        common::header_of(&response, "content-type"),
        Some("application/zip")
    );
    let disposition = common::header_of(&response, "content-disposition")
        .expect("an attachment")
        .to_string();
    assert!(disposition.starts_with("attachment; "), "{disposition}");
    assert!(disposition.ends_with(".zip\""), "{disposition}");

    let bytes = response.bytes().await.expect("body").to_vec();
    let Some(files) = unzipped(&bytes) else {
        return;
    };
    let names: Vec<&String> = files.iter().map(|(name, _)| name).collect();
    assert_eq!(names.len(), 3, "a manifest and two Calls: {names:?}");
    assert_eq!(names[0], "manifest.json", "the manifest comes first");
    assert!(names[1].ends_with(".wav"), "{names:?}");

    let stored = app
        .object_bytes("k/1-1000.wav")
        .await
        .expect("the stored object");
    assert_eq!(files[1].1, stored, "the audio is the stored audio");
}

/// The manifest is what makes a folder of audio an archive of *Calls*: it names
/// every Call in it, and which file in the zip is which.
#[tokio::test]
async fn the_manifest_says_which_file_is_which_call() {
    let app = TestApp::spawn().await;
    let first = seed_audible_call(&app, 7, 1_000, 1000.0, 500).await;

    let bytes = export(&app, "?format=zip").await;

    let Some(files) = unzipped(&bytes) else {
        return;
    };
    let manifest: Value = serde_json::from_slice(&files[0].1).expect("manifest is JSON");
    let calls = manifest["calls"].as_array().expect("calls array");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["id"], first);
    assert_eq!(calls[0]["talkgroupRef"], 7);
    assert_eq!(calls[0]["talkgroupLabel"], "Channel 7");
    assert_eq!(
        calls[0]["file"], files[1].0,
        "the manifest names the file this Call is in"
    );
}

/// An **Encrypted Call** is a row and no object (#42, spec US 9). It belongs in
/// the manifest — the activity is the fact — and there is nothing to put a file
/// there for.
#[tokio::test]
async fn an_encrypted_call_is_in_the_manifest_with_no_file() {
    let app = TestApp::spawn().await;
    seed_audible_call(&app, 1, 1_000, 1000.0, 500).await;
    app.seed_call(
        NewCall {
            encrypted: true,
            duration_ms: Some(500),
            ..NewCall::new(100, 1, 2_000)
        },
        None,
    )
    .await;

    let bytes = export(&app, "?format=zip").await;

    let Some(files) = unzipped(&bytes) else {
        return;
    };
    let manifest: Value = serde_json::from_slice(&files[0].1).expect("manifest is JSON");
    assert_eq!(manifest["calls"].as_array().expect("calls").len(), 2);
    assert_eq!(files.len(), 2, "one manifest, one audio file");
    assert!(
        manifest["calls"][1]["file"].is_null(),
        "an encrypted Call names no file"
    );
}

// ---------------------------------------------------------------------------
// The stitch
// ---------------------------------------------------------------------------

/// The stitched export's whole design in one assertion: the length is known
/// before any audio is read, so the response can state it — and the body is
/// exactly that long.
#[tokio::test]
async fn a_stitched_export_states_its_length_before_it_reads_any_audio() {
    let app = TestApp::spawn().await;
    seed_audible_call(&app, 1, 1_000, 1000.0, 500).await;
    seed_audible_call(&app, 1, 2_000, 1400.0, 1_500).await;

    let response = app.get("/api/calls/export?format=wav").await;

    assert_eq!(
        common::header_of(&response, "content-type"),
        Some("audio/wav")
    );
    // 2 seconds of mono 16-bit at 8 kHz, behind a 44-byte header.
    let expected = 44 + 2 * 8_000 * 2;
    assert_eq!(
        common::header_of(&response, "content-length"),
        Some(expected.to_string().as_str())
    );

    let bytes = response.bytes().await.expect("body").to_vec();
    assert_eq!(bytes.len(), expected);
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(
        u32::from_le_bytes(bytes[40..44].try_into().expect("four bytes")),
        (expected - 44) as u32,
        "the header's own data length"
    );
}

/// Chronological, whatever order the Calls were stored in and whatever order
/// the search asked for — a stitched incident that played backwards would be
/// worse than no export.
#[tokio::test]
async fn a_stitched_export_plays_oldest_first() {
    let app = TestApp::spawn().await;
    seed_audible_call(&app, 1, 5_000, 2000.0, 1_000).await;
    seed_audible_call(&app, 1, 1_000, 500.0, 1_000).await;

    let bytes = export(&app, "?format=wav&sort=newest").await;

    let samples = &bytes[44..];
    assert_eq!(samples.len(), 2 * 8_000 * 2);
    assert!(
        dominant_hz(&samples[..8_000 * 2]) < dominant_hz(&samples[8_000 * 2..]),
        "the 500 Hz Call was recorded first and must be heard first"
    );
}

/// A Call this Instance never measured cannot be placed on a declared timeline,
/// and an **Encrypted Call** has nothing to place. Both are simply not in the
/// file — which is what keeps the header true.
#[tokio::test]
async fn the_stitch_holds_only_what_it_can_place() {
    let app = TestApp::spawn().await;
    seed_audible_call(&app, 1, 1_000, 1000.0, 1_000).await;
    app.seed_call(
        NewCall {
            duration_ms: None,
            ..NewCall::new(100, 1, 2_000)
        },
        common::audio_at("k/unmeasured.wav"),
    )
    .await;
    app.seed_call(
        NewCall {
            encrypted: true,
            duration_ms: Some(9_000),
            ..NewCall::new(100, 1, 3_000)
        },
        None,
    )
    .await;

    let bytes = export(&app, "?format=wav").await;

    assert_eq!(bytes.len(), 44 + 8_000 * 2, "one second, and only one");
}

/// An object retention took between the pre-pass and the read still takes up
/// the room the header reserved for it. The alternative is a valid header over
/// a short body, which plays as every later Call being the wrong one.
#[tokio::test]
async fn a_call_whose_object_has_gone_is_silence_of_the_right_length() {
    let app = TestApp::spawn().await;
    seed_audible_call(&app, 1, 1_000, 1000.0, 1_000).await;
    app.seed_call(
        NewCall {
            duration_ms: Some(1_000),
            ..NewCall::new(100, 1, 2_000)
        },
        common::audio_at("k/never-written.wav"),
    )
    .await;

    let bytes = export(&app, "?format=wav").await;

    assert_eq!(bytes.len(), 44 + 2 * 8_000 * 2);
    assert!(
        bytes[44 + 8_000 * 2..].iter().all(|byte| *byte == 0),
        "the missing Call's place is silence"
    );
}

// ---------------------------------------------------------------------------
// The filters are the search's own
// ---------------------------------------------------------------------------

/// The export's filters are the results' filters, read by the same parser — so
/// what comes down is what was on screen (#62's rule, one surface along).
#[tokio::test]
async fn an_export_carries_exactly_what_the_search_matched() {
    let app = TestApp::spawn().await;
    seed_audible_call(&app, 1, 1_000, 1000.0, 500).await;
    seed_audible_call(&app, 2, 2_000, 1000.0, 500).await;
    seed_audible_call(&app, 1, 9_000, 1000.0, 500).await;

    let bytes = export(&app, "?format=zip&talkgroup=1&before=5000").await;

    let Some(files) = unzipped(&bytes) else {
        return;
    };
    let manifest: Value = serde_json::from_slice(&files[0].1).expect("manifest is JSON");
    let calls = manifest["calls"].as_array().expect("calls");
    assert_eq!(calls.len(), 1, "one Call matched: {calls:?}");
    assert_eq!(calls[0]["timestamp"], 1_000);
}

// ---------------------------------------------------------------------------
// Reading a zip back, and hearing one
// ---------------------------------------------------------------------------

/// Report a skipped test to whoever is reading the run — there is no subscriber
/// installed here, and a test that silently passes for want of a tool is
/// indistinguishable from one that proved something.
#[allow(clippy::print_stderr)]
fn skip(reason: &str) {
    eprintln!("skipping export test: {reason}");
}

/// Whether this machine can read a zip back.
fn unzip_available() -> bool {
    std::process::Command::new("unzip")
        .arg("-v")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Every file in a zip, in the order the zip holds them, read with the system's
/// own `unzip` — `None` where the machine has none.
///
/// **Skipped loudly rather than silently**, the way the Postgres, real-S3 and
/// `uploadScript` suites are, because a container format nobody else has ever
/// read is exactly the thing that is wrong.
fn unzipped(bytes: &[u8]) -> Option<Vec<(String, Vec<u8>)>> {
    if !unzip_available() {
        skip("unzip is not installed");
        return None;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let zip = dir.path().join("export.zip");
    std::fs::write(&zip, bytes).expect("write the zip");

    let out = dir.path().join("out");
    let unzip = std::process::Command::new("unzip")
        .arg("-q")
        .arg(&zip)
        .arg("-d")
        .arg(&out)
        .output()
        .expect("run unzip");
    assert!(
        unzip.status.success(),
        "unzip refused the zip: {}{}",
        String::from_utf8_lossy(&unzip.stdout),
        String::from_utf8_lossy(&unzip.stderr)
    );

    // The zip's own order, which `unzip -Z1` lists and a directory walk does
    // not.
    let listing = std::process::Command::new("unzip")
        .arg("-Z1")
        .arg(&zip)
        .output()
        .expect("run unzip -Z1");
    Some(
        String::from_utf8_lossy(&listing.stdout)
            .lines()
            .map(|name| {
                (
                    name.to_string(),
                    std::fs::read(out.join(name)).expect("extracted file"),
                )
            })
            .collect(),
    )
}

/// The strongest frequency in a block of mono 16-bit PCM at 8 kHz, to the
/// nearest of the two tones the fixtures use. Enough to tell which Call is
/// playing, which is the only question asked of it.
fn dominant_hz(pcm: &[u8]) -> u32 {
    let samples: Vec<f32> = pcm
        .as_chunks::<2>()
        .0
        .iter()
        .map(|s| i16::from_le_bytes(*s) as f32 / i16::MAX as f32)
        .collect();
    (100..3_000)
        .step_by(50)
        .max_by(|a, b| {
            energy_at(&samples, *a as f32)
                .partial_cmp(&energy_at(&samples, *b as f32))
                .expect("comparable")
        })
        .expect("a band")
}

/// Goertzel: how much of `hz` is in `samples`.
fn energy_at(samples: &[f32], hz: f32) -> f32 {
    let coefficient = 2.0 * (std::f32::consts::TAU * hz / 8_000.0).cos();
    let (mut previous, mut before) = (0.0f32, 0.0f32);
    for sample in samples {
        let current = sample + coefficient * previous - before;
        before = previous;
        previous = current;
    }
    previous * previous + before * before - coefficient * previous * before
}

// ---------------------------------------------------------------------------
// A big range, and what bounds one
// ---------------------------------------------------------------------------

/// **The criterion: a big range never buffers whole.**
///
/// The proof is the **absent `Content-Length`**, and it is worth being precise
/// about why that is the proof rather than the timing. An implementation that
/// assembled the zip and *then* answered would be handing hyper a body of known
/// size, and hyper would state it; only a body whose length the server does not
/// know when it commits to the response is sent chunked. So a `content-length`
/// appearing here would mean the archive had been built before the first byte
/// went out, whatever the rest of the test observed.
///
/// Six hundred Calls — twelve pages of the export's own paging — are what make
/// it a range rather than a page, and reading the first chunk before draining
/// the rest is what shows the body is delivered as it is written.
#[tokio::test]
async fn a_large_range_starts_arriving_before_it_has_finished() {
    let app = TestApp::builder()
        .config(|config| config.export.max_calls = 1_000)
        .spawn()
        .await;
    let audio = wav(&vec![0.25; 8_000], 8_000);
    app.put_object("k/bulk.wav", &audio).await;
    for n in 0..600 {
        app.seed_call(
            NewCall {
                duration_ms: Some(1_000),
                audio_mime: Some("audio/wav".into()),
                ..NewCall::new(100, 1, 1_000 + n)
            },
            Some(radio_scout::blob::StoredAudio::written(
                "k/bulk.wav".to_string(),
                audio.len(),
            )),
        )
        .await;
    }

    let mut response = app.get("/api/calls/export?format=zip").await;
    assert_eq!(response.status().as_u16(), 200);

    assert_eq!(
        common::header_of(&response, "content-length"),
        None,
        "a zip whose length the server knew is a zip it had already built"
    );
    assert_eq!(
        common::header_of(&response, "transfer-encoding"),
        Some("chunked")
    );

    let first = response
        .chunk()
        .await
        .expect("a chunk")
        .expect("the body starts");
    let mut total = first.len();
    assert_eq!(
        &first[..4],
        b"PK\x03\x04",
        "the archive starts with an entry"
    );
    while let Some(chunk) = response.chunk().await.expect("chunk") {
        total += chunk.len();
    }

    assert!(
        first.len() < total,
        "the whole archive arrived in one piece, so it was not streamed"
    );
    assert!(
        total > 600 * audio.len(),
        "every Call's audio should be in there: {total}"
    );
}

/// A range bigger than the cap is refused with **both numbers**, because
/// "narrow it" is only actionable if a Listener knows by how much.
#[tokio::test]
async fn a_range_over_the_cap_is_refused_with_the_count() {
    let app = TestApp::builder()
        .config(|config| config.export.max_calls = 2)
        .spawn()
        .await;
    for n in 0..3 {
        seed_audible_call(&app, 1, 1_000 + n, 1000.0, 100).await;
    }

    let response = app.get("/api/calls/export?format=zip").await;

    assert_eq!(response.status().as_u16(), 413);
    let body = response.text().await.expect("body");
    assert!(body.contains('3') && body.contains('2'), "{body}");
}

/// A range longer than a 32-bit container can address is refused **before a
/// byte is written**, rather than discovered four gigabytes into a download.
/// Both formats have the same ceiling, for the same reason, so both are asked.
#[rstest]
#[case::a_zip_of_more_audio_than_offsets_can_reach("zip")]
#[case::a_stitch_longer_than_a_wav_can_state("wav")]
#[tokio::test]
async fn a_range_the_format_cannot_address_is_refused_before_it_starts(#[case] format: &str) {
    let app = TestApp::spawn().await;
    app.seed_call(
        NewCall {
            // 74 hours of audio, which is both more bytes than a ZIP offset can
            // reach and more samples than a WAV can state.
            duration_ms: Some(300_000_000),
            ..NewCall::new(100, 1, 1_000)
        },
        Some(radio_scout::blob::StoredAudio::written(
            "k/huge.wav".to_string(),
            5_000_000_000,
        )),
    )
    .await;

    let response = app.get(&format!("/api/calls/export?format={format}")).await;

    assert_eq!(response.status().as_u16(), 413);
    assert!(
        response.text().await.expect("body").contains("address"),
        "the refusal should say why"
    );
}

/// A search that matched nothing is refused rather than answered with an empty
/// archive — a download that *looks* like it worked is how a Listener who
/// mistyped a date finds out on the aeroplane.
#[rstest]
#[case::a_zip("zip")]
#[case::a_stitch("wav")]
#[tokio::test]
async fn an_export_of_nothing_is_refused(#[case] format: &str) {
    let app = TestApp::spawn().await;
    seed_audible_call(&app, 1, 1_000, 1000.0, 500).await;

    let response = app
        .get(&format!("/api/calls/export?format={format}&talkgroup=999"))
        .await;

    assert_eq!(response.status().as_u16(), 404);
}

/// An Operator who turned exporting off turned it off for everybody, including
/// whoever has the URL.
#[tokio::test]
async fn an_instance_with_export_off_answers_no_such_thing() {
    let app = TestApp::builder()
        .config(|config| config.export.enabled = false)
        .spawn()
        .await;
    seed_audible_call(&app, 1, 1_000, 1000.0, 500).await;

    let response = app.get("/api/calls/export?format=zip").await;

    assert_eq!(response.status().as_u16(), 404);
    assert_eq!(
        app.get_json("/api/catalog").await["export"]["enabled"],
        false,
        "and the client is told, so it never draws the control"
    );
}

/// The catalog carries the cap, so the client can say "that is 4,312 Calls"
/// *before* the wait rather than after it (#64's rule).
#[tokio::test]
async fn the_catalog_says_what_an_export_may_hold() {
    let app = TestApp::builder()
        .config(|config| config.export.max_calls = 250)
        .spawn()
        .await;

    let catalog = app.get_json("/api/catalog").await;

    assert_eq!(catalog["export"]["enabled"], true);
    assert_eq!(catalog["export"]["maxCalls"], 250);
}

/// A format nobody has heard of is a mistyped URL, and says so.
#[tokio::test]
async fn a_format_that_is_neither_is_refused_by_name() {
    let app = TestApp::spawn().await;
    seed_audible_call(&app, 1, 1_000, 1000.0, 500).await;

    let response = app.get("/api/calls/export?format=mp3").await;

    assert_eq!(response.status().as_u16(), 400);
    assert!(response.text().await.expect("body").contains("format"));
}

/// **One at a time.** This is the only unauthenticated surface here that will
/// read a thousand objects and decode an hour of audio, on a box that is
/// usually also recording — so a second export is refused rather than queued.
#[tokio::test]
async fn only_one_export_runs_at_a_time() {
    let app = TestApp::spawn().await;
    let audio = wav(&vec![0.25; 8_000], 8_000);
    app.put_object("k/bulk.wav", &audio).await;
    for n in 0..600 {
        app.seed_call(
            NewCall {
                duration_ms: Some(1_000),
                ..NewCall::new(100, 1, 1_000 + n)
            },
            Some(radio_scout::blob::StoredAudio::written(
                "k/bulk.wav".to_string(),
                audio.len(),
            )),
        )
        .await;
    }

    // Started and deliberately not drained: the body is open, so the export
    // that is writing it still holds the slot.
    let mut running = app.get("/api/calls/export?format=zip").await;
    running
        .chunk()
        .await
        .expect("a chunk")
        .expect("body starts");

    let second = app.get("/api/calls/export?format=zip").await;

    assert_eq!(second.status().as_u16(), 429);
    assert_eq!(
        common::header_of(&second, "retry-after"),
        Some("30"),
        "and says when to come back"
    );
}

/// ...and the slot comes back, or an Instance would export exactly once per
/// boot.
#[tokio::test]
async fn a_finished_export_hands_the_slot_back() {
    let app = TestApp::spawn().await;
    seed_audible_call(&app, 1, 1_000, 1000.0, 500).await;

    let first = export(&app, "?format=zip").await;
    let second = export(&app, "?format=wav").await;

    assert!(!first.is_empty() && !second.is_empty());
}

/// A **Selection** scopes an export exactly as it scopes the DVR (#63) — which
/// is how "export what I am rewinding" costs this module no code at all.
#[tokio::test]
async fn a_selection_scopes_an_export() {
    let app = TestApp::spawn().await;
    seed_audible_call(&app, 1, 1_000, 1000.0, 500).await;
    seed_audible_call(&app, 2, 2_000, 1000.0, 500).await;

    let bytes = export(&app, "?format=zip&sel=0_100.1").await;

    let Some(files) = unzipped(&bytes) else {
        return;
    };
    let manifest: Value = serde_json::from_slice(&files[0].1).expect("manifest is JSON");
    let calls = manifest["calls"].as_array().expect("calls");
    assert_eq!(calls.len(), 1, "only the selected channel: {calls:?}");
    assert_eq!(calls[0]["talkgroupRef"], 1);
}

/// **A page of an export costs a fixed handful of round trips however many
/// Calls it holds** — the N+1 that is invisible from outside, because the
/// archive it produces is correct either way and only the cost behind it is
/// wrong (#86, #98's rule one surface along).
#[tokio::test]
async fn a_page_of_an_export_costs_the_same_however_many_calls_it_holds() {
    let app = TestApp::spawn().await;
    for n in 0..5 {
        seed_audible_call(&app, 1, 1_000 + n, 1000.0, 100).await;
    }
    let before = app.statements_issued();
    export(&app, "?format=zip&talkgroup=1").await;
    let five = app.statements_issued() - before;

    for n in 0..40 {
        seed_audible_call(&app, 1, 2_000 + n, 1000.0, 100).await;
    }
    let before = app.statements_issued();
    export(&app, "?format=zip&talkgroup=1").await;
    let forty_five = app.statements_issued() - before;

    assert_eq!(
        five, forty_five,
        "the export issued a statement per Call rather than per page"
    );
}

/// An object retention took **after** the manifest promised it still leaves the
/// entry there, empty — so the archive and its index describe the same set of
/// Calls, and a visibly empty file is a fact rather than a silent absence.
#[tokio::test]
async fn a_zip_entry_for_a_vanished_object_is_empty_rather_than_missing() {
    let app = TestApp::spawn().await;
    seed_audible_call(&app, 1, 1_000, 1000.0, 500).await;
    app.seed_call(
        NewCall {
            duration_ms: Some(500),
            ..NewCall::new(100, 1, 2_000)
        },
        common::audio_at("k/never-written.wav"),
    )
    .await;

    let bytes = export(&app, "?format=zip").await;

    let Some(files) = unzipped(&bytes) else {
        return;
    };
    assert_eq!(files.len(), 3, "a manifest and both Calls");
    assert!(files[2].1.is_empty(), "the vanished Call's file is empty");
    let manifest: Value = serde_json::from_slice(&files[0].1).expect("manifest is JSON");
    assert_eq!(manifest["calls"][1]["file"], files[2].0);
}

/// A URL that named no format gets the one that keeps everything.
#[tokio::test]
async fn an_export_that_named_no_format_is_a_zip() {
    let app = TestApp::spawn().await;
    seed_audible_call(&app, 1, 1_000, 1000.0, 500).await;

    let response = app.get("/api/calls/export").await;

    assert_eq!(
        common::header_of(&response, "content-type"),
        Some("application/zip")
    );
}

// ---------------------------------------------------------------------------
// When it goes wrong after the headers have gone
// ---------------------------------------------------------------------------

/// **The one stage here that cannot become a status code.** By the time the
/// Archive stops answering, the `200` and its headers have been sent — so the
/// download breaks, and the ERROR line is the whole record of why (ADR-0011
/// rule 4, in the only place it can be kept).
#[tokio::test]
async fn an_archive_that_stops_answering_mid_export_says_so_in_the_log() {
    let capture = LogCapture::start();
    let app = TestApp::spawn().await;
    seed_audible_call(&app, 1, 1_000, 1000.0, 500).await;
    // The measuring statement joins nothing, so it still answers; the page
    // behind it denormalizes through `systems` and does not.
    app.refuse_statements_on("systems");

    let response = app.get("/api/calls/export?format=zip").await;

    assert_eq!(
        response.status().as_u16(),
        200,
        "the headers had already gone"
    );
    assert!(
        response.bytes().await.is_err(),
        "and the body stops rather than pretending to be an archive"
    );
    let line = capture.only_line_containing("stage=read-export");
    assert!(line.contains(" ERROR "), "{line}");
}

/// **A store that has gone away must not write a line per Call.** An export may
/// hold a thousand of them and anybody with the URL can ask for one, so the
/// failure is counted and reported once when the export finishes — the
/// **Mining** sweep's rule, one surface along (ADR-0011 rule 8).
#[tokio::test]
async fn a_store_that_will_not_answer_is_reported_once_and_not_once_per_call() {
    let capture = LogCapture::start();
    let tmp = tempfile::tempdir().expect("tempdir");
    let (store, faults) = common::faulty_store(tmp.path());
    let app = TestApp::builder().store(store).spawn().await;
    for n in 0..5 {
        seed_audible_call(&app, 1, 1_000 + n, 1000.0, 500).await;
    }

    faults.fail_reads();
    let bytes = export(&app, "?format=zip").await;

    assert!(!bytes.is_empty(), "the export still completes");
    let lines = capture.lines_containing("export could not read");
    assert_eq!(
        lines.len(),
        1,
        "one line, whatever the range holds: {lines:?}"
    );
    assert!(
        lines[0].contains("calls=5"),
        "and it says how many: {}",
        lines[0]
    );
}

/// **A Call's name inside the zip is a function of that Call alone.**
///
/// It has to be, and the reason is the two passes: the manifest is written from
/// one read of the Archive and the audio from another, and the Archive is live.
/// A name derived from a row's *position* would slide by one for every Call
/// **Retention** pruned between them — leaving a manifest whose `file` named a
/// real file belonging to a different Call, which is worse than a missing one.
/// Asserted by pruning between two exports and finding the survivors named
/// exactly as they were.
#[tokio::test]
async fn a_calls_name_in_the_zip_does_not_depend_on_what_is_beside_it() {
    let app = TestApp::spawn().await;
    seed_audible_call(&app, 1, 1_000, 1000.0, 500).await;
    let gone = seed_audible_call(&app, 1, 2_000, 1000.0, 500).await;
    seed_audible_call(&app, 1, 3_000, 1000.0, 500).await;
    let before = export(&app, "?format=zip").await;
    let Some(before) = unzipped(&before) else {
        return;
    };

    radio_scout::db::repo::delete_calls(&app.db, &[gone])
        .await
        .expect("prune the middle Call");
    let after = export(&app, "?format=zip").await;
    let Some(after) = unzipped(&after) else {
        return;
    };

    assert_eq!(
        [&after[1].0, &after[2].0],
        [&before[1].0, &before[3].0],
        "the survivors kept their names"
    );
    let manifest: Value = serde_json::from_slice(&after[0].1).expect("manifest is JSON");
    assert_eq!(manifest["calls"][0]["file"], after[1].0);
    assert_eq!(manifest["calls"][1]["file"], after[2].0);
}

/// ...and that name says **when**, so a folder of extracted files sorts into the
/// order the incident happened whatever a filesystem thinks of the labels.
#[tokio::test]
async fn a_zip_sorts_into_the_order_the_incident_happened() {
    let app = TestApp::spawn().await;
    // 2023-11-14T22:13:20Z, and a second later.
    seed_audible_call(&app, 1, 1_700_000_001_000, 1000.0, 500).await;
    seed_audible_call(&app, 1, 1_700_000_000_000, 1000.0, 500).await;

    let bytes = export(&app, "?format=zip").await;

    let Some(files) = unzipped(&bytes) else {
        return;
    };
    let mut names: Vec<&String> = files.iter().skip(1).map(|(name, _)| name).collect();
    assert!(names[0].starts_with("20231114-221320-"), "{names:?}");
    let written = names.clone();
    names.sort();
    assert_eq!(names, written, "written in the order a listing sorts them");
}
