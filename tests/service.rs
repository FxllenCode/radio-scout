//! `radio-scout service …` through the shipped executable (#23, spec US 42).
//!
//! `src/service.rs` proves what every platform's definition *says* — it renders
//! all three wherever the suite happens to run. What it cannot prove is that
//! the subcommand is reachable, that the flags an operator types arrive, and
//! that the two things a service cannot inherit — an absolute base directory
//! and the configuration file that was actually read — are resolved against the
//! working directory the operator ran the command in. That is `main.rs`, and
//! the only honest way to test it is to run it.
//!
//! Only `--print` is exercised: the other path writes into `/etc` or
//! `/Library` and drives the host's real service manager, which is a live test
//! ([`docs/agents/live-testing.md`]), not a suite one.

use std::path::Path;
use std::process::{Command, Output};

/// Run the binary from `cwd`, in an environment stripped of anything a
/// developer's shell (or the live-test loop) routinely has set.
fn radio_scout(cwd: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_radio-scout"));
    command.args(args).current_dir(cwd);
    for (name, _) in std::env::vars() {
        if name.starts_with("RADIO_SCOUT_") {
            command.env_remove(name);
        }
    }
    command.env_remove("RUST_LOG");
    command.output().expect("run radio-scout")
}

/// A temporary directory, by the path a child process will report for it.
///
/// On macOS `TMPDIR` is under `/var`, which is a symlink to `/private/var` —
/// so the path the test holds and the working directory the binary resolves
/// differ by a prefix, and every assertion about an absolutised path fails on
/// the difference rather than on the behaviour.
fn workspace() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().canonicalize().expect("canonicalize");
    (dir, path)
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The dry run an operator does before letting anything write to `/etc`.
#[test]
fn a_printed_install_shows_the_definition_it_would_write() {
    let (_dir, cwd) = workspace();

    let output = radio_scout(
        &cwd,
        &["service", "install", "--print", "--base-dir", "scanner"],
    );

    assert!(output.status.success(), "{}", stderr(&output));
    let printed = stdout(&output);
    assert!(
        printed.contains("--- write "),
        "nothing would be written:\n{printed}"
    );
    // The flag and its value are asserted separately because a launchd plist
    // puts every argument in an element of its own — what matters is that both
    // are in the definition, not that they are adjacent in it.
    assert!(printed.contains("--base-dir"), "{printed}");
    // Resolved against the working directory, not copied through: a relative
    // path in a service definition resolves against `/` at boot.
    let base_dir = cwd.join("scanner");
    assert!(
        printed.contains(&base_dir.display().to_string()),
        "the base directory was not made absolute:\n{printed}"
    );
    assert!(
        printed.contains(env!("CARGO_BIN_EXE_radio-scout")),
        "the service would run some other binary:\n{printed}"
    );
    assert!(
        printed.contains("--- run "),
        "nothing would be run:\n{printed}"
    );
}

/// Every setting flag is `global`, so it reads the same after the verb as
/// before it — and what it is set to is what gets baked in.
#[test]
fn a_flag_given_to_install_is_baked_into_the_definition() {
    let (_dir, cwd) = workspace();

    let output = radio_scout(&cwd, &["service", "install", "--print", "--port", "8080"]);

    assert!(output.status.success(), "{}", stderr(&output));
    let printed = stdout(&output);
    assert!(printed.contains("--port"), "{printed}");
    assert!(printed.contains("8080"), "{printed}");
}

/// The configuration file is found relative to the working directory, which is
/// the operator's — never the service's. So the definition has to name it
/// absolutely, or the service comes up on the defaults instead.
#[test]
fn a_configuration_file_beside_the_operator_is_named_absolutely() {
    let (_dir, cwd) = workspace();
    std::fs::write(cwd.join("radio-scout.toml"), "[server]\nport = 4000\n").expect("write config");

    let output = radio_scout(&cwd, &["service", "install", "--print"]);

    assert!(output.status.success(), "{}", stderr(&output));
    let printed = stdout(&output);
    assert!(printed.contains("--config"), "{printed}");
    assert!(
        printed.contains(&cwd.join("radio-scout.toml").display().to_string()),
        "{printed}"
    );
}

/// A service definition is world-readable and a database URL routinely carries
/// a password. Exit 2 is "fix your configuration", the same code an unusable
/// setting gets.
#[test]
fn a_database_url_refuses_the_install_without_echoing_the_url() {
    let (_dir, cwd) = workspace();

    let output = radio_scout(
        &cwd,
        &[
            "service",
            "install",
            "--print",
            "--database-url",
            "postgres://scanner:hunter2@db.example/radio",
        ],
    );

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let said = format!("{}{}", stdout(&output), stderr(&output));
    assert!(said.contains("--database-url"), "{said}");
    assert!(!said.contains("hunter2"), "the secret leaked: {said}");
}

/// rdio-scanner's `-service` takes `start|stop|restart|install|uninstall`;
/// these are those, plus the `status` it has no equivalent of.
#[test]
fn the_subcommand_offers_every_verb() {
    let (_dir, cwd) = workspace();

    let output = radio_scout(&cwd, &["service", "--help"]);

    assert!(output.status.success(), "{}", stderr(&output));
    let help = stdout(&output);
    for verb in ["install", "uninstall", "start", "stop", "restart", "status"] {
        assert!(help.contains(verb), "`{verb}` is missing from:\n{help}");
    }
}

/// Serving is still what running the binary with no verb means.
#[test]
fn the_top_level_help_still_documents_serving() {
    let (_dir, cwd) = workspace();

    let output = radio_scout(&cwd, &["--help"]);

    assert!(output.status.success(), "{}", stderr(&output));
    let help = stdout(&output);
    assert!(help.contains("service"), "{help}");
    assert!(help.contains("--base-dir"), "{help}");
}
