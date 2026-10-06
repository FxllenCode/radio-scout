//! The **Station stream** end to end (#74, spec US 60): a URL that plays a
//! **Selection** continuously, for players that cannot run the app.

mod common;

use std::time::Duration;

use common::{TestApp, Tuned};
use radio_scout::station::encode::{FRAME_BYTES, FRAME_SAMPLES, RATE};

/// Long enough for a loaded runner; a healthy stream answers in milliseconds.
const BUDGET: Duration = Duration::from_secs(10);

/// What a player needs to hear before it will start: a few seconds of audio.
const THREE_SECONDS: usize = 3 * RATE as usize / FRAME_SAMPLES * FRAME_BYTES;

/// **It plays at once, and it is MP3.** A player is handed seconds of audio
/// before it has asked twice, so it starts the moment it connects rather than
/// after buffering in real time — and what it is handed is a format every radio
/// player there is will play, said in the headers a radio player looks for.
#[tokio::test]
async fn a_stream_starts_at_once_and_is_mp3() {
    let app = TestApp::with_key("k").await;

    let response = app.tune("sel=1").await;

    assert_eq!(response.status(), 200);
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .map(|value| value.to_str().expect("ASCII").to_string())
    };
    assert_eq!(header("content-type").as_deref(), Some("audio/mpeg"));
    assert_eq!(header("cache-control").as_deref(), Some("no-store"));
    // A live stream has no byte to seek to, and a player that probes with a
    // `Range` request (Safari does) is told so rather than left to guess.
    assert_eq!(header("accept-ranges").as_deref(), Some("none"));
    assert!(
        header("icy-name").is_some(),
        "a player has something to call it"
    );
    assert!(header("icy-metaint").is_some(), "it asked, so it is told");

    let mut tuned = Tuned::new(response);
    let heard = tuned
        .listen_until(BUDGET, "three seconds of audio", |heard| {
            heard.audio.len() >= THREE_SECONDS
        })
        .await;
    let (samples, rate) = heard.decoded();
    assert_eq!(rate, RATE);
    assert!(
        samples.iter().all(|s| s.abs() < 1e-3),
        "nothing is on the air yet"
    );
}

/// A second of 1 kHz — a Call that is really a sound, so the stream can be
/// judged by what it plays.
fn tone_wav() -> Vec<u8> {
    common::tone_wav(1_000.0, 1_000)
}

/// How much of what was heard was loud — milliseconds of samples above -20 dBFS.
///
/// Nothing yet — too little arrived for a decoder to lock on to — is no loud
/// audio yet, because this is a condition read while a stream is still
/// arriving, not an assertion about what it carried.
fn loud_ms(heard: &common::Heard) -> usize {
    radio_scout::enhance::decode(&heard.audio).map_or(0, |(samples, rate)| {
        samples.iter().filter(|s| s.abs() > 0.1).count() * 1_000 / rate as usize
    })
}

/// What the app calls an auto-populated channel: its own number is its label.
const TALKGROUP_54241: &str = "System 11 - 54241";

/// **A Call joins the stream the moment it goes out**, announced as what the
/// app would call it and audible as what the Recorder sent — a player already
/// listening hears it without reconnecting, which is the whole of "live".
#[tokio::test]
async fn a_call_joins_the_stream_live() {
    let app = TestApp::with_key("k").await;
    let mut tuned = app.tune_in("sel=1").await;

    app.upload_ok(common::CallUpload::new().audio(&tone_wav()))
        .await;

    let heard = tuned
        .listen_until(BUDGET, "the Call, all of it", |heard| {
            heard.titles.iter().any(|title| title == TALKGROUP_54241) && loud_ms(heard) >= 800
        })
        .await;
    assert_eq!(heard.titles, vec![TALKGROUP_54241]);
}

/// **Only the Selection is on the air.** A Call on a channel the URL did not
/// select goes out on the live feed and never reaches the stream — and because
/// the stream plays in arrival order, it would have been announced first if it
/// had.
#[tokio::test]
async fn only_the_selection_is_on_the_air() {
    let app = TestApp::with_key("k").await;
    // System 11, Talkgroup 54241 on; nothing else.
    let mut tuned = app.tune_in("sel=0_11.54241").await;

    app.upload_ok(
        common::CallUpload::new()
            .talkgroup(54242)
            .at(100_000)
            .audio(&tone_wav()),
    )
    .await;
    app.upload_ok(common::CallUpload::new().at(200_000).audio(&tone_wav()))
        .await;

    let heard = tuned
        .listen_until(BUDGET, "the selected Call", |heard| {
            heard.titles.iter().any(|title| title == TALKGROUP_54241)
        })
        .await;
    assert_eq!(heard.titles, vec![TALKGROUP_54241]);
}

/// A link is one statement made by somebody else, so half of one is refused
/// whole — the share link's rule, and the DVR's — rather than played as a
/// scanner nobody asked for.
#[tokio::test]
async fn a_selection_that_does_not_read_is_refused() {
    let app = TestApp::with_key("k").await;

    let response = app.tune("sel=everything").await;

    assert_eq!(response.status(), 400);
    assert!(response.text().await.expect("a body").contains("sel"));
}

/// Blank is absent, here as on every surface that reads a Selection — a form
/// that left the field empty asked for no filter, not for a refusal.
#[tokio::test]
async fn a_blank_selection_is_no_filter() {
    let app = TestApp::with_key("k").await;

    assert_eq!(app.tune("sel=").await.status(), 200);
}

// ---------------------------------------------------------------------------
// Access codes (#68): a stream is gated exactly as a live socket is
// ---------------------------------------------------------------------------

const OPEN: i64 = 54241;
const GATED: i64 = 54999;
const TALKGROUP_54999: &str = "System 11 - 54999";

/// An Instance where Talkgroup 54999 on System 11 is **restricted** and 54241
/// beside it is not — marked through the admin surface, the way an Operator
/// does it.
async fn one_gated_channel(app: &TestApp) {
    app.login().await;
    app.upload_ok(common::CallUpload::new().talkgroup(OPEN).at(100_000))
        .await;
    app.upload_ok(common::CallUpload::new().talkgroup(GATED).at(200_000))
        .await;
    app.restrict_talkgroup(11, GATED, Some(true)).await;
}

/// **A gated Call is silent without its code, and plays with it.** The two
/// streams hear the same two Calls go out — gated first — and only the one
/// holding a grant announces both. Out of scope is not there, so the open
/// stream does not even say something was withheld.
#[tokio::test]
async fn a_gated_call_plays_only_to_a_stream_holding_its_code() {
    let app = TestApp::with_key("k").await;
    one_gated_channel(&app).await;
    let grant = app
        .create_access_code_as(serde_json::json!({
            "code": "FIRE-2026-OPS",
            "scope": { "all": true },
        }))
        .await;
    let mut open = app.tune_in("sel=1").await;
    let mut granted = app.tune_in(&format!("sel=1&grant={grant}")).await;

    // Minutes apart, so neither is a **Copy** of the other.
    let at = |minutes: i64| 1_000_000 + minutes * 60_000;
    app.upload_ok(
        common::CallUpload::new()
            .talkgroup(GATED)
            .at(at(1))
            .audio(&tone_wav()),
    )
    .await;
    app.upload_ok(
        common::CallUpload::new()
            .talkgroup(OPEN)
            .at(at(2))
            .audio(&tone_wav()),
    )
    .await;

    let until_open = |heard: &common::Heard| heard.titles.iter().any(|t| t == TALKGROUP_54241);
    let without = open.listen_until(BUDGET, "the open Call", until_open).await;
    let with = granted
        .listen_until(BUDGET, "the open Call", until_open)
        .await;
    assert_eq!(without.titles, vec![TALKGROUP_54241]);
    assert_eq!(with.titles, vec![TALKGROUP_54999, TALKGROUP_54241]);
}

/// **Switching a code off reaches the speakers already playing it.** A stream
/// is the listener nobody reconnects, so a scope read once, when it opened,
/// would be a code an Operator can never take back from a kitchen radio. The
/// stream is told on the same request that changed the code, falls back to
/// the open channels — what every read does with a grant that has stopped
/// working — and hands the code's connection back.
#[tokio::test]
async fn disabling_a_code_reaches_a_stream_already_playing_it() {
    let app = TestApp::with_key("k").await;
    one_gated_channel(&app).await;
    let grant = app
        .create_access_code_as(serde_json::json!({
            "code": "FIRE-2026-OPS",
            "scope": { "all": true },
        }))
        .await;
    let mut speaker = app.tune_in(&format!("sel=1&grant={grant}")).await;
    let (_, listing) = app.admin_get("/api/admin/codes").await;
    let code = &listing["results"][0];
    assert_eq!(code["connections"], 1, "the stream holds the code");

    let (status, _) = app
        .admin_patch(
            &format!("/api/admin/codes/{}", code["id"]),
            serde_json::json!({ "disabled": true }),
        )
        .await;
    assert_eq!(status, 200);
    let at = |minutes: i64| 1_000_000 + minutes * 60_000;
    app.upload_ok(
        common::CallUpload::new()
            .talkgroup(GATED)
            .at(at(1))
            .audio(&tone_wav()),
    )
    .await;
    app.upload_ok(
        common::CallUpload::new()
            .talkgroup(OPEN)
            .at(at(2))
            .audio(&tone_wav()),
    )
    .await;

    let heard = speaker
        .listen_until(BUDGET, "the open Call", |heard| {
            heard.titles.iter().any(|t| t == TALKGROUP_54241)
        })
        .await;
    assert_eq!(
        heard.titles,
        vec![TALKGROUP_54241],
        "the gated one is silent"
    );
    let (_, listing) = app.admin_get("/api/admin/codes").await;
    assert_eq!(
        listing["results"][0]["connections"], 0,
        "and the slot is back"
    );
}

/// **Restricting a channel reaches a stream that opened before anything was
/// gated** — the one that had been told it could hear everything, because then
/// it could.
#[tokio::test]
async fn restricting_a_channel_reaches_a_stream_that_opened_ungated() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    app.upload_ok(common::CallUpload::new().talkgroup(OPEN).at(100_000))
        .await;
    app.upload_ok(common::CallUpload::new().talkgroup(GATED).at(200_000))
        .await;
    let mut speaker = app.tune_in("sel=1").await;

    app.restrict_talkgroup(11, GATED, Some(true)).await;
    let at = |minutes: i64| 1_000_000 + minutes * 60_000;
    app.upload_ok(
        common::CallUpload::new()
            .talkgroup(GATED)
            .at(at(1))
            .audio(&tone_wav()),
    )
    .await;
    app.upload_ok(
        common::CallUpload::new()
            .talkgroup(OPEN)
            .at(at(2))
            .audio(&tone_wav()),
    )
    .await;

    let heard = speaker
        .listen_until(BUDGET, "the open Call", |heard| {
            heard.titles.iter().any(|t| t == TALKGROUP_54241)
        })
        .await;
    assert_eq!(heard.titles, vec![TALKGROUP_54241]);
}

/// **A roster that cannot be read changes nothing.** The stream keeps what it
/// last knew — `Access::arm`'s rule, because an error is not evidence that
/// anything changed — and says why at ERROR, where an Operator looks.
#[tokio::test]
async fn a_rescope_that_cannot_read_the_codes_keeps_the_last_answer() {
    let capture = common::logs::LogCapture::start();
    let app = TestApp::with_key("k").await;
    one_gated_channel(&app).await;
    let grant = app
        .create_access_code_as(serde_json::json!({
            "code": "FIRE-2026-OPS",
            "scope": { "all": true },
        }))
        .await;
    let mut speaker = app.tune_in(&format!("sel=1&grant={grant}")).await;

    app.refuse_statements_on("access_codes");
    // Anything that re-arms the gate tells every stream to look again.
    app.restrict_talkgroup(11, OPEN, Some(true)).await;
    capture.wait_for("stage=access").await;
    let at = |minutes: i64| 1_000_000 + minutes * 60_000;
    app.upload_ok(
        common::CallUpload::new()
            .talkgroup(GATED)
            .at(at(1))
            .audio(&tone_wav()),
    )
    .await;

    let heard = speaker
        .listen_until(BUDGET, "the gated Call", |heard| {
            heard.titles.iter().any(|t| t == TALKGROUP_54999)
        })
        .await;
    assert_eq!(heard.titles, vec![TALKGROUP_54999]);
}

/// **A code limited to one listener cannot be stretched across a house.** A
/// stream holds one of its code's connections, exactly as a live socket does —
/// the two count against the same limit — and gives it back when it goes.
#[tokio::test]
async fn a_stream_holds_one_of_its_codes_connections() {
    let app = TestApp::with_key("k").await;
    one_gated_channel(&app).await;
    let grant = app
        .create_access_code_as(serde_json::json!({
            "code": "FIRE-2026-OPS",
            "scope": { "all": true },
            "maxConnections": 1,
        }))
        .await;
    let speaker = app.tune_in(&format!("sel=1&grant={grant}")).await;

    let second = app.tune(&format!("sel=1&grant={grant}")).await;
    assert_eq!(second.status(), 429);
    assert!(second.text().await.expect("a body").contains("limit 1"));
    let (_phone, refused) = app.try_connect_ws_as(&grant).await;
    assert_eq!(refused["reason"], "access-connection-limit");

    // ...and turning the speaker off hands the connection back. Polled rather
    // than awaited: the slot is released when the *server* notices the player
    // went, which is not something a client's `drop` can wait for.
    drop(speaker);
    common::eventually("the connection came back when the stream went", || async {
        app.tune(&format!("sel=1&grant={grant}")).await.status() == 200
    })
    .await;
}

/// **A code that runs out takes its stream with it**, on the stream's own
/// clock — a speaker is the listener nobody closes a tab on, so a stream that
/// only looked when it opened would be a code that never expires.
#[tokio::test]
async fn a_code_that_runs_out_ends_its_stream() {
    let clock = radio_scout::Clock::frozen(5_000);
    let app = TestApp::builder().clock(clock.clone()).spawn().await;
    app.create_api_key("k").await;
    one_gated_channel(&app).await;
    let grant = app
        .create_access_code_as(serde_json::json!({
            "code": "FIRE-2026-OPS",
            "scope": { "all": true },
            "expiresAtMs": 6_000,
        }))
        .await;
    let mut speaker = app.tune_in(&format!("sel=1&grant={grant}")).await;
    speaker
        .listen_until(BUDGET, "the stream playing", |heard| {
            !heard.audio.is_empty()
        })
        .await;

    clock.advance(Duration::from_secs(2));

    assert!(
        speaker.ends_within(BUDGET).await,
        "the stream outlived its code"
    );
}

// ---------------------------------------------------------------------------
// The cap: `[station] max_streams`
// ---------------------------------------------------------------------------

/// **Past the cap a player is refused and told when to come back** — and the
/// slot a stream held is free again the moment its player goes.
#[tokio::test]
async fn the_cap_refuses_the_next_stream_and_frees_its_slot() {
    let app = TestApp::builder()
        .config(|config| config.station.max_streams = 1)
        .spawn()
        .await;
    let kitchen = app.tune_in("sel=1").await;

    let car = app.tune("sel=1").await;

    assert_eq!(car.status(), 429);
    assert_eq!(
        car.headers()
            .get("retry-after")
            .map(|v| v.to_str().expect("ASCII").to_string()),
        Some("60".to_string())
    );
    assert!(car.text().await.expect("a body").contains("limit 1"));

    drop(kitchen);
    common::eventually("the slot came back when the stream went", || async {
        app.tune("sel=1").await.status() == 200
    })
    .await;
}

/// **`0` is off, and off is not there**: the URL answers like a route that does
/// not exist, and the app stops offering one — a control offered and then
/// refused is a control that lies.
#[tokio::test]
async fn a_station_turned_off_is_neither_served_nor_offered() {
    let off = TestApp::builder()
        .config(|config| config.station.max_streams = 0)
        .spawn()
        .await;
    let on = TestApp::spawn().await;

    assert_eq!(off.tune("sel=1").await.status(), 404);
    assert_eq!(off.get_json("/api/catalog").await["station"], false);
    assert_eq!(on.get_json("/api/catalog").await["station"], true);
}

/// **A speaker is somebody listening.** The count an Operator watches (#62,
/// #70) includes every stream, so a kitchen radio left on is not invisible —
/// and it leaves the count when it goes, like a socket does.
#[tokio::test]
async fn a_stream_is_a_listener() {
    let app = TestApp::spawn().await;
    app.login().await;
    let speaker = app.tune_in("sel=1").await;

    assert_eq!(app.get_json("/api/admin/status").await["listeners"], 1);

    drop(speaker);
    common::eventually("the stream left the count when it went", || async {
        app.get_json("/api/admin/status").await["listeners"] == 0
    })
    .await;
}

/// **The bare URL, in a player that asks for nothing.** No `sel` is everything
/// this listener may hear — what an Operator typing the address into VLC means
/// by it — and a player that did not ask for metadata gets none: the body is
/// MP3 and nothing else, with no interval to strip.
#[tokio::test]
async fn a_bare_url_in_a_plain_player_plays_everything_as_pure_audio() {
    let app = TestApp::with_key("k").await;

    let response = app.get("/api/station.mp3").await;

    assert_eq!(response.status(), 200);
    assert!(response.headers().get("icy-metaint").is_none());
    let mut tuned = Tuned::new(response);
    app.upload_ok(common::CallUpload::new().audio(&tone_wav()))
        .await;
    let heard = tuned
        .listen_until(BUDGET, "the Call", |heard| loud_ms(heard) >= 800)
        .await;
    assert!(heard.titles.is_empty());
}

/// How a Call can turn out to have nothing to play.
#[derive(Debug, Clone, Copy)]
enum Unplayable {
    /// The store refused the read.
    Refused,
    /// The object is not there — pruned between going out and being read.
    Gone,
    /// What is there is not audio.
    NotAudio,
}

/// **A Call with nothing to play is passed over, and the station plays on.**
/// The next Call is the next thing on the air — which is also how a stream two
/// minutes behind on a short retention window behaves when the oldest of what
/// it was waiting on has already been pruned.
#[rstest::rstest]
#[case::the_store_refuses_the_read(
    Unplayable::Refused,
    "station stream could not read a Call's audio"
)]
#[case::the_object_is_gone(Unplayable::Gone, "station stream found nothing at a Call's object")]
#[case::it_is_not_audio(Unplayable::NotAudio, "station stream found no audio in a Call")]
#[tokio::test]
async fn a_call_with_nothing_to_play_is_passed_over(#[case] how: Unplayable, #[case] said: &str) {
    let capture = common::logs::LogCapture::start();
    let tmp = tempfile::tempdir().expect("tempdir");
    let (store, faults) = common::faulty_store(tmp.path());
    let app = TestApp::builder().store(store).spawn().await;
    app.create_api_key("k").await;
    let mut tuned = app.tune_in("sel=1").await;

    let first = common::CallUpload::new().talkgroup(54242).at(100_000);
    let first = match how {
        Unplayable::Refused => {
            faults.fail_reads();
            first.audio(&tone_wav())
        }
        Unplayable::Gone => {
            faults.hide_reads();
            first.audio(&tone_wav())
        }
        // The eleven bytes a default upload carries are deliberately not audio.
        Unplayable::NotAudio => first,
    };
    app.upload_ok(first).await;
    capture.wait_for(said).await;
    faults.allow_reads();
    app.upload_ok(common::CallUpload::new().at(200_000).audio(&tone_wav()))
        .await;

    let heard = tuned
        .listen_until(BUDGET, "the next Call", |heard| {
            heard.titles.iter().any(|title| title == TALKGROUP_54241)
        })
        .await;
    assert_eq!(heard.titles, vec![TALKGROUP_54241], "{how:?}");
}

/// **A stream does not hold an Instance open.** A response that never ends is
/// one a graceful stop would otherwise wait on for ever — so stopping ends
/// every stream first, and a speaker that was playing finds its stream over
/// rather than the Instance stuck.
#[tokio::test]
async fn stopping_the_instance_ends_its_streams() {
    let mut app = TestApp::spawn().await;
    let mut speaker = app.tune_in("sel=1").await;

    tokio::time::timeout(BUDGET, app.restart())
        .await
        .expect("the restart waited on a stream that never ends");

    assert!(speaker.ends_within(BUDGET).await);
}

/// **A Delayed Call is heard when it is released, and not before** (#73) —
/// the stream follows the live feed, so it is held back exactly as a socket's
/// Calls are: the undelayed Call behind it plays first, and the Delayed one
/// plays the moment its Delay has passed.
#[tokio::test]
async fn a_delayed_call_plays_when_it_is_released_and_not_before() {
    let app = TestApp::builder()
        .clock(radio_scout::Clock::frozen(1_790_000_000_000))
        .spawn()
        .await;
    app.create_api_key("k").await;
    app.login().await;
    let (status, body) = app
        .admin_post(
            "/api/admin/systems",
            serde_json::json!({ "ref": 11, "delayMinutes": 10 }),
        )
        .await;
    assert_eq!(status, 201, "{body}");
    let mut speaker = app.tune_in("sel=1").await;

    app.upload_ok(common::CallUpload::new().audio(&tone_wav()))
        .await;
    app.upload_ok(common::CallUpload::new().system(12).audio(&tone_wav()))
        .await;
    let undelayed = "System 12 - 54241";
    speaker
        .listen_until(BUDGET, "the undelayed Call", |heard| {
            heard.titles.iter().any(|t| t == undelayed)
        })
        .await;

    app.advance(Duration::from_secs(10 * 60)).await;

    let heard = speaker
        .listen_until(BUDGET, "the released Call", |heard| {
            heard.titles.iter().any(|t| t == TALKGROUP_54241)
        })
        .await;
    assert_eq!(heard.titles, vec![undelayed, TALKGROUP_54241]);
}
