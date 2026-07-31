//! The first-party Trunk Recorder plugin (#44, spec US 6).
//!
//! `radio-scout-upload.sh` (#43) gets a stock recorder onto the native endpoint
//! with one line of config and no build. This is the other way in, for installs
//! that would rather load a plugin: it posts from inside Trunk Recorder's own
//! process — no `fork`/`execvp` per Call, no shell — and, because a plugin *may*
//! fail a Call where the script deliberately may not, it gets Trunk Recorder's
//! retry with backoff for free.
//!
//! **What these tests drive is the plugin's own upload core**, compiled from the
//! same `plugins/trunk-recorder/radio_scout_upload.cc` the plugin links, and
//! posting over real HTTP to a real app. The core is deliberately free of every
//! Trunk Recorder header so this is possible at all: it takes strings and file
//! paths, and the ~40-line shim in `radio_scout_uploader.cc` is the only part
//! that knows what a `Call_Data_t` is. That shim is proven by the CI job that
//! compiles it against the recorder's source tree — there is no seam here that
//! could reach it, and pretending otherwise with a hand-written `Call_Data_t`
//! would prove only that the fake matches the fake.
//!
//! The alternative — a committed `.multipart` fixture claiming to be what the
//! plugin emits — is what `tests/uploadscript.rs` rejected for the script, for
//! the same reason: it stays green when the plugin changes underneath it.
//!
//! These need a C++ compiler and libcurl's **headers** — not the `curl` binary
//! `tests/uploadscript.rs` wants, which is a different thing a runner can have
//! without. CI installs them (`ci.yml`), and these refuse to skip there for
//! exactly that reason; anywhere else they skip, saying so.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::OnceLock;

use radio_scout::db::entities::{call_frequency, call_unit};
use rstest::rstest;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tokio::process::Command;

mod common;
use common::TestApp;

/// Where the plugin's source lives, in the repo that ships it.
fn plugin_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/trunk-recorder")
}

/// Stop the test, saying why, when this machine cannot build what it drives.
///
/// Reported to whoever is reading the run — there is no subscriber installed
/// here, and a test that silently passes for want of a toolchain is
/// indistinguishable from one that proved something.
///
/// **In CI this is a failure, not a skip.** The pipeline installs libcurl's
/// headers precisely so these run; if that step is ever dropped, the whole file
/// would go quietly green and the plugin would ship untested. A skip is for a
/// contributor's machine, not for the thing that gates a merge.
#[allow(clippy::print_stderr)]
fn skip(reason: &str) {
    assert!(
        std::env::var_os("CI").is_none(),
        "the Trunk Recorder plugin tests cannot skip in CI ({reason}); \
         the pipeline installs libcurl's headers so that they run"
    );
    eprintln!("skipping Trunk Recorder plugin test: {reason}");
}

/// Stop this test unless the plugin can be built here.
///
/// A macro rather than a helper because it has to `return` from the test it is
/// written in.
macro_rules! needs_toolchain {
    () => {
        if harness().is_none() {
            return skip("no C++ compiler with libcurl headers");
        }
    };
}

/// Compile the plugin's upload core together with the harness `main` that
/// drives it, once for the whole test binary.
///
/// Built into `CARGO_TARGET_TMPDIR` rather than a temp dir per test: the
/// compile is the expensive part, and every test in this file wants the same
/// binary.
///
/// `None` means **this machine cannot build it**, which is the only thing worth
/// skipping over. A machine that can and then doesn't **panics** — a plugin that
/// no longer compiles is the loudest thing this file can find, and reporting it
/// as a skip would leave every test below passing for the wrong reason.
fn harness() -> Option<&'static Path> {
    static BUILT: OnceLock<Option<PathBuf>> = OnceLock::new();
    BUILT.get_or_init(build_harness).as_deref()
}

/// Does this machine have the C++ toolchain and libcurl headers the plugin is
/// built with? Probed by compiling the smallest program that needs both, so a
/// failure below is our source rather than the machine.
///
/// Named per process, like the staged binary below and for the same reason:
/// nextest gives every test its own, they all probe at once, and two of them
/// sharing one file would have one see a truncated program and report this
/// machine incapable — a silent skip of the whole file on a machine that is
/// perfectly capable.
fn toolchain_available(out_dir: &Path) -> bool {
    let probe = out_dir.join(format!("probe-{}.cc", std::process::id()));
    if std::fs::write(
        &probe,
        "#include <curl/curl.h>\nint main() { return curl_global_init(0) == 0 ? 0 : 1; }\n",
    )
    .is_err()
    {
        return false;
    }
    std::process::Command::new(compiler())
        .args(["-std=c++17", "-o"])
        .arg(out_dir.join(format!("probe-{}", std::process::id())))
        .arg(&probe)
        .arg("-lcurl")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// The compiler Trunk Recorder itself would be built with, overridable the way
/// every other build here overrides it.
fn compiler() -> String {
    std::env::var("CXX").unwrap_or_else(|_| "c++".to_string())
}

/// The sources the harness is built from: the plugin's own upload core, and the
/// `main` that drives it.
fn sources() -> Vec<PathBuf> {
    [
        "radio_scout_upload.h",
        "radio_scout_upload.cc",
        "harness.cc",
    ]
    .iter()
    .map(|name| plugin_dir().join(name))
    .collect()
}

/// Is `binary` newer than every source it was built from?
fn up_to_date(binary: &Path) -> bool {
    let Ok(built) = binary.metadata().and_then(|m| m.modified()) else {
        return false;
    };
    sources().iter().all(|source| {
        source
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|edited| edited <= built)
    })
}

fn build_harness() -> Option<PathBuf> {
    let out_dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("trplugin");
    std::fs::create_dir_all(&out_dir).expect("create the build directory");

    let binary = out_dir.join("harness");
    // nextest runs every test in a process of its own, so without this the
    // plugin would be recompiled once per test in this file rather than once.
    if up_to_date(&binary) {
        return Some(binary);
    }
    if !toolchain_available(&out_dir) {
        return None;
    }

    // Built under a name only this process uses and moved into place, because
    // those same per-test processes race to build it on the first run and a
    // half-written binary is one another test would try to execute.
    let staged = out_dir.join(format!("harness-{}", std::process::id()));
    let output = std::process::Command::new(compiler())
        .args(["-std=c++17", "-Wall", "-Wextra", "-Werror", "-o"])
        .arg(&staged)
        .arg(plugin_dir().join("radio_scout_upload.cc"))
        .arg(plugin_dir().join("harness.cc"))
        .arg("-lcurl")
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", compiler()));

    assert!(
        output.status.success(),
        "the plugin's upload core did not compile:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::rename(&staged, &binary).expect("move the built harness into place");
    Some(binary)
}

/// The Talkgroup every Call here is on. It rides twice — inside the metadata,
/// and beside it as the value `call_end` reads straight off `Call_Data_t` to
/// filter on — so it is written once.
const TALKGROUP: i64 = 54155;

/// Trunk Recorder's own call metadata, as `create_call_json` writes it
/// (`trunk-recorder/call_concluder/call_concluder.cc:785`) — the object the
/// plugin sends **verbatim** out of `Call_Data_t::call_json`, which is why
/// there is no field mapping for a test to disagree with.
fn tr_meta(short_name: &str) -> String {
    format!(
        r#"{{
  "call_num": 4171,
  "freq": 771093750,
  "start_time": 1669740338,
  "stop_time": 1669740344,
  "emergency": 1,
  "priority": 2,
  "encrypted": 0,
  "call_length": 5.76,
  "call_length_ms": 5760,
  "talkgroup": {TALKGROUP},
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

/// The files Trunk Recorder has on disk when it calls a plugin's `call_end`:
/// the rendered audio, the compressed copy `compressWav` may have made, and the
/// metadata JSON beside them.
struct CallFiles {
    wav: PathBuf,
    m4a: PathBuf,
    json: PathBuf,
    _dir: tempfile::TempDir,
}

impl CallFiles {
    fn write(meta: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let stem = dir.path().join("54155-1669740338_771093750");
        let wav = stem.with_extension("wav");
        let json = stem.with_extension("json");
        std::fs::write(&wav, common::silence_ms(5760)).expect("write the audio");
        std::fs::write(&json, meta).expect("write the metadata");
        CallFiles {
            wav,
            m4a: stem.with_extension("m4a"),
            json,
            _dir: dir,
        }
    }

    /// The compressed copy `compressWav` leaves beside the rendered WAV.
    fn compress(self) -> Self {
        std::fs::write(&self.m4a, COMPRESSED).expect("write the compressed copy");
        self
    }
}

/// Stand-in for what `ffmpeg` hands back — the bytes matter only in that they
/// are not the WAV's, so a test can tell which file was sent.
const COMPRESSED: &[u8] = b"\0\0\0\x1cftypM4A \0\0\x02\0M4A mp42isom";

/// Drive the plugin's upload core the way `call_end` drives it: the address and
/// key from the plugin's configuration, the two audio paths and the flag that
/// chooses between them, and the metadata JSON Trunk Recorder just built.
///
/// **Asynchronous, and it has to be.** `TestApp` serves on a task spawned into
/// this test's own current-thread runtime, so blocking the thread on a child
/// process means the server never gets polled and the upload waits for its own
/// timeout instead.
async fn run_upload(app: &TestApp, key: &str, files: &CallFiles, extra: &[&str]) -> Output {
    run_upload_at(&app.url(""), key, files, extra).await
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The same, against an address of our choosing rather than the app's — for the
/// failures that need a server which is missing, or worse, present.
async fn run_upload_at(server: &str, key: &str, files: &CallFiles, extra: &[&str]) -> Output {
    let mut command = Command::new(harness().expect("the harness is built"));
    command
        .arg("--server")
        .arg(server)
        .arg("--key")
        .arg(key)
        .arg("--meta")
        .arg(&files.json)
        .arg("--wav")
        .arg(&files.wav)
        .arg("--m4a")
        .arg(&files.m4a)
        .arg("--talkgroup")
        .arg(TALKGROUP.to_string())
        .args(extra);
    command
        .output()
        .await
        .expect("run the plugin's upload core")
}

/// A key distinctive enough that finding it anywhere in the plugin's output is
/// unambiguous (ADR-0011 rule 2 — a secret never reaches a log line, and the
/// recorder's log is a log line).
const KEY_THAT_MUST_NOT_LEAK: &str = "sekrit-ingest-key-9f3a";

/// The whole point of the ticket: a recorder running the plugin sends the
/// native contract, and everything Trunk Recorder knows arrives — the fields
/// the rdio dialect its own `rdioscanner_uploader` speaks has no room for.
#[tokio::test]
async fn a_call_uploaded_by_the_plugin_lands_with_every_enriched_field() {
    needs_toolchain!();
    let app = TestApp::with_key("tr-plugin-key").await;
    let files = CallFiles::write(&tr_meta("fulton"));

    let output = run_upload(&app, "tr-plugin-key", &files, &[]).await;

    assert!(
        output.status.success(),
        "call_end must report success: {}",
        stdout_of(&output)
    );

    let call = app.the_call().await;
    assert_eq!(app.talkgroup_of(&call).await.r#ref, 54155);
    assert!(call.emergency, "the emergency bit Trunk Recorder wrote");
    assert_eq!(call.priority, Some(2));
    assert_eq!(call.audio_type.as_deref(), Some("digital"));
    assert_eq!(call.duration_ms, Some(5760), "the recorder's own figure");
    assert_eq!(call.stop_at_ms, Some(1669740344000));
    assert_eq!(app.system_of(&call).await.label.as_deref(), Some("fulton"));
}

/// The signal detail the rdio dialect has no room for at all — which is half of
/// what the plugin exists for, and the half a payload could quietly lose while
/// every scalar above still arrived.
#[tokio::test]
async fn the_per_frequency_and_per_source_detail_arrives_too() {
    needs_toolchain!();
    let app = TestApp::with_key("k").await;
    let files = CallFiles::write(&tr_meta("fulton"));

    let output = run_upload(&app, "k", &files, &[]).await;
    assert!(output.status.success(), "{}", stdout_of(&output));
    let call = app.the_call().await;

    let freqs = call_frequency::Entity::find()
        .filter(call_frequency::Column::CallId.eq(call.id))
        .all(&app.db)
        .await
        .unwrap();
    assert_eq!(freqs.len(), 1);
    assert_eq!(freqs[0].freq, 771093750);
    assert_eq!(freqs[0].error_count, Some(3), "a dongle's decode health");
    assert_eq!(freqs[0].spike_count, Some(1));

    let units = call_unit::Entity::find()
        .filter(call_unit::Column::CallId.eq(call.id))
        .all(&app.db)
        .await
        .unwrap();
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].unit_ref, 1610092);
    assert_eq!(
        units[0].label.as_deref(),
        Some("EMS 1"),
        "the configured tag"
    );
    assert_eq!(
        units[0].tag_ota.as_deref(),
        Some("MEDIC7"),
        "and the one the radio put over the air, kept apart from it"
    );
    assert!(units[0].emergency, "this radio keyed the emergency");
    assert_eq!(units[0].signal_system.as_deref(), Some("P25"));
}

/// With `compressWav` off — Trunk Recorder's default — the rendered WAV is the
/// only audio there is, and it goes named and typed as what it is.
#[tokio::test]
async fn the_rendered_wav_is_sent_when_there_is_no_compressed_copy() {
    needs_toolchain!();
    let app = TestApp::with_key("k").await;
    let files = CallFiles::write(&tr_meta("fulton"));

    let output = run_upload(&app, "k", &files, &[]).await;
    assert!(output.status.success(), "{}", stdout_of(&output));

    let call = app.the_call().await;
    assert_eq!(
        call.audio_name.as_deref(),
        Some("54155-1669740338_771093750.wav")
    );
    assert_eq!(call.audio_mime.as_deref(), Some("audio/wav"));
    assert_eq!(
        app.object_bytes(&call.object_key).await,
        Some(std::fs::read(&files.wav).unwrap()),
        "the bytes on disk are the bytes stored"
    );
}

/// ...and with `compressWav` on, the compressed copy goes instead.
///
/// Which file matters more than it looks: 32 kbps AAC against 8 kHz 16-bit PCM
/// is most of a home uplink's headroom, and a recorder on DSL is the normal
/// case. Trunk Recorder's own rdio uploader picks the same way — on the
/// `compress_wav` flag, not on which file happens to exist
/// (`rdioscanner_uploader.cc:334`).
#[tokio::test]
async fn the_compressed_copy_is_sent_when_compress_wav_made_one() {
    needs_toolchain!();
    let app = TestApp::with_key("k").await;
    let files = CallFiles::write(&tr_meta("fulton")).compress();

    let output = run_upload(&app, "k", &files, &["--compress"]).await;
    assert!(output.status.success(), "{}", stdout_of(&output));

    let call = app.the_call().await;
    assert_eq!(
        call.audio_name.as_deref(),
        Some("54155-1669740338_771093750.m4a")
    );
    assert_eq!(call.audio_mime.as_deref(), Some("audio/mp4"));
    assert_eq!(
        app.object_bytes(&call.object_key).await,
        Some(COMPRESSED.to_vec()),
        "the compressed copy, not the WAV beside it"
    );
}

// ---- What `call_end` returns, and what Trunk Recorder does with it ---------
//
// Returning non-zero from `call_end` puts *this* plugin — and only this plugin
// — on `call_info.plugin_retry_list` (`plugin_manager.cc:194-198`); the retry
// runs `call_end` for the failed plugins alone (`:203-205`), keeps the call's
// files, and backs off `2^attempt * 60s + jitter` for `MAX_RETRY` attempts
// (`call_concluder.cc:1294`, `:30`). So a plugin may fail a Call safely, which
// the shipped `uploadScript` may not — a non-zero exit from *that* hook skips
// `plugman_call_end` entirely and takes every other uploader on the recorder
// down with it (#43, `call_concluder.cc:981-987`). This is the plugin's one
// real advantage over the script, and these tests are the contract for it.

/// A server that accepts the connection and then says nothing at all — a
/// half-open NAT, a wedged reverse proxy, a host that went away mid-transfer.
///
/// Without a bound this parks a call-data worker forever: Trunk Recorder polls
/// those futures from its main loop and waits on them at shutdown
/// (`shutdown_call_data_workers`), so one black hole would leak a thread per
/// Call and then hang the recorder on the way out. Giving up and asking for a
/// retry is the whole of the fix.
#[tokio::test]
async fn a_server_that_never_answers_gives_up_rather_than_wedging_the_recorder() {
    needs_toolchain!();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the black hole");
    let server = format!("http://{}", listener.local_addr().unwrap());
    // Accept and hold: never read, never answer, never close.
    let held = tokio::spawn(async move {
        let mut open = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            open.push(socket);
        }
    });
    let files = CallFiles::write(&tr_meta("fulton"));

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        run_upload_at(&server, KEY_THAT_MUST_NOT_LEAK, &files, &["--timeout", "1"]),
    )
    .await
    .expect("the plugin gave up on its own");
    held.abort();

    assert_eq!(
        output.status.code(),
        Some(1),
        "call_end asks Trunk Recorder to retry: {}",
        stdout_of(&output)
    );
    assert!(
        !stdout_of(&output).contains(KEY_THAT_MUST_NOT_LEAK),
        "the API key must not ride into the recorder's log: {}",
        stdout_of(&output)
    );
}

/// Nobody is listening — Radio-Scout restarting, a flaky home network. The
/// commonest failure there is, and the one worth retrying: two minutes later it
/// will very likely work.
#[tokio::test]
async fn an_unreachable_server_asks_trunk_recorder_to_retry() {
    needs_toolchain!();
    // Bound and dropped: nothing is listening on it now, and nothing else on
    // the machine has been handed it either.
    let dead = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().unwrap()
    };
    let files = CallFiles::write(&tr_meta("fulton"));

    let output = run_upload_at(
        &format!("http://{dead}"),
        KEY_THAT_MUST_NOT_LEAK,
        &files,
        &[],
    )
    .await;

    assert_eq!(output.status.code(), Some(1), "{}", stdout_of(&output));
    assert!(
        stdout_of(&output).starts_with("failed:"),
        "the operator is told nobody answered: {}",
        stdout_of(&output)
    );
    assert!(!stdout_of(&output).contains(KEY_THAT_MUST_NOT_LEAK));
}

/// A key the server refuses. Retried too — and it will fail again, which is the
/// point: Trunk Recorder gives up after `MAX_RETRY` and logs "Upload failed
/// after N attempts" (`call_concluder.cc:885`), which is how an operator finds
/// out on the first Call rather than a week later. The cost of being wrong here
/// is two extra requests, borne by this plugin alone.
#[tokio::test]
async fn a_refused_key_asks_for_a_retry_and_repeats_what_the_server_said() {
    needs_toolchain!();
    let app = TestApp::with_key("the-real-key").await;
    let files = CallFiles::write(&tr_meta("fulton"));

    let output = run_upload_at(&app.url(""), KEY_THAT_MUST_NOT_LEAK, &files, &[]).await;

    assert_eq!(output.status.code(), Some(1), "{}", stdout_of(&output));
    assert!(
        stdout_of(&output).contains("Invalid API key for system"),
        "Radio-Scout's own words reach the recorder's log: {}",
        stdout_of(&output)
    );
    assert!(!stdout_of(&output).contains(KEY_THAT_MUST_NOT_LEAK));
    assert_eq!(app.calls().await.len(), 0);
}

/// The Call arrived already. rdio-scanner answers a duplicate `200` on purpose
/// — a recorder given an error retries forever and this will never succeed —
/// and the plugin has to honour that by reporting the Call *done*, not by
/// asking for the retry that will be refused identically twice more.
#[tokio::test]
async fn a_duplicate_is_finished_business_rather_than_something_to_retry() {
    needs_toolchain!();
    let app = TestApp::with_key("k").await;
    let files = CallFiles::write(&tr_meta("fulton"));

    let first = run_upload(&app, "k", &files, &[]).await;
    assert!(first.status.success(), "{}", stdout_of(&first));
    let again = run_upload(&app, "k", &files, &[]).await;

    assert!(
        again.status.success(),
        "call_end reports success so the Call is not retried: {}",
        stdout_of(&again)
    );
    assert!(
        stdout_of(&again).contains("duplicate call rejected"),
        "{}",
        stdout_of(&again)
    );
    assert_eq!(app.calls().await.len(), 1);
}

// ---- Talkgroup filtering ---------------------------------------------------

/// A busy system does not have to send a Raspberry Pi everything it hears, and
/// the `rdioscanner_uploader` plugin an operator is migrating from has this
/// (`rdioscanner_uploader.cc:603`), so a configuration that loses it on the way
/// across is a downgrade.
///
/// The patterns are globs, anchored whole — `541*` is a range of talkgroups and
/// `5.155` is one talkgroup with a dot in it, which no talkgroup has. Getting
/// that backwards would silently widen every filter an operator wrote.
///
/// A filtered Call exits **0**: it is not a failure, and asking Trunk Recorder
/// to retry something we will decline identically twice more would waste its
/// backoff on a decision that is already final.
#[rstest]
#[case::no_filters_at_all(&[], &[], true)]
#[case::allowed_exactly(&["--allow", "54155"], &[], true)]
#[case::allowed_by_glob(&["--allow", "541*"], &[], true)]
#[case::a_question_mark_stands_for_one_digit(&["--allow", "541?5"], &[], true)]
#[case::not_in_the_allow_list(&["--allow", "54241", "--allow", "545*"], &[], false)]
#[case::denied_outright(&[], &["--deny", "54155"], false)]
#[case::denied_by_glob(&[], &["--deny", "541*"], false)]
#[case::deny_beats_allow(&["--allow", "541*"], &["--deny", "54155"], false)]
#[case::a_dot_is_a_dot_and_not_any_character(&[], &["--deny", "5.155"], true)]
#[case::a_prefix_is_not_a_match_on_its_own(&["--allow", "541"], &[], false)]
#[tokio::test]
async fn the_talkgroup_filter_decides_what_leaves_the_recorder(
    #[case] allow: &[&str],
    #[case] deny: &[&str],
    #[case] uploaded: bool,
) {
    needs_toolchain!();
    let app = TestApp::with_key("k").await;
    let files = CallFiles::write(&tr_meta("fulton"));
    let filters: Vec<&str> = allow.iter().chain(deny.iter()).copied().collect();

    let output = run_upload(&app, "k", &files, &filters).await;

    assert!(
        output.status.success(),
        "a filtered Call is a decision, not a failure: {}",
        stdout_of(&output)
    );
    assert_eq!(
        app.calls().await.len(),
        usize::from(uploaded),
        "allow={allow:?} deny={deny:?}: {}",
        stdout_of(&output)
    );
}

/// A Call on an encrypted talkgroup, which Trunk Recorder records as the
/// vocoder's noise when `monitorEncrypted` is on.
///
/// The `rdioscanner_uploader` plugin drops these on the floor — `if
/// (call_info.encrypted) return 0;` (`rdioscanner_uploader.cc:171-173`) —
/// because the rdio dialect has no field to say what they are, so an operator
/// loses every record that the encrypted channel was even busy. We forward
/// them: the native contract carries `encrypted`, and Radio-Scout keeps the
/// flagged row and writes no audio object at all (#42, spec US 9).
///
/// Reachable on Trunk Recorder 5.0.2 with `monitorEncrypted` on. On master
/// `conclude_call` returns before the plugin manager for an encrypted Call
/// (`call_concluder.cc:1244-1252`), so no plugin — and no `uploadScript` —
/// hears about one; that is the recorder's decision to make, and the plugin's
/// job is only to be right when it is asked.
#[tokio::test]
async fn an_encrypted_call_is_forwarded_and_becomes_a_row_with_no_audio() {
    needs_toolchain!();
    let app = TestApp::with_key("k").await;
    let meta = tr_meta("fulton").replace(r#""encrypted": 0"#, r#""encrypted": 1"#);
    let files = CallFiles::write(&meta);

    let output = run_upload(&app, "k", &files, &[]).await;
    assert!(output.status.success(), "{}", stdout_of(&output));

    let call = app.the_call().await;
    assert!(call.encrypted, "the flag the rdio dialect cannot carry");
    assert_eq!(call.duration_ms, Some(5760), "the recorder still knows");
    assert_eq!(call.object_key, "", "no object was written");
    assert!(app.object_keys().await.is_empty());
}

/// Drive the core with nothing but the arguments given — for the cases about
/// what happens *before* there is anything to upload to.
async fn run_harness(args: &[&str]) -> Output {
    Command::new(harness().expect("the harness is built"))
        .args(args)
        .output()
        .await
        .expect("run the plugin's upload core")
}

/// A plugin block with no `server`, or none with an `apiKey`.
///
/// This has to be caught before the request, because Trunk Recorder **discards
/// what `parse_config` returns** (`plugin_manager.cc:56`) — a plugin that
/// reports its configuration unusable is loaded and handed every Call anyway.
/// Left to libcurl, each of those becomes a failed transfer and a `1`, and the
/// recorder then spends its whole retry budget — two attempts, two and four
/// minutes apart — on every Call for as long as the typo lasts, keeping each
/// one's files on disk meanwhile. The reference plugin guards the same thing
/// the same way (`rdioscanner_uploader.cc:190-192`).
#[rstest]
#[case::no_server(&["--key", "a-key"])]
#[case::no_key(&["--server", "http://127.0.0.1:1"])]
#[case::neither(&[])]
#[tokio::test]
async fn an_unconfigured_plugin_asks_for_no_retry(#[case] args: &[&str]) {
    needs_toolchain!();

    let output = run_harness(args).await;

    assert!(
        output.status.success(),
        "no retry could fix a missing setting: {}",
        stdout_of(&output)
    );
    assert!(
        stdout_of(&output).contains("not configured"),
        "the operator is told which half is missing: {}",
        stdout_of(&output)
    );
}
