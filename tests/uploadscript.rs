//! The shipped `uploadScript` for Trunk Recorder (#43, spec US 6).
//!
//! `radio-scout-upload.sh` is the documented way a **stock** Trunk Recorder
//! sends Radio-Scout everything it knows: TR's own hook hands a script the call
//! it just finished, and the script posts that pair to the native endpoint,
//! where #42's parser reads the emergency/encrypted/priority/duration fields the
//! rdio dialect cannot carry.
//!
//! **These tests run the real script**, with the three arguments TR really
//! appends (`call_concluder.cc`'s `run_upload_script_argv`), against a real app
//! over real HTTP. That is `tests/packaging.rs`'s precedent — it runs
//! `install.sh` for a release rather than asserting about it — and it is the
//! only version that cannot drift: a committed `.multipart` fixture claiming to
//! be what the script emits stays green when the script changes underneath it.
//!
//! The script needs `curl`. Every CI runner has one; a dev box without it sees
//! these tests skip, saying so.

use std::path::{Path, PathBuf};
use std::process::Output;

use tokio::process::Command;

mod common;
use common::TestApp;

/// The script under test, from the repo it lives in.
fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("radio-scout-upload.sh")
}

/// Is there a `curl` for the script to use?
fn curl_available() -> bool {
    std::process::Command::new("curl")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Report a skipped test to whoever is reading the run — there is no subscriber
/// installed here, and a test that silently passes for want of a tool is
/// indistinguishable from one that proved something.
#[allow(clippy::print_stderr)]
fn skip(reason: &str) {
    eprintln!("skipping uploadScript test: {reason}");
}

/// Trunk Recorder's own call metadata, as `create_call_json` writes it
/// (`trunk-recorder/call_concluder/call_concluder.cc`). The full field set —
/// this is the payload #42's parser exists to read.
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

/// The pair Trunk Recorder leaves on disk for one finished Call: the rendered
/// audio and the metadata beside it, named the way TR names them.
struct CallFiles {
    wav: PathBuf,
    json: PathBuf,
    /// TR appends this third path whether or not `compressWav` produced it, so
    /// the script has to cope with a name that points at nothing.
    m4a: PathBuf,
    _dir: tempfile::TempDir,
}

impl CallFiles {
    fn write(short_name: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let stem = dir.path().join("54155-1669740338_771093750");
        let wav = stem.with_extension("wav");
        let json = stem.with_extension("json");
        std::fs::write(&wav, common::silence_ms(5760)).expect("write the audio");
        std::fs::write(&json, tr_meta(short_name)).expect("write the metadata");
        CallFiles {
            wav,
            json,
            m4a: stem.with_extension("m4a"),
            _dir: dir,
        }
    }
}

/// Run the script exactly as Trunk Recorder runs it: whatever the operator put
/// in `uploadScript`, then the wav, the json and the m4a
/// (`run_upload_script_argv`, `call_concluder.cc:768-770`).
///
/// `env` is the environment TR passes down — it `fork`s and `execvp`s without
/// touching it (`run_process_wait`, `:139-150`), so a variable set for the
/// recorder's service reaches the script. `env_clear` first, so a variable that
/// happens to be set on the machine running the suite cannot make a test pass.
/// **Asynchronous, and it has to be.** `TestApp` serves on a task spawned into
/// this test's own current-thread runtime, so blocking the thread on a child
/// process means the server never gets polled — the script's `curl` sits there
/// until its own timeout and the Call never arrives. The first version of this
/// test spent thirty seconds proving exactly that.
async fn run_hook(args: &[&str], env: &[(&str, &str)], files: &CallFiles) -> Output {
    let mut command = Command::new(script());
    command.env_clear();
    // Everything `sh` and `curl` need to exist at all; nothing about us.
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    command
        .args(args)
        .arg(&files.wav)
        .arg(&files.json)
        .arg(&files.m4a)
        .output()
        .await
        .expect("run the upload script")
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The whole point of the ticket: a stock Trunk Recorder, one line of config,
/// and everything it knows arrives.
#[tokio::test]
async fn a_trunk_recorder_call_lands_with_every_enriched_field() {
    if !curl_available() {
        return skip("curl is not installed");
    }
    let app = TestApp::with_key("tr-key").await;
    let files = CallFiles::write("butco");

    let output = run_hook(
        &["--server", &app.url("")],
        &[("RADIO_SCOUT_API_KEY", "tr-key")],
        &files,
    )
    .await;

    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(
        stderr_of(&output),
        "",
        "a successful upload says nothing at all: Trunk Recorder's log is busy \
         enough without one line per call"
    );
    let call = app.the_call().await;
    // The fields the rdio dialect cannot carry, which is why this path exists.
    assert!(call.emergency, "the emergency bit TR wrote");
    assert_eq!(call.priority, Some(2));
    assert_eq!(call.audio_type.as_deref(), Some("digital"));
    assert_eq!(call.duration_ms, Some(5760), "the recorder's own figure");
    assert_eq!(call.stop_at_ms, Some(1669740344000));
    assert_eq!(
        call.call_at_ms, 1669740338000,
        "start_time, not the moment it was uploaded"
    );
}

/// The signal detail the rdio dialect has no room for at all: per-frequency
/// decode health and the alias each radio put over the air.
#[tokio::test]
async fn the_per_frequency_and_per_source_detail_arrives_too() {
    if !curl_available() {
        return skip("curl is not installed");
    }
    let app = TestApp::with_key("tr-key").await;
    let files = CallFiles::write("butco");

    run_hook(
        &["--server", &app.url("")],
        &[("RADIO_SCOUT_API_KEY", "tr-key")],
        &files,
    )
    .await;

    let id = app.the_call().await.id;
    let detail = app.get_json(&format!("/api/call/{id}")).await;
    assert_eq!(detail["frequencies"][0]["errorCount"], 3);
    assert_eq!(detail["frequencies"][0]["spikeCount"], 1);
    assert_eq!(detail["units"][0]["ref"], 1610092);
    assert_eq!(detail["units"][0]["label"], "EMS 1");
    assert_eq!(
        detail["units"][0]["tagOta"], "MEDIC7",
        "the name the radio broadcast about itself"
    );
    assert_eq!(detail["units"][0]["signalSystem"], "P25");
}

/// Trunk Recorder appends the `.m4a` path whether or not `compressWav` made the
/// file, so its *existence* is the only thing saying which copy to send.
///
/// Sending the compressed one matters: 32 kbps AAC against 8 kHz 16-bit PCM is
/// most of a home uplink's headroom, and a recorder uploading over DSL is the
/// normal case rather than the exotic one.
#[tokio::test]
async fn the_compressed_copy_is_preferred_when_trunk_recorder_made_one() {
    if !curl_available() {
        return skip("curl is not installed");
    }
    let app = TestApp::with_key("tr-key").await;
    let files = CallFiles::write("butco");
    // Not real AAC — nothing here decodes it. What is asserted is *which file's
    // bytes* left the machine, which is the decision under test.
    std::fs::write(&files.m4a, b"compressed-copy").expect("write the m4a");

    run_hook(
        &["--server", &app.url("")],
        &[("RADIO_SCOUT_API_KEY", "tr-key")],
        &files,
    )
    .await;

    let call = app.the_call().await;
    assert_eq!(
        app.object_bytes(&call.object_key).await.as_deref(),
        Some(b"compressed-copy".as_slice()),
        "the .m4a was sent, not the .wav"
    );
    assert_eq!(call.audio_mime.as_deref(), Some("audio/mp4"));
}

/// ...and with `compressWav` off there is no such file, so the `.wav` goes —
/// which is the default Trunk Recorder configuration.
#[tokio::test]
async fn the_wav_is_sent_when_there_is_no_compressed_copy() {
    if !curl_available() {
        return skip("curl is not installed");
    }
    let app = TestApp::with_key("tr-key").await;
    let files = CallFiles::write("butco");
    assert!(!files.m4a.exists(), "the fixture leaves this one absent");

    run_hook(
        &["--server", &app.url("")],
        &[("RADIO_SCOUT_API_KEY", "tr-key")],
        &files,
    )
    .await;

    let call = app.the_call().await;
    assert_eq!(call.audio_mime.as_deref(), Some("audio/x-wav"));
    assert_eq!(
        call.duration_ms,
        Some(5760),
        "TR's own figure either way, but the audio really is that long too"
    );
}

// ---------------------------------------------------------------------------
// Failure modes (#43 AC4, decided against the ticket's wording — see below)
// ---------------------------------------------------------------------------
//
// The ticket asked for every failure to exit non-zero "so TR logs them". Half
// of that is wrong, and dangerously so. A non-zero exit does not merely get
// logged: `upload_call_worker` does `remove_call_files(); status = FAILED;
// return` (`call_concluder.cc:981-987`), which **skips `plugman_call_end`
// entirely** — so every other plugin on that recorder, including an
// `rdioscanner_uploader` feeding a production rdio-scanner, never runs for that
// Call. And there is no retry: the RETRY path with its backoff is reachable
// only from plugin failures (`:996-998`), never from this hook.
//
// So the split is: non-zero for what says the *install* is wrong, which no
// retry could fix and which an operator must see on the first Call; exit 0 with
// a loud stderr line for anything the network or the server did. Trunk Recorder
// deletes the call files after this returns either way, so a non-zero exit on a
// server outage buys a log line at the price of somebody else's feed.

/// A server that is not there. The commonest failure by far — Radio-Scout
/// restarting, a flaky home network — and the one that must cost nothing but
/// its own Call.
#[tokio::test]
async fn an_unreachable_server_is_loud_and_still_lets_the_other_plugins_run() {
    if !curl_available() {
        return skip("curl is not installed");
    }
    let files = CallFiles::write("butco");

    // Port 1 is reserved and never bound, so the connection is refused the
    // instant it is made — the same address `common::s3::unreachable_store`
    // uses for the same reason.
    let output = run_hook(
        &["--server", "http://127.0.0.1:1"],
        &[("RADIO_SCOUT_API_KEY", "tr-key")],
        &files,
    )
    .await;

    assert!(
        output.status.success(),
        "a server outage must not fail Trunk Recorder's pipeline"
    );
    let said = stderr_of(&output);
    assert!(
        said.contains("radio-scout: upload failed"),
        "and it must say so, in TR's own log: {said:?}"
    );
}

/// A key the server refuses. Permanent, and the operator has to fix it — but
/// still not worth taking the recorder's other uploaders down over, because
/// with a wrong key that would be *every* Call.
#[tokio::test]
async fn a_refused_key_is_loud_and_still_exits_zero() {
    if !curl_available() {
        return skip("curl is not installed");
    }
    let app = TestApp::with_key("the-real-key").await;
    let files = CallFiles::write("butco");

    let output = run_hook(
        &["--server", &app.url("")],
        &[("RADIO_SCOUT_API_KEY", "not-the-real-key")],
        &files,
    )
    .await;

    assert!(output.status.success(), "one typo must not stop everything");
    let said = stderr_of(&output);
    assert!(
        said.contains("radio-scout: upload refused (HTTP 401)"),
        "{said:?}"
    );
    assert!(
        said.contains("Invalid API key"),
        "the server's own words, so the operator knows which of the two it was: {said:?}"
    );
    assert!(
        !said.contains("not-the-real-key"),
        "the key must not be echoed back into a log the operator may paste: {said:?}"
    );
    assert_eq!(
        app.count::<radio_scout::db::entities::call::Entity>().await,
        0
    );
}

/// The install is wrong, in each of the ways it can be. These exit non-zero: no
/// retry could fix any of them, and TR's "Upload script failed with status N"
/// is what makes an operator notice on the very first Call rather than a week
/// later.
#[rstest::rstest]
#[case::no_server(&[], &[("RADIO_SCOUT_API_KEY", "k")], "no server")]
#[case::no_key(&["--server", "http://127.0.0.1:1"], &[], "no API key")]
#[case::unknown_option(&["--wat"], &[("RADIO_SCOUT_API_KEY", "k")], "unknown option")]
#[case::missing_env_file(
    &["--server", "http://127.0.0.1:1", "--env-file", "/nowhere/at/all"],
    &[],
    "cannot read --env-file"
)]
#[tokio::test]
async fn a_broken_install_exits_non_zero_so_trunk_recorder_says_so(
    #[case] args: &[&str],
    #[case] env: &[(&str, &str)],
    #[case] expected: &str,
) {
    if !curl_available() {
        return skip("curl is not installed");
    }
    let files = CallFiles::write("butco");

    let output = run_hook(args, env, &files).await;

    assert!(
        !output.status.success(),
        "{expected}: should have failed the pipeline"
    );
    let said = stderr_of(&output);
    assert!(said.contains(expected), "expected {expected:?} in {said:?}");
}

/// The metadata is the one file this script cannot do without — no `.json`, no
/// call. TR always writes one, so its absence means something is wrong with the
/// install rather than with the recording.
#[tokio::test]
async fn a_missing_call_json_exits_non_zero() {
    if !curl_available() {
        return skip("curl is not installed");
    }
    let files = CallFiles::write("butco");
    std::fs::remove_file(&files.json).expect("take the metadata away");

    let output = run_hook(
        &["--server", "http://127.0.0.1:1"],
        &[("RADIO_SCOUT_API_KEY", "k")],
        &files,
    )
    .await;

    assert!(!output.status.success());
    assert!(
        stderr_of(&output).contains("no call metadata"),
        "{}",
        stderr_of(&output)
    );
}

/// ...and so does a call with no audio at all, which means the render failed
/// and there is nothing to send.
#[tokio::test]
async fn a_call_with_neither_audio_file_exits_non_zero() {
    if !curl_available() {
        return skip("curl is not installed");
    }
    let files = CallFiles::write("butco");
    std::fs::remove_file(&files.wav).expect("take the audio away");

    let output = run_hook(
        &["--server", "http://127.0.0.1:1"],
        &[("RADIO_SCOUT_API_KEY", "k")],
        &files,
    )
    .await;

    assert!(!output.status.success());
    assert!(
        stderr_of(&output).contains("no audio to upload"),
        "{}",
        stderr_of(&output)
    );
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// The shape a service manager already writes, and the one the recorders guide
/// recommends: both settings in a 0600 file, so the key is never on a command
/// line where `ps` would show it to every user on the recorder.
#[tokio::test]
async fn an_env_file_carries_both_the_server_and_the_key() {
    if !curl_available() {
        return skip("curl is not installed");
    }
    let app = TestApp::with_key("tr-key").await;
    let files = CallFiles::write("butco");
    let dir = tempfile::tempdir().expect("tempdir");
    let env_file = dir.path().join("radio-scout.env");
    std::fs::write(
        &env_file,
        format!(
            "# Radio-Scout\nRADIO_SCOUT_URL={}\nRADIO_SCOUT_API_KEY=tr-key\n",
            app.url("")
        ),
    )
    .expect("write the env file");

    let output = run_hook(
        &["--env-file", env_file.to_str().expect("utf-8 path")],
        &[],
        &files,
    )
    .await;

    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(app.the_call().await.talkgroup_id, 1);
}

/// `--server` beats a file that also sets one, so an operator can point a
/// second recorder at a test instance without editing the file both of them
/// share.
#[tokio::test]
async fn an_explicit_server_wins_over_the_env_files_own() {
    if !curl_available() {
        return skip("curl is not installed");
    }
    let app = TestApp::with_key("tr-key").await;
    let files = CallFiles::write("butco");
    let dir = tempfile::tempdir().expect("tempdir");
    let env_file = dir.path().join("radio-scout.env");
    std::fs::write(
        &env_file,
        "RADIO_SCOUT_URL=http://127.0.0.1:1\nRADIO_SCOUT_API_KEY=tr-key\n",
    )
    .expect("write the env file");

    let output = run_hook(
        &[
            "--env-file",
            env_file.to_str().expect("utf-8 path"),
            "--server",
            &app.url(""),
        ],
        &[],
        &files,
    )
    .await;

    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(
        app.count::<radio_scout::db::entities::call::Entity>().await,
        1,
        "the Call went to the server on the command line, not the one in the file"
    );
}

/// The key never appears in the process's own arguments, whichever way it was
/// configured — `ps` is world-readable on the recorder, which is why
/// `[storage.s3]`'s credentials have no flags either.
#[tokio::test]
async fn there_is_no_way_to_put_the_api_key_on_the_command_line() {
    if !curl_available() {
        return skip("curl is not installed");
    }
    let files = CallFiles::write("butco");

    let output = run_hook(
        &["--server", "http://127.0.0.1:1", "--key", "hunter2"],
        &[("RADIO_SCOUT_API_KEY", "k")],
        &files,
    )
    .await;

    assert!(!output.status.success(), "--key must not be a thing");
    assert!(
        stderr_of(&output).contains("unknown option: --key"),
        "{}",
        stderr_of(&output)
    );
}

/// A human running it by hand to check their setup gets usage, not a crash
/// about missing file arguments.
#[tokio::test]
async fn help_works_with_no_call_attached() {
    let output = Command::new(script())
        .arg("--help")
        .output()
        .await
        .expect("run the upload script");

    assert!(output.status.success());
    let said = String::from_utf8_lossy(&output.stdout);
    assert!(said.contains("uploadScript"), "{said}");
    assert!(
        said.contains("world-readable through ps"),
        "the reason the key has no flag belongs where somebody looking for one reads: {said}"
    );
}

// ---------------------------------------------------------------------------
// What the script actually asks `curl` to do
// ---------------------------------------------------------------------------

/// A `curl` that records the arguments it was given and answers 200.
///
/// Two properties are worth proving about the command line rather than about
/// the upload, and neither is observable from the server's side: that the API
/// key is not on it, and that no option newer than the curl on a Raspberry Pi
/// is on it either.
struct FakeCurl {
    dir: tempfile::TempDir,
}

impl FakeCurl {
    fn install() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("argv");
        let curl = dir.path().join("curl");
        std::fs::write(
            &curl,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> {}; done\nprintf '200'\n",
                log.display()
            ),
        )
        .expect("write the fake curl");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755))
                .expect("chmod the fake curl");
        }
        FakeCurl { dir }
    }

    /// Prepended to `PATH`, so the script finds this `curl` first.
    fn path(&self) -> String {
        let inherited = std::env::var("PATH").unwrap_or_default();
        format!("{}:{inherited}", self.dir.path().display())
    }

    fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("argv"))
            .expect("the script never ran curl")
            .lines()
            .map(str::to_string)
            .collect()
    }
}

/// The API key must not reach `curl`'s own command line.
///
/// `--help` and the recorders guide both promise this, citing the same reason
/// `[storage.s3]`'s credentials have no flags: `ps` is world-readable, so a
/// secret in any process's argv is a secret every user on the recorder can
/// read. Keeping it off *this* script's arguments and then handing it straight
/// to `curl -F key=…` would move the leak one process along and call it fixed.
#[tokio::test]
async fn the_api_key_never_reaches_curls_command_line() {
    if !curl_available() {
        return skip("curl is not installed");
    }
    let fake = FakeCurl::install();
    let files = CallFiles::write("butco");

    run_hook(
        &["--server", "http://example.invalid"],
        &[
            ("RADIO_SCOUT_API_KEY", "super-secret-key"),
            ("PATH", &fake.path()),
        ],
        &files,
    )
    .await;

    let argv = fake.argv();
    assert!(
        !argv.iter().any(|arg| arg.contains("super-secret-key")),
        "the key is in curl's argv, which `ps` shows to everyone: {argv:?}"
    );
}

/// Only options a recorder's `curl` actually has.
///
/// `--fail-with-body` landed in curl 7.76 (2021). Raspberry Pi OS Bullseye —
/// which is what a great many recorders run — ships 7.74, where an unknown
/// option makes curl exit 2 before sending anything. The script would then
/// report a transient-looking failure and exit 0, so **every** Call would fail
/// silently and forever, looking exactly like an outage that never ends.
#[tokio::test]
async fn no_option_newer_than_the_curl_on_a_raspberry_pi_is_used() {
    if !curl_available() {
        return skip("curl is not installed");
    }
    let fake = FakeCurl::install();
    let files = CallFiles::write("butco");

    run_hook(
        &["--server", "http://example.invalid"],
        &[("RADIO_SCOUT_API_KEY", "k"), ("PATH", &fake.path())],
        &files,
    )
    .await;

    let argv = fake.argv();
    for too_new in ["--fail-with-body", "--json", "--url-query", "--variable"] {
        assert!(
            !argv.iter().any(|arg| arg == too_new),
            "{too_new} needs a curl newer than Raspberry Pi OS Bullseye ships: {argv:?}"
        );
    }
}

/// A Trunk Recorder that appends a *fourth* path must not brick the recorder.
///
/// TR's argument list has grown before and can again. A strict arity check
/// would exit non-zero on the first Call after an upgrade — and non-zero here
/// takes every other plugin down with it, on every Call, until somebody notices.
/// `src/ingest.rs`'s `TrMeta` takes the same stance on the same reasoning: a
/// recorder that adds a field must not fail an upload.
#[tokio::test]
async fn a_recorder_that_appends_a_fourth_path_still_uploads() {
    if !curl_available() {
        return skip("curl is not installed");
    }
    let app = TestApp::with_key("tr-key").await;
    let files = CallFiles::write("butco");

    let output = Command::new(script())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("RADIO_SCOUT_API_KEY", "tr-key")
        .args(["--server", &app.url("")])
        .arg(&files.wav)
        .arg(&files.json)
        .arg(&files.m4a)
        .arg("/some/path/a/future/trunk-recorder/adds")
        .output()
        .await
        .expect("run the upload script");

    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(
        app.count::<radio_scout::db::entities::call::Entity>().await,
        1,
        "the Call still arrived"
    );
}

/// A key is opaque bytes an operator chose, not a token with a grammar.
///
/// `curl -F name=value` treats a leading `@` as "upload this file" and a
/// leading `<` as "read the value from this file", so a key beginning with
/// either would silently send something else — or nothing — under the name the
/// server authenticates on. `--form-string` never interprets them.
#[tokio::test]
async fn a_key_that_looks_like_curl_syntax_is_still_just_a_key() {
    if !curl_available() {
        return skip("curl is not installed");
    }
    let awkward = "@/etc/passwd\"key";
    let app = TestApp::with_key(awkward).await;
    let files = CallFiles::write("butco");

    let output = run_hook(
        &["--server", &app.url("")],
        &[("RADIO_SCOUT_API_KEY", awkward)],
        &files,
    )
    .await;

    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(
        app.count::<radio_scout::db::entities::call::Entity>().await,
        1,
        "the key authenticated, so it was sent verbatim: {}",
        stderr_of(&output)
    );
}
