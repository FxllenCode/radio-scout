//! **Dirwatch** (#72, spec US 14), driven the way it is used: an Operator makes a
//! watch in the admin surface, a Recorder drops files into a real folder, and
//! what is asserted is what lands in the Archive, what reaches a Listener, what
//! is left on disk and what the log says.
//!
//! The folder is the harness's temp-directory driver — every app's `drops/`,
//! which its `[dirwatch] roots` names — and files arrive the way a well-behaved
//! Recorder writes them, renamed into place. Most tests then ask the watch to
//! look (`scan_dirwatch`) and `settle()`, because a test cannot know when the
//! operating system will get round to telling anybody; the ones about the
//! operating system telling us wait for the Call on the live feed instead.
//!
//! The pure halves — masks, DSDPlus names, SDRTrunk tags, the routing rule —
//! are unit-tested in `src/dirwatch/`.

mod common;

use std::time::{Duration, SystemTime};

use chrono::TimeZone;
use common::logs::LogCapture;
use common::{CallUpload, SdrTrunkMp3, TestApp, frame_within, silence_ms, subscribe};
use radio_scout::db::entities::{call, call_frequency, call_unit, dirwatch};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
use serde_json::{Value, json};

/// Trunk Recorder's `.json` for one Call, as `create_call_json` writes it.
fn tr_meta(short_name: &str) -> String {
    format!(
        r#"{{
  "freq": 771093750,
  "start_time": 1669740338,
  "stop_time": 1669740344,
  "emergency": 1,
  "priority": 2,
  "encrypted": 0,
  "call_length": 5.76,
  "call_length_ms": 5760,
  "talkgroup": 54155,
  "talkgroup_tag": "EMS DISP",
  "talkgroup_description": "EMS Dispatch",
  "talkgroup_group_tag": "EMS Dispatch",
  "talkgroup_group": "EMS",
  "audio_type": "digital",
  "short_name": "{short_name}",
  "freqList": [{{"freq": 771093750, "time": 1669740338, "pos": 0.0, "len": 5.76,
                 "error_count": 3, "spike_count": 1}}],
  "srcList": [{{"src": 1610092, "time": 1669740339, "pos": 0.0, "emergency": 1,
                "signal_system": "P25", "tag": "EMS 1", "tag_ota": "MEDIC7"}}]
}}"#
    )
}

/// Where Trunk Recorder files a Call: `captureDir/<short_name>/YYYY/M/D/`.
const TR_STEM: &str = "fulton/2022/11/29/54155-1669740338_771093750";

/// Drop one Trunk Recorder Call — the `.json` first, as TR writes it.
fn drop_tr_call(app: &TestApp, stem: &str, meta: &str) {
    app.drop_file(&format!("{stem}.json"), meta.as_bytes());
    app.drop_file(&format!("{stem}.wav"), &silence_ms(5760));
}

/// The one Call, rendered whole — everything a Recorder's file could have
/// decided, and nothing the Instance invents (ids, object keys, timestamps of
/// storage). Two Calls that render the same were ingested the same.
async fn rendered(app: &TestApp) -> String {
    let call = app.the_call().await;
    let system = app.system_of(&call).await;
    let talkgroup = app.talkgroup_of(&call).await;
    let mut out = format!(
        "system={} {:?} talkgroup={} {:?} {:?} at={} freq={:?} mime={:?} name={:?} \
         size={:?} duration={:?} stop={:?} emergency={} encrypted={} priority={:?} type={:?}\n",
        system.r#ref,
        system.label,
        talkgroup.r#ref,
        talkgroup.label,
        talkgroup.name,
        call.call_at_ms,
        call.frequency,
        call.audio_mime,
        call.audio_name,
        call.audio_size,
        call.duration_ms,
        call.stop_at_ms,
        call.emergency,
        call.encrypted,
        call.priority,
        call.audio_type,
    );
    for f in call_frequency::Entity::find()
        .filter(call_frequency::Column::CallId.eq(call.id))
        .all(&app.db)
        .await
        .unwrap()
    {
        out += &format!(
            "freq {} {:?} {:?} {:?} {:?} {:?}\n",
            f.freq, f.pos_ms, f.len_ms, f.error_count, f.spike_count, f.at_ms
        );
    }
    for u in call_unit::Entity::find()
        .filter(call_unit::Column::CallId.eq(call.id))
        .all(&app.db)
        .await
        .unwrap()
    {
        out += &format!(
            "unit {} {:?} {:?} {:?} {} {:?} {:?}\n",
            u.unit_ref, u.label, u.offset_ms, u.tag_ota, u.emergency, u.signal_system, u.at_ms
        );
    }
    out
}

/// A watch's row as the screen lists it.
async fn listed(app: &TestApp, id: i64) -> Value {
    let (status, body) = app.admin_get("/api/admin/dirwatches").await;
    assert_eq!(status, 200, "{body}");
    body["results"]
        .as_array()
        .expect("a listing")
        .iter()
        .find(|row| row["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("watch {id} is not listed: {body}"))
}

/// Pretend a file was last written `ago` before now.
fn age(path: &std::path::Path, ago: Duration) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open")
        .set_modified(SystemTime::now() - ago)
        .expect("set mtime");
}

// ---------------------------------------------------------------------------
// Every format, through the whole pipeline
// ---------------------------------------------------------------------------

/// **The same pipeline, in the strongest sense**: a Trunk Recorder Call dropped
/// into a watched `captureDir` lands as exactly the Call the native upload of
/// the same `.json` and audio lands — the two read TR's document through one
/// parser, and nothing a Recorder said is lost by not uploading it.
#[tokio::test]
async fn a_dropped_trunk_recorder_call_lands_exactly_as_an_uploaded_one() {
    let dropped = TestApp::spawn().await;
    let id = dropped
        .add_dirwatch(json!({ "format": "trunk-recorder", "delayMs": 0 }))
        .await;
    drop_tr_call(&dropped, TR_STEM, &tr_meta("fulton"));
    dropped.scan_dirwatch(id).await;

    let uploaded = TestApp::with_key("k").await;
    let (status, body) = uploaded
        .upload_tr(CallUpload::tr(&tr_meta("fulton")).key("k").audio_named(
            &silence_ms(5760),
            "54155-1669740338_771093750.wav",
            "audio/wav",
        ))
        .await;
    assert_eq!(status, 200, "{body}");

    assert_eq!(dropped.count::<call::Entity>().await, 1);
    assert_eq!(rendered(&dropped).await, rendered(&uploaded).await);
    // ...and the audio is really there.
    let call = dropped.the_call().await;
    assert!(dropped.stored(&call.object_key).await);
}

/// SDRTrunk's MP3s carry their whole description in an ID3 tag — the Talkgroup
/// and its alias, the radio, the System by name, and when it was written — and
/// Mining names the radio from the same tag, exactly as it does on an upload.
#[tokio::test]
async fn a_dropped_sdrtrunk_recording_is_read_from_its_tag() {
    let app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({ "format": "sdrtrunk", "delayMs": 0 }))
        .await;
    let mp3 = SdrTrunkMp3::new()
        .frame("TIT2", "54241\"Fire Dispatch\"")
        .radio("1234567 Engine 1")
        .comment("Date:2026-08-11 09:00:10.000;System:Fulton;Site:Downtown;Frequency:851012500;");
    app.drop_file(
        "20260811_090010Fulton_TO_54241_FROM_1234567.mp3",
        &mp3.bytes(),
    );
    app.scan_dirwatch(id).await;

    let call = app.the_call().await;
    let system = app.system_of(&call).await;
    assert_eq!(system.label.as_deref(), Some("Fulton"));
    let talkgroup = app.talkgroup_of(&call).await;
    assert_eq!(talkgroup.r#ref, 54241);
    assert_eq!(talkgroup.label.as_deref(), Some("Fire Dispatch"));
    assert_eq!(
        app.units_of(call.id).await,
        vec![(1_234_567, Some("Engine 1".to_string()), None)],
        "the radio from the tag, named by Mining"
    );
    assert_eq!(call.frequency, Some(851_012_500));
    // Written at 09:00:10 on this Instance's wall clock, and the Call is as
    // long as its audio — so it began that much earlier.
    let written = chrono::Local
        .with_ymd_and_hms(2026, 8, 11, 9, 0, 10)
        .earliest()
        .expect("a local time")
        .timestamp_millis();
    assert_eq!(call.call_at_ms, written - SdrTrunkMp3::DURATION_MS);
    assert_eq!(call.audio_mime.as_deref(), Some("audio/mpeg"));
}

/// A System SDRTrunk names that this Instance already has is that System, not a
/// second one with the same name — the `short_name` rule, applied to a label.
#[tokio::test]
async fn an_sdrtrunk_system_name_finds_the_system_already_known_by_it() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (status, body) = app
        .admin_post(
            "/api/admin/systems",
            json!({ "ref": 41, "label": "Fulton" }),
        )
        .await;
    assert_eq!(status, 201, "{body}");
    let id = app
        .add_dirwatch(json!({ "format": "sdrtrunk", "delayMs": 0 }))
        .await;
    let mp3 = SdrTrunkMp3::new()
        .frame("TIT2", "54241")
        .comment("System:Fulton;");
    app.drop_file("a.mp3", &mp3.bytes());
    app.scan_dirwatch(id).await;

    let call = app.the_call().await;
    assert_eq!(app.system_of(&call).await.r#ref, 41);
}

/// DSDPlus says nothing anywhere but the path: the folder is the date, and the
/// name is the time, the network, the Talkgroup and the radio. A real name, from
/// rdio-scanner discussion #244.
#[tokio::test]
async fn a_dropped_dsdplus_recording_is_read_from_its_path() {
    let app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({ "format": "dsdplus", "delayMs": 0 }))
        .await;
    let audio = SdrTrunkMp3::new().anonymous().bytes();
    app.drop_file(
        "1R-Record/20220809/153120_001_DMR(BS)_1-899_DCC2_Slot1_GC_750[Ram_Muni]_52.mp3",
        &audio,
    );
    app.scan_dirwatch(id).await;

    let call = app.the_call().await;
    assert_eq!(app.system_of(&call).await.r#ref, 1);
    let talkgroup = app.talkgroup_of(&call).await;
    assert_eq!(talkgroup.r#ref, 750);
    assert_eq!(talkgroup.label.as_deref(), Some("Ram_Muni"));
    assert_eq!(app.units_of(call.id).await, vec![(52, None, None)]);
    let at = chrono::Local
        .with_ymd_and_hms(2022, 8, 9, 15, 31, 20)
        .earliest()
        .expect("a local time")
        .timestamp_millis();
    assert_eq!(call.call_at_ms, at);
}

/// rdio's documented mask, read the way rdio reads it — into the System the
/// watch names, since the mask names none.
#[tokio::test]
async fn a_masked_drop_is_read_from_its_name() {
    let app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({
            "format": "mask",
            "mask": "cymx_#TG_#DATE_#ZTIME_#HZ",
            "systemRef": 11,
            "delayMs": 0,
        }))
        .await;
    app.drop_file("cymx_1457_20201231_083439_119100000.wav", &silence_ms(500));
    app.scan_dirwatch(id).await;

    let call = app.the_call().await;
    assert_eq!(app.system_of(&call).await.r#ref, 11);
    assert_eq!(app.talkgroup_of(&call).await.r#ref, 1457);
    assert_eq!(call.frequency, Some(119_100_000));
    assert_eq!(
        call.call_at_ms,
        chrono::Utc
            .with_ymd_and_hms(2020, 12, 31, 8, 34, 39)
            .unwrap()
            .timestamp_millis()
    );
    // Counted as an upload's Admission is, though nothing rendered it — an
    // Instance fed only by dropped files must not report nothing arriving.
    let status = app.get_json("/api/admin/status").await;
    assert_eq!(status["ingest"]["stored"], 1, "{status}");
}

/// A file that does not say when — a mask with no time in it — is filed at
/// when it was **written**, not when it was noticed. rdio uses `time.Now()`, so
/// every Call a backfill picks up is filed at the moment the Instance came back.
#[tokio::test]
async fn a_file_that_does_not_say_when_is_filed_at_when_it_was_written() {
    let app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({
            "format": "mask",
            "mask": "#TG",
            "systemRef": 11,
            "delayMs": 0,
            "deleteAfter": true,
        }))
        .await;
    let path = app.drop_file("1457.wav", &silence_ms(500));
    let written = SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(written)
        .unwrap();
    app.scan_dirwatch(id).await;

    let written_ms = written
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    assert_eq!(app.the_call().await.call_at_ms, written_ms);
}

/// The watch's own System outranks the one the file names — an Operator who set
/// it is saying where these Calls go, #111's rule for a Trunk Recorder naming
/// its own System.
#[tokio::test]
async fn a_watchs_system_outranks_the_one_the_file_names() {
    let app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({ "format": "trunk-recorder", "systemRef": 411, "delayMs": 0 }))
        .await;
    drop_tr_call(&app, TR_STEM, &tr_meta("fulton"));
    app.scan_dirwatch(id).await;

    assert_eq!(app.system_of(&app.the_call().await).await.r#ref, 411);
}

// ---------------------------------------------------------------------------
// The operating system tells us
// ---------------------------------------------------------------------------

/// Nobody asks the watch to look: the operating system tells it a file arrived,
/// and a Listener hears the Call. The test waits on the live feed, which is
/// what "it arrived" means to anybody.
#[tokio::test]
async fn a_dropped_file_reaches_a_listener_without_anyone_asking() {
    let app = TestApp::spawn().await;
    app.add_dirwatch(json!({
        "format": "mask",
        "mask": "#TG",
        "systemRef": 11,
        "delayMs": 50,
    }))
    .await;
    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, r#"{"t":"sub","all":true}"#).await;

    app.drop_file("1457.wav", &silence_ms(500));

    let frame = frame_within(&mut ws, Duration::from_secs(20))
        .await
        .expect("the dropped Call on the live feed");
    assert_eq!(frame["t"], "call");
    assert_eq!(frame["call"]["talkgroupRef"], 1457, "{frame}");
}

/// A network share has no events to give, so a `poll` watch looks on a timer —
/// and a Listener still hears the Call nobody announced.
#[tokio::test]
async fn a_polling_watch_finds_a_file_no_event_announced() {
    let app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({
            "format": "mask", "mask": "#TG", "systemRef": 11, "delayMs": 0, "poll": true,
        }))
        .await;
    assert_eq!(listed(&app, id).await["health"]["status"], "watching");
    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, r#"{"t":"sub","all":true}"#).await;

    app.drop_file("1457.wav", &silence_ms(500));

    let frame = frame_within(&mut ws, Duration::from_secs(20))
        .await
        .expect("the Call, found by polling");
    assert_eq!(frame["call"]["talkgroupRef"], 1457, "{frame}");
}

/// Trunk Recorder files each day in a folder it creates at midnight, and writes
/// the first Call into it a moment later — before any watch on the new folder
/// can exist. That Call must not be lost.
#[tokio::test]
async fn a_call_written_into_a_folder_that_did_not_exist_is_not_lost() {
    let app = TestApp::spawn().await;
    app.add_dirwatch(json!({ "format": "trunk-recorder", "delayMs": 0 }))
        .await;
    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, r#"{"t":"sub","all":true}"#).await;

    // The whole day's folder, with its first Call already in it, appears at
    // once — the race at its worst.
    let staging = app.path().join("tomorrow");
    let day = staging.join("2022/11/30");
    std::fs::create_dir_all(&day).unwrap();
    std::fs::write(
        day.join("54155-1669826738_771093750.json"),
        tr_meta("fulton"),
    )
    .unwrap();
    std::fs::write(day.join("54155-1669826738_771093750.wav"), silence_ms(5760)).unwrap();
    std::fs::create_dir_all(app.drops().join("fulton")).unwrap();
    std::fs::rename(staging.join("2022"), app.drops().join("fulton/2022")).unwrap();

    let frame = frame_within(&mut ws, Duration::from_secs(20))
        .await
        .expect("the first Call of the day");
    assert_eq!(frame["call"]["talkgroupRef"], 54155, "{frame}");
}

// ---------------------------------------------------------------------------
// Files that are not Calls
// ---------------------------------------------------------------------------

/// An SDRTrunk MP3 naming a Talkgroup and nothing else.
fn sdrtrunk_tg_only() -> Vec<u8> {
    SdrTrunkMp3::new().frame("TIT2", "5").bytes()
}

/// **Refused with a logged reason, never a crash**: every file that cannot be a
/// Call leaves exactly one WARN naming why and which file, stays where it was
/// even under delete-after, and is counted on the watch — and asking the watch
/// to look again does not refuse the same unchanged file twice.
#[rstest::rstest]
#[case::tr_not_json(json!({ "format": "trunk-recorder" }), vec![("c.json", b"{not json".to_vec()), ("c.wav", b"RIFF".to_vec())], "invalid-meta")]
#[case::tr_no_talkgroup(json!({ "format": "trunk-recorder" }), vec![("c.json", br#"{"short_name":"x"}"#.to_vec()), ("c.wav", b"RIFF".to_vec())], "no-talkgroup")]
#[case::tr_empty_audio(json!({ "format": "trunk-recorder" }), vec![("c.json", br#"{"talkgroup":5}"#.to_vec()), ("c.wav", Vec::new())], "no-audio")]
#[case::mask_no_match(json!({ "format": "mask", "mask": "cymx_#TG", "systemRef": 1 }), vec![("other_5.wav", b"RIFF".to_vec())], "no-match")]
#[case::mask_empty(json!({ "format": "mask", "mask": "#TG", "systemRef": 1 }), vec![("5.wav", Vec::new())], "no-audio")]
#[case::sdrtrunk_untagged(json!({ "format": "sdrtrunk" }), vec![("x.mp3", b"not an mp3".to_vec())], "no-talkgroup")]
#[case::sdrtrunk_no_system(json!({ "format": "sdrtrunk" }), vec![("x.mp3", sdrtrunk_tg_only())], "no-system")]
#[case::dsdplus_too_short(json!({ "format": "dsdplus" }), vec![("20220809/153120.mp3", b"ID3".to_vec())], "no-talkgroup")]
#[case::dsdplus_no_system(json!({ "format": "dsdplus" }), vec![("20220809/153120_001_DCDM(D2)__DCC9_Slot2_GC_292_5.mp3", b"ID3".to_vec())], "no-system")]
#[tokio::test]
async fn a_file_that_cannot_be_a_call_is_refused_by_name_and_left_alone(
    #[case] mut watch: Value,
    #[case] files: Vec<(&str, Vec<u8>)>,
    #[case] reason: &str,
) {
    let logs = LogCapture::start();
    let app = TestApp::spawn().await;
    watch["delayMs"] = json!(0);
    watch["deleteAfter"] = json!(true);
    let id = app.add_dirwatch(watch).await;
    let dropped: Vec<_> = files
        .iter()
        .map(|(name, bytes)| app.drop_file(name, bytes))
        .collect();
    app.scan_dirwatch(id).await;

    assert_eq!(app.count::<call::Entity>().await, 0);
    let line = logs.only_line_containing("file refused");
    assert!(line.contains(" WARN "), "{line}");
    assert!(line.contains(&format!("reason={reason}")), "{line}");
    assert!(line.contains(&format!("watch_id={id}")), "{line}");
    for path in &dropped {
        assert!(path.exists(), "a refused file is never deleted: {path:?}");
    }
    let row = listed(&app, id).await;
    assert_eq!(row["health"]["refused"], 1, "{row}");
    assert!(
        row["health"]["lastRefusal"]
            .as_str()
            .is_some_and(|last| last.starts_with(reason)),
        "{row}"
    );

    app.scan_dirwatch(id).await;
    assert_eq!(
        logs.lines_containing("file refused").len(),
        1,
        "{}",
        logs.text()
    );
}

/// Trunk Recorder writes the `.json` before it renders the audio. rdio reads it
/// in that moment, finds no audio and drops the Call without a word. Here a
/// `.json` waits for its audio — and only one alone for longer than any render
/// takes is refused.
#[tokio::test]
async fn a_trunk_recorder_json_waits_for_its_audio_and_is_refused_only_when_it_never_comes() {
    let logs = LogCapture::start();
    let app = TestApp::spawn().await;
    // Deleting, so a `.json` left over from before is still owed whatever its
    // age, the way a Recorder's leftovers are.
    let id = app
        .add_dirwatch(json!({ "format": "trunk-recorder", "delayMs": 0, "deleteAfter": true }))
        .await;

    // Fresh: it waits, owing nothing, refused for nothing.
    app.drop_file(&format!("{TR_STEM}.json"), tr_meta("fulton").as_bytes());
    app.scan_dirwatch(id).await;
    assert_eq!(app.count::<call::Entity>().await, 0);
    assert!(
        logs.lines_containing("file refused").is_empty(),
        "{}",
        logs.text()
    );

    // Its audio arrives: the Call.
    app.drop_file(&format!("{TR_STEM}.wav"), &silence_ms(5760));
    app.scan_dirwatch(id).await;
    assert_eq!(app.count::<call::Entity>().await, 1);

    // A `.json` two minutes alone is never getting its audio.
    let orphan = app.drop_file(
        "fulton/2022/11/29/54155-1669740999_771093750.json",
        tr_meta("fulton").as_bytes(),
    );
    age(&orphan, Duration::from_secs(120));
    app.scan_dirwatch(id).await;
    assert!(
        logs.only_line_containing("file refused")
            .contains("reason=no-audio")
    );
}

/// A stray huge file in a watched folder is refused, never read into memory on
/// a Pi. Sparse, so the test costs no disk.
#[tokio::test]
async fn a_file_too_large_to_be_a_call_is_refused_without_being_read() {
    let logs = LogCapture::start();
    let app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({ "format": "mask", "mask": "#TG", "systemRef": 1, "delayMs": 0 }))
        .await;
    let staged = app.path().join("huge");
    std::fs::File::create(&staged)
        .unwrap()
        .set_len(radio_scout::dirwatch::MAX_FILE_BYTES + 1)
        .unwrap();
    std::fs::rename(&staged, app.drops().join("5.wav")).unwrap();
    app.scan_dirwatch(id).await;

    assert!(
        logs.only_line_containing("file refused")
            .contains("reason=too-large")
    );
}

/// Files a watch does not read are not its business: another extension, a
/// dotfile mid-copy, a symlink pointing out of the folder.
#[tokio::test]
async fn files_a_watch_does_not_read_are_left_entirely_alone() {
    let logs = LogCapture::start();
    let app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({
            "format": "mask", "mask": "#TG", "systemRef": 1, "delayMs": 0, "deleteAfter": true,
        }))
        .await;
    let other = app.drop_file("5.mp3", &silence_ms(100));
    let partial = app.drop_file(".5.wav.part", &silence_ms(100));
    let hidden = app.drop_file(".6.wav", &silence_ms(100));
    let outside = app.path().join("secret.wav");
    std::fs::write(&outside, silence_ms(100)).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, app.drops().join("7.wav")).unwrap();
    app.scan_dirwatch(id).await;

    assert_eq!(app.count::<call::Entity>().await, 0);
    assert!(
        logs.lines_containing("file refused").is_empty(),
        "{}",
        logs.text()
    );
    for path in [other, partial, hidden, outside] {
        assert!(path.exists(), "{path:?}");
    }
}

// ---------------------------------------------------------------------------
// Delete-after, and what is never deleted
// ---------------------------------------------------------------------------

/// Delete-after removes a file once it has an **answer** — stored, or refused
/// by the pipeline as a duplicate, which is an answer too — and a Trunk
/// Recorder Call goes whole: its `.json` and every rendering of its audio.
#[tokio::test]
async fn delete_after_removes_a_file_once_it_is_answered() {
    let app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({ "format": "trunk-recorder", "delayMs": 0, "deleteAfter": true }))
        .await;
    drop_tr_call(&app, TR_STEM, &tr_meta("fulton"));
    let m4a = app.drop_file(&format!("{TR_STEM}.m4a"), b"m4a bytes");
    app.scan_dirwatch(id).await;

    assert_eq!(app.count::<call::Entity>().await, 1);
    for extension in ["json", "wav"] {
        let path = app.drops().join(format!("{TR_STEM}.{extension}"));
        assert!(!path.exists(), "{path:?} should be gone");
    }
    assert!(!m4a.exists(), "the compressed copy goes with its Call");

    // The same Call again is a duplicate — answered, so removed.
    drop_tr_call(&app, TR_STEM, &tr_meta("fulton"));
    app.scan_dirwatch(id).await;
    assert_eq!(app.count::<call::Entity>().await, 1);
    assert!(!app.drops().join(format!("{TR_STEM}.json")).exists());
}

/// A file whose ingest **broke** is the one thing delete-after must never
/// touch: the store refused the audio, so the file on disk is the only copy
/// there is. rdio deletes after queueing, before the Call is stored. It is kept,
/// and taken once the store takes writes again.
#[tokio::test]
async fn a_file_the_store_refused_is_kept_and_taken_once_the_store_recovers() {
    let logs = LogCapture::start();
    let tmp = tempfile::tempdir().unwrap();
    let (store, faults) = common::faulty_store(tmp.path());
    let app = TestApp::builder().store(store).spawn().await;
    let id = app
        .add_dirwatch(json!({
            "format": "mask", "mask": "#TG", "systemRef": 1, "delayMs": 0, "deleteAfter": true,
        }))
        .await;
    faults.fail_puts();
    let path = app.drop_file("1457.wav", &silence_ms(500));
    app.scan_dirwatch(id).await;

    assert_eq!(app.count::<call::Entity>().await, 0);
    assert!(path.exists(), "the only copy there is");
    // Once at least — the operating system may have told the watch before the
    // scan did, and each attempt is its own line.
    let lines = logs.lines_containing("server error");
    assert!(!lines.is_empty(), "{}", logs.text());
    for line in &lines {
        assert!(line.contains("stage=store-audio"), "{line}");
        assert!(line.contains(&format!("watch_id={id}")), "{line}");
    }

    let status = app.get_json("/api/admin/status").await;
    assert!(
        status["errors"]["store-audio"].as_u64() >= Some(1),
        "a break nobody answered is still counted: {status}"
    );

    faults.allow_puts();
    app.scan_dirwatch(id).await;
    assert_eq!(app.count::<call::Entity>().await, 1);
    assert!(!path.exists());
}

// ---------------------------------------------------------------------------
// Restarts, and what arrived while nobody was watching
// ---------------------------------------------------------------------------

/// Turn a watch off behind the Worker's back — the row only, no re-arm — so the
/// next thing that starts it is a boot.
async fn set_disabled(app: &TestApp, id: i64, disabled: bool) {
    let mut row = dirwatch::Entity::find_by_id(id)
        .one(&app.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    row.disabled = Set(disabled);
    row.update(&app.db).await.unwrap();
}

/// **Polling, deliberately, in the tests about what a scan reads.** macOS's
/// FSEvents may hand a stream started a moment ago the changes from just before
/// it — a file renamed in and re-dated is one coalesced event — and an event is
/// a live arrival, which no watermark filters (`rsync -a` delivers new files
/// with old timestamps). Linux's inotify has no such replay. A poller takes its
/// baseline silently, so these hold on every operating system and test exactly
/// the claim they name.
const SCANNED: &str =
    r##"{ "format": "mask", "mask": "#TG", "systemRef": 1, "delayMs": 0, "poll": true }"##;

/// A mask watch reading `#TG` into System 1, polled — see [`SCANNED`].
fn scanned() -> Value {
    serde_json::from_str(SCANNED).expect("json")
}

/// **The watermark**: a watch that keeps its files backfills exactly the ones
/// that arrived while the Instance was down — and not one it already ingested,
/// which with no record of what it had seen would be every file in the folder,
/// every boot.
#[tokio::test]
async fn a_watch_that_keeps_its_files_backfills_only_what_arrived_while_it_was_down() {
    let logs = LogCapture::start();
    let mut app = TestApp::spawn().await;
    let id = app.add_dirwatch(scanned()).await;
    app.drop_file("100.wav", &silence_ms(500));
    app.scan_dirwatch(id).await;
    assert_eq!(app.count::<call::Entity>().await, 1);

    // The Instance goes down; a Call arrives; it comes back.
    set_disabled(&app, id, true).await;
    app.restart().await;
    app.settle().await;
    let during = app.drop_file("200.wav", &silence_ms(500));
    set_disabled(&app, id, false).await;
    app.restart().await;
    app.settle().await;

    let mut talkgroups = Vec::new();
    for call in app.calls().await {
        talkgroups.push(app.talkgroup_of(&call).await.r#ref);
    }
    talkgroups.sort();
    assert_eq!(talkgroups, vec![100, 200]);
    assert!(during.exists(), "kept: this watch keeps its files");
    assert!(
        logs.lines_containing("reason=duplicate").is_empty(),
        "nothing already ingested was read again:\n{}",
        logs.text()
    );
}

/// A watch that deletes as it goes needs no watermark: whatever is still in the
/// folder at boot is owed, whatever its age — rdio's own rule, and here a file
/// that broke last run is still there to be taken.
#[tokio::test]
async fn a_deleting_watch_backfills_everything_left_in_its_folder() {
    let mut app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({
            "format": "mask", "mask": "#TG", "systemRef": 1, "delayMs": 0, "deleteAfter": true,
        }))
        .await;
    set_disabled(&app, id, true).await;
    app.restart().await;
    let old = app.drop_file("300.wav", &silence_ms(500));
    age(&old, Duration::from_secs(86_400));
    set_disabled(&app, id, false).await;
    app.restart().await;
    app.settle().await;

    assert_eq!(app.count::<call::Entity>().await, 1);
    assert!(!old.exists());
}

/// Pointing a new watch that keeps its files at a folder of history does not
/// import the history — not even the last minute of it, which a rescan's
/// look-back would otherwise reach.
#[tokio::test]
async fn a_new_watch_does_not_import_the_history_already_in_its_folder() {
    let app = TestApp::spawn().await;
    let history = app.drop_file("100.wav", &silence_ms(500));
    age(&history, Duration::from_secs(60));
    let id = app.add_dirwatch(scanned()).await;
    app.scan_dirwatch(id).await;

    assert_eq!(app.count::<call::Entity>().await, 0);
}

/// After a restart the watch is watching again — not only backfilled once.
#[tokio::test]
async fn a_watch_is_still_watching_after_a_restart() {
    let mut app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({ "format": "mask", "mask": "#TG", "systemRef": 1, "delayMs": 0 }))
        .await;
    app.restart().await;
    app.settle().await;
    app.login().await;
    assert_eq!(listed(&app, id).await["health"]["status"], "watching");

    let mut ws = app.connect_ws().await;
    subscribe(&mut ws, r#"{"t":"sub","all":true}"#).await;
    app.drop_file("1457.wav", &silence_ms(500));
    assert!(
        frame_within(&mut ws, Duration::from_secs(20))
            .await
            .is_some(),
        "the Call, after the restart"
    );
}

/// The TOML is the authority on where a watch may be, checked every time a
/// watch starts — so a root removed under a watch stops it, and says so.
#[tokio::test]
async fn a_watch_whose_root_was_removed_stops_and_says_why() {
    let logs = LogCapture::start();
    let mut app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({ "format": "mask", "mask": "#TG", "systemRef": 1, "delayMs": 0 }))
        .await;
    let elsewhere = app.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    app.restart_with(move |config| {
        config.dirwatch.roots = vec![elsewhere.display().to_string().parse().unwrap()];
    })
    .await;
    app.settle().await;
    app.drop_file("1457.wav", &silence_ms(500));
    app.settle().await;
    app.login().await;

    assert_eq!(app.count::<call::Entity>().await, 0);
    assert_eq!(listed(&app, id).await["health"]["status"], "outside-roots");
    assert!(
        logs.only_line_containing("outside every root")
            .contains(&format!("watch_id={id}"))
    );
}

// ---------------------------------------------------------------------------
// The admin surface
// ---------------------------------------------------------------------------

/// The listing says where a watch may be, and what each one is doing.
#[tokio::test]
async fn the_listing_carries_the_roots_and_each_watchs_health() {
    let app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({
            "label": "Fulton TR", "format": "trunk-recorder", "delayMs": 0,
        }))
        .await;
    drop_tr_call(&app, TR_STEM, &tr_meta("fulton"));
    app.scan_dirwatch(id).await;

    let (_, body) = app.admin_get("/api/admin/dirwatches").await;
    let roots = body["roots"].as_array().expect("roots");
    assert_eq!(roots.len(), 1);
    assert!(roots[0].as_str().unwrap().ends_with("drops"), "{body}");
    let row = listed(&app, id).await;
    assert_eq!(row["label"], "Fulton TR");
    assert_eq!(row["format"], "trunk-recorder");
    assert_eq!(row["delayMs"], 0);
    assert_eq!(row["health"]["status"], "watching");
    assert_eq!(row["health"]["ingested"], 1);
    assert!(row["health"]["lastIngestMs"].is_i64(), "{row}");
}

/// Disabled is off — nothing is read — and enabled again it catches up on what
/// arrived meanwhile, because the watermark never moved past it.
#[tokio::test]
async fn a_disabled_watch_reads_nothing_and_catches_up_when_enabled() {
    let app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({ "format": "mask", "mask": "#TG", "systemRef": 1, "delayMs": 0 }))
        .await;
    let (status, row) = app
        .admin_patch(
            &format!("/api/admin/dirwatches/{id}"),
            json!({ "disabled": true }),
        )
        .await;
    assert_eq!(status, 200, "{row}");
    app.settle().await;
    assert_eq!(listed(&app, id).await["health"]["status"], "disabled");

    app.drop_file("1457.wav", &silence_ms(500));
    app.settle().await;
    assert_eq!(app.count::<call::Entity>().await, 0);

    app.admin_patch(
        &format!("/api/admin/dirwatches/{id}"),
        json!({ "disabled": false }),
    )
    .await;
    app.settle().await;
    assert_eq!(app.count::<call::Entity>().await, 1);
}

/// Deleting a watch stops it, leaves its folder alone, and leaves the Calls it
/// ingested in the Archive.
#[tokio::test]
async fn deleting_a_watch_stops_it_and_keeps_everything_it_ingested() {
    let app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({ "format": "mask", "mask": "#TG", "systemRef": 1, "delayMs": 0 }))
        .await;
    app.drop_file("1.wav", &silence_ms(500));
    app.scan_dirwatch(id).await;

    let (status, _) = app
        .admin_delete(&format!("/api/admin/dirwatches/{id}"))
        .await;
    assert_eq!(status, 204);
    app.settle().await;
    app.drop_file("2.wav", &silence_ms(500));
    app.settle().await;

    assert_eq!(app.count::<call::Entity>().await, 1);
    assert!(app.drops().join("2.wav").exists());
    let (status, body) = app
        .admin_delete(&format!("/api/admin/dirwatches/{id}"))
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (404, Some("dirwatch-not-found"))
    );
    let (status, _) = app
        .admin_post(&format!("/api/admin/dirwatches/{id}/scan"), json!({}))
        .await;
    assert_eq!(status, 404);
}

/// A watch that could never run is refused **on the form**, naming the one
/// thing to fix — rdio stores it and finds out at the first file, or, for a
/// bad mask, panics.
#[rstest::rstest]
#[case::no_directory(json!({ "directory": "", "format": "sdrtrunk" }), 400, "field-required")]
#[case::no_such_directory(json!({ "directory": "{drops}/missing", "format": "sdrtrunk" }), 400, "no-such-directory")]
#[case::a_file_is_not_a_folder(json!({ "directory": "{tmp}/t.db", "format": "sdrtrunk" }), 400, "no-such-directory")]
#[case::outside_the_roots(json!({ "directory": "{tmp}", "format": "sdrtrunk" }), 400, "outside-roots")]
#[case::climbing_out(json!({ "directory": "{drops}/..", "format": "sdrtrunk" }), 400, "outside-roots")]
#[case::unknown_format(json!({ "format": "default" }), 400, "unknown-dirwatch-format")]
#[case::not_audio(json!({ "format": "mask", "mask": "#SYS_#TG", "extension": "conf" }), 400, "not-audio")]
#[case::no_mask(json!({ "format": "mask" }), 400, "field-required")]
#[case::bad_mask(json!({ "format": "mask", "mask": "#TG_#TIEM", "systemRef": 1 }), 400, "unusable-mask")]
#[case::unroutable_talkgroup(json!({ "format": "mask", "mask": "#UNIT", "systemRef": 1 }), 400, "unroutable")]
#[case::unroutable_system(json!({ "format": "mask", "mask": "#TG" }), 400, "unroutable")]
#[case::negative_delay(json!({ "format": "sdrtrunk", "delayMs": -1 }), 400, "out-of-range")]
#[case::zero_system(json!({ "format": "sdrtrunk", "systemRef": 0 }), 400, "out-of-range")]
#[case::unknown_field(json!({ "format": "sdrtrunk", "type": "default" }), 422, "")]
#[tokio::test]
async fn a_watch_that_could_never_run_is_refused_on_the_form(
    #[case] body: Value,
    #[case] status: u16,
    #[case] error: &str,
) {
    let app = TestApp::spawn().await;
    app.login().await;
    let drops = app.drops().display().to_string();
    let tmp = app.path().display().to_string();
    let mut body: Value = serde_json::from_str(
        &body
            .to_string()
            .replace("{drops}", &drops)
            .replace("{tmp}", &tmp),
    )
    .unwrap();
    if body.get("directory").is_none() {
        body["directory"] = json!(drops);
    }

    let (got, refused) = app.admin_post("/api/admin/dirwatches", body).await;
    assert_eq!(got, status, "{refused}");
    if !error.is_empty() {
        assert_eq!(refused["error"], error, "{refused}");
    }
    assert_eq!(app.count::<dirwatch::Entity>().await, 0);
}

/// A symlink inside a root that points out of it is outside it — the folder is
/// resolved before it is judged.
#[cfg(unix)]
#[tokio::test]
async fn a_symlink_out_of_the_roots_is_outside_them() {
    let app = TestApp::spawn().await;
    app.login().await;
    let outside = app.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let link = app.drops().join("link");
    std::os::unix::fs::symlink(&outside, &link).unwrap();

    let (status, body) = app
        .admin_post(
            "/api/admin/dirwatches",
            json!({ "directory": link.display().to_string(), "format": "sdrtrunk" }),
        )
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (400, Some("outside-roots"))
    );
}

/// Two watches on one file would race each other's delete-after, so a folder
/// already watched — or one inside it, or one around it — is refused.
#[tokio::test]
async fn a_folder_another_watch_reads_is_refused() {
    let app = TestApp::spawn().await;
    std::fs::create_dir_all(app.drops().join("tr/fulton")).unwrap();
    app.add_dirwatch(json!({
        "directory": app.drops().join("tr").display().to_string(),
        "format": "trunk-recorder",
    }))
    .await;

    for directory in [
        app.drops().join("tr"),
        app.drops().join("tr/fulton"),
        app.drops(),
    ] {
        let (status, body) = app
            .admin_post(
                "/api/admin/dirwatches",
                json!({ "directory": directory.display().to_string(), "format": "sdrtrunk" }),
            )
            .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (409, Some("dirwatch-overlaps")),
            "{directory:?}"
        );
    }
}

/// An edit is held to the whole row it produces, not just the fields it names —
/// so no sequence of edits reaches a watch a create would refuse.
#[tokio::test]
async fn an_edit_is_checked_as_the_whole_watch_it_makes() {
    let app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({ "format": "mask", "mask": "#SYS_#TG" }))
        .await;

    // Taking the System out of the mask leaves nothing saying which System.
    let (status, body) = app
        .admin_patch(
            &format!("/api/admin/dirwatches/{id}"),
            json!({ "mask": "#TG" }),
        )
        .await;
    assert_eq!((status, body["error"].as_str()), (400, Some("unroutable")));
    // ...unless the watch says it at the same time.
    let (status, body) = app
        .admin_patch(
            &format!("/api/admin/dirwatches/{id}"),
            json!({ "mask": "#TG", "systemRef": 9 }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["systemRef"], 9);
    // An unknown id is not there.
    let (status, _) = app
        .admin_patch("/api/admin/dirwatches/999", json!({ "disabled": true }))
        .await;
    assert_eq!(status, 404);
}

/// With no roots, Dirwatch is off: the listing says so with an empty list, and
/// a create is refused naming the setting that turns it on.
#[tokio::test]
async fn with_no_roots_dirwatch_is_off_and_says_how_to_turn_it_on() {
    let app = TestApp::builder()
        .config(|config| config.dirwatch.roots.clear())
        .spawn()
        .await;
    app.login().await;

    let (_, listing) = app.admin_get("/api/admin/dirwatches").await;
    assert_eq!(listing["roots"], json!([]));
    let (status, body) = app
        .admin_post(
            "/api/admin/dirwatches",
            json!({ "directory": app.drops().display().to_string(), "format": "sdrtrunk" }),
        )
        .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "dirwatch-unavailable");
    assert!(
        body["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("[dirwatch] roots")),
        "{body}"
    );
}

// ---------------------------------------------------------------------------
// When the Instance itself fails
// ---------------------------------------------------------------------------

/// A System that cannot be looked up — the database refusing — breaks the
/// ingest of that one file, which is kept and logged as a server error against
/// the watch, whichever format named the System by name.
#[rstest::rstest]
#[case::trunk_recorder("trunk-recorder")]
#[case::sdrtrunk("sdrtrunk")]
#[tokio::test]
async fn a_system_that_cannot_be_looked_up_breaks_only_that_file(#[case] format: &str) {
    let logs = LogCapture::start();
    let app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({ "format": format, "delayMs": 0, "deleteAfter": true }))
        .await;
    app.refuse_statements_on("systems");
    let kept = match format {
        "trunk-recorder" => {
            drop_tr_call(&app, TR_STEM, &tr_meta("fulton"));
            app.drops().join(format!("{TR_STEM}.json"))
        }
        _ => app.drop_file(
            "a.mp3",
            &SdrTrunkMp3::new()
                .frame("TIT2", "54241")
                .comment("System:Fulton;")
                .bytes(),
        ),
    };
    app.scan_dirwatch(id).await;

    assert!(kept.exists());
    let lines = logs.lines_containing("server error");
    assert!(!lines.is_empty(), "{}", logs.text());
    assert!(
        lines
            .iter()
            .all(|line| line.contains("stage=resolve-system")),
        "{lines:#?}"
    );
}

/// The watermark that cannot be written down is a server error, not a lost
/// Call: the Call is stored, and the worst case is one already-ingested file
/// read again at the next boot — answered as the duplicate it is.
#[tokio::test]
async fn a_watermark_that_cannot_be_written_costs_a_line_and_not_a_call() {
    let logs = LogCapture::start();
    let app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({ "format": "mask", "mask": "#TG", "systemRef": 1, "delayMs": 0 }))
        .await;
    app.refuse_updates_to("dirwatches");
    app.drop_file("1457.wav", &silence_ms(500));
    app.scan_dirwatch(id).await;

    assert_eq!(app.count::<call::Entity>().await, 1);
    let line = logs.only_line_containing("server error");
    assert!(line.contains("stage=curate"), "{line}");
}

/// A roster that cannot be read leaves the watches it had running as they
/// were, and says so — the re-arm after a delete here.
#[tokio::test]
async fn a_roster_that_cannot_be_read_is_a_server_error() {
    let logs = LogCapture::start();
    let app = TestApp::spawn().await;
    let id = app.add_dirwatch(json!({ "format": "sdrtrunk" })).await;
    app.refuse_reads_of("dirwatches");
    let (status, _) = app
        .admin_delete(&format!("/api/admin/dirwatches/{id}"))
        .await;
    assert_eq!(status, 204);
    app.settle().await;

    let line = logs.only_line_containing("server error");
    assert!(line.contains("stage=curate"), "{line}");
    let status = app.get_json("/api/admin/status").await;
    assert_eq!(status["errors"]["curate"], 1, "{status}");
}

/// A file delete-after cannot remove — a folder the service user may read but
/// not write — is ingested once, said so, and not read again.
#[cfg(unix)]
#[tokio::test]
async fn a_file_that_cannot_be_deleted_is_ingested_once_and_said_so() {
    use std::os::unix::fs::PermissionsExt;
    let logs = LogCapture::start();
    let app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({
            "format": "mask", "mask": "#TG", "systemRef": 1, "delayMs": 0, "deleteAfter": true,
        }))
        .await;
    let file = app.drop_file("locked/1457.wav", &silence_ms(500));
    let folder = file.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
    app.scan_dirwatch(id).await;
    app.scan_dirwatch(id).await;
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(app.count::<call::Entity>().await, 1);
    if file.exists() {
        assert!(
            logs.only_line_containing("could not delete an ingested file")
                .contains(&format!("watch_id={id}"))
        );
    }
}

// ---------------------------------------------------------------------------
// Editing one watch
// ---------------------------------------------------------------------------

/// An edit to one watch restarts that one and leaves the others running as
/// they were — so a file one of them refused is not refused all over again
/// because somebody changed its neighbour, or renamed it.
#[tokio::test]
async fn an_edit_to_one_watch_leaves_the_others_running_untouched() {
    let logs = LogCapture::start();
    let app = TestApp::spawn().await;
    std::fs::create_dir_all(app.drops().join("a")).unwrap();
    std::fs::create_dir_all(app.drops().join("b")).unwrap();
    let a = app
        .add_dirwatch(json!({
            "directory": app.drops().join("a").display().to_string(),
            "format": "mask", "mask": "x_#TG", "systemRef": 1, "delayMs": 0,
        }))
        .await;
    let b = app
        .add_dirwatch(json!({
            "directory": app.drops().join("b").display().to_string(),
            "format": "sdrtrunk",
        }))
        .await;
    app.drop_file("a/nomatch.wav", &silence_ms(100));
    app.scan_dirwatch(a).await;
    assert_eq!(logs.lines_containing("file refused").len(), 1);

    // An edit the Worker has to act on, to the *other* watch...
    let (status, body) = app
        .admin_patch(
            &format!("/api/admin/dirwatches/{b}"),
            json!({ "delayMs": 100 }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    // ...and a rename of this one, which means nothing to the Worker at all.
    let (status, body) = app
        .admin_patch(
            &format!("/api/admin/dirwatches/{a}"),
            json!({ "label": "Masked" }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["label"], "Masked");
    app.settle().await;

    assert_eq!(
        logs.lines_containing("file refused").len(),
        1,
        "{}",
        logs.text()
    );
}

/// Moving a watch to another folder is a new watch as far as history goes: the
/// new folder's existing files are not imported.
#[tokio::test]
async fn moving_a_watch_does_not_import_the_new_folders_history() {
    let app = TestApp::spawn().await;
    std::fs::create_dir_all(app.drops().join("old")).unwrap();
    let history = app.drop_file("new/100.wav", &silence_ms(500));
    let id = app
        .add_dirwatch(json!({
            "directory": app.drops().join("old").display().to_string(),
            "format": "mask", "mask": "#TG", "systemRef": 1, "delayMs": 0, "poll": true,
        }))
        .await;
    // Older than the move, but newer than the watch — only a watermark reset
    // keeps it out.
    std::fs::File::options()
        .write(true)
        .open(&history)
        .unwrap()
        .set_modified(SystemTime::now())
        .unwrap();

    let (status, body) = app
        .admin_patch(
            &format!("/api/admin/dirwatches/{id}"),
            json!({ "directory": app.drops().join("new").display().to_string() }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    app.settle().await;
    app.scan_dirwatch(id).await;

    assert_eq!(app.count::<call::Entity>().await, 0);
}

/// A Trunk Recorder Call that names no frequency takes the watch's.
#[tokio::test]
async fn a_watch_frequency_fills_a_trunk_recorder_call_that_names_none() {
    let app = TestApp::spawn().await;
    let id = app
        .add_dirwatch(json!({ "format": "trunk-recorder", "frequency": 155_000_000, "delayMs": 0 }))
        .await;
    drop_tr_call(
        &app,
        TR_STEM,
        r#"{"talkgroup": 54155, "short_name": "fulton"}"#,
    );
    app.scan_dirwatch(id).await;

    assert_eq!(app.the_call().await.frequency, Some(155_000_000));
}
