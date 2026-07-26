//! Booting the real binary (#17, spec US 35-36).
//!
//! Everything else in the suite drives the router in-process through
//! `common::TestApp`, which is the right seam for behavior — but it cannot
//! answer the question this ticket is judged on: *does the shipped executable,
//! run in an empty directory with no arguments, come up?* That path is
//! `main.rs` — argument parsing, config discovery, directory creation, the
//! database and store it opens, the exit code it leaves behind — and the only
//! honest way to test it is to run it.
//!
//! So each test here spawns `radio-scout` in a temp directory, reads its log
//! from stdout (ADR-0011: stdout is where the log goes), and either drives it
//! over HTTP or waits for it to exit.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// How long a spawned scanner gets to say something. Slack, not a timing
/// assumption: it bounds a failure rather than defining a pass. A boot takes
/// well under a second, so this is ~15x headroom — and deliberately shorter
/// than the 20s timeout `cargo mutants` sets from the baseline run, so a mutant
/// that stops the binary booting is reported as *caught* rather than as a
/// timeout nobody can interpret (ADR-0010: mutation testing is the quality
/// gate, so it has to give a clean verdict).
const BOOT_TIMEOUT: Duration = Duration::from_secs(10);

/// A running scanner, its log, and the guarantee that it dies with the test.
struct Scanner {
    child: Child,
    /// Everything read from stdout so far, so a failure can show the log.
    log: String,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
}

impl Scanner {
    /// Run the binary with `args`, from `cwd`, in a hermetic environment.
    ///
    /// The inherited environment is stripped of anything that would configure
    /// the child (`RADIO_SCOUT_*`, `RUST_LOG`), because a developer's shell —
    /// or the live-test loop — routinely has those set, and a test that passes
    /// only on a clean machine tests nothing.
    fn start(cwd: &Path, args: &[&str]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_radio-scout"));
        command
            .args(args)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for (name, _) in std::env::vars() {
            if name.starts_with("RADIO_SCOUT_") {
                command.env_remove(name);
            }
        }
        command.env_remove("RUST_LOG");

        let mut child = command.spawn().expect("spawn radio-scout");
        let stdout = child.stdout.take().expect("piped stdout");
        Scanner {
            child,
            log: String::new(),
            lines: BufReader::new(stdout).lines(),
        }
    }

    /// Read the log until a line contains `needle`, and return it.
    async fn wait_for(&mut self, needle: &str) -> String {
        let deadline = tokio::time::sleep(BOOT_TIMEOUT);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                line = self.lines.next_line() => match line.expect("read stdout") {
                    Some(line) => {
                        self.log.push_str(&line);
                        self.log.push('\n');
                        if line.contains(needle) {
                            return line;
                        }
                    }
                    None => panic!("radio-scout stopped before saying {needle:?}; log:\n{}", self.log),
                },
                _ = &mut deadline => {
                    panic!("radio-scout never said {needle:?} in {BOOT_TIMEOUT:?}; log:\n{}", self.log)
                }
            }
        }
    }

    /// Wait for the process to exit, returning its status code and its log.
    async fn wait_for_exit(mut self) -> (i32, String) {
        let status = tokio::time::timeout(BOOT_TIMEOUT, async {
            while let Some(line) = self.lines.next_line().await.expect("read stdout") {
                self.log.push_str(&line);
                self.log.push('\n');
            }
            self.child.wait().await.expect("wait")
        })
        .await
        .unwrap_or_else(|_| panic!("radio-scout never exited; log:\n{}", self.log));

        (status.code().expect("an exit code"), self.log)
    }

    /// The address it is listening on, once it says so.
    async fn listening_addr(&mut self) -> String {
        let line = self.wait_for("radio-scout listening").await;
        line.split("addr=")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap_or_else(|| panic!("no addr= in {line:?}"))
            .replace("0.0.0.0", "127.0.0.1")
    }
}

/// `GET path` against a running scanner.
async fn get(addr: &str, path: &str) -> (u16, String) {
    let resp = reqwest::get(format!("http://{addr}{path}"))
        .await
        .expect("GET");
    (resp.status().as_u16(), resp.text().await.expect("body"))
}

/// The acceptance criterion, whole: an empty directory, no config file, and
/// **the default `base_dir`** — a scanner that comes up with a database, an
/// audio store and a working HTTP surface.
///
/// The one flag is `--port 0`, because the default 3000 would collide with
/// whatever the developer is already running (and with the other tests in this
/// file). Everything the criterion is about — that a path nobody configured
/// gets created, and where — is left to the default.
#[tokio::test]
async fn a_first_run_with_no_config_creates_everything_and_serves() {
    let dir = tempfile::tempdir().expect("tempdir");
    // `./radio-scout-data`, resolved against the working directory the scanner
    // is started in, which is this test's temp dir.
    let base_dir = dir.path().join("radio-scout-data");

    let mut scanner = Scanner::start(dir.path(), &["--port", "0"]);
    let addr = scanner.listening_addr().await;

    assert_eq!(get(&addr, "/healthz").await, (200, "ok".to_string()));
    // The three things zero-config has to create, none of which existed a
    // moment ago.
    assert!(base_dir.is_dir(), "base_dir was not created");
    assert!(
        base_dir.join("radio-scout.db").is_file(),
        "the SQLite database was not created"
    );
    assert!(
        base_dir.join("audio").is_dir(),
        "the audio store was not created"
    );
    // ...and it says it is running on the defaults, so "no file" is visible
    // rather than inferred.
    assert!(
        scanner.log.contains("no configuration file"),
        "{}",
        scanner.log
    );
}

/// `radio-scout.toml` in the working directory is found without being asked
/// for, and what it says is what runs.
#[tokio::test]
async fn a_config_file_in_the_working_directory_is_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base_dir = dir.path().join("from-the-file");
    std::fs::write(
        dir.path().join("radio-scout.toml"),
        format!(
            "[server]\nport = 0\nbase_dir = {:?}\n\n[retention]\ndays = 30\n",
            base_dir.display().to_string()
        ),
    )
    .expect("write config");

    let mut scanner = Scanner::start(dir.path(), &[]);
    let addr = scanner.listening_addr().await;

    assert_eq!(get(&addr, "/healthz").await.0, 200);
    assert!(base_dir.is_dir(), "the configured base_dir was not used");
    assert!(
        scanner.log.contains("radio-scout.toml"),
        "boot must name the file it read:\n{}",
        scanner.log
    );
    assert!(
        scanner.log.contains("days=30"),
        "the configured retention policy must be the one logged:\n{}",
        scanner.log
    );
}

/// A flag beats the file, which is the half of precedence rdio-scanner gets
/// backwards (`server/config.go:96-137` re-reads the INI *over* the flags).
#[tokio::test]
async fn a_flag_overrides_the_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let from_file = dir.path().join("from-the-file");
    let from_flag = dir.path().join("from-the-flag");
    std::fs::write(
        dir.path().join("radio-scout.toml"),
        format!(
            "[server]\nport = 0\nbase_dir = {:?}\n",
            from_file.display().to_string()
        ),
    )
    .expect("write config");

    let mut scanner = Scanner::start(
        dir.path(),
        &["--base-dir", &from_flag.display().to_string()],
    );
    scanner.listening_addr().await;

    assert!(from_flag.is_dir(), "the flag must win");
    assert!(!from_file.exists(), "the file must not have won");
}

/// A configuration that cannot be run stops the boot, says which key, and exits
/// with a code an init system can act on — rather than starting on defaults the
/// operator never asked for.
#[tokio::test]
async fn an_invalid_configuration_refuses_to_boot() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("radio-scout.toml"),
        "[retention]\ndayz = 30\n",
    )
    .expect("write config");

    let (code, log) = Scanner::start(dir.path(), &[]).wait_for_exit().await;

    assert_eq!(code, 2, "log:\n{log}");
    assert!(log.contains("invalid configuration"), "{log}");
    assert!(log.contains("dayz"), "{log}");
}

/// `--write-config` produces a file that is documentation *and* runs: the
/// template is written, then handed straight back to the binary as its config.
#[tokio::test]
async fn the_written_config_template_is_one_the_binary_accepts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("radio-scout.toml");

    let (code, log) = Scanner::start(dir.path(), &["--write-config"])
        .wait_for_exit()
        .await;
    assert_eq!(code, 0, "log:\n{log}");
    assert!(path.is_file(), "no config file was written");

    // Writing over it would destroy hand-edits, so a second run refuses.
    let (code, log) = Scanner::start(dir.path(), &["--write-config"])
        .wait_for_exit()
        .await;
    assert_eq!(code, 2, "log:\n{log}");

    let base_dir = dir.path().join("data");
    let mut scanner = Scanner::start(
        dir.path(),
        &[
            "--config",
            &path.display().to_string(),
            "--base-dir",
            &base_dir.display().to_string(),
            "--port",
            "0",
        ],
    );
    let addr = scanner.listening_addr().await;

    assert_eq!(get(&addr, "/healthz").await.0, 200);
}
