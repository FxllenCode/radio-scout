//! What ships, and the one thing that can silently stop it shipping (#23,
//! spec US 42, ADR-0007).
//!
//! Three files have to agree on a single string — the name of a release asset.
//! `.github/workflows/release.yml` builds it, `install.sh` fetches it, and
//! nothing connects them: rename a target, or add one, and the installer keeps
//! working right up until somebody runs it on the platform that moved, at which
//! point it 404s. Neither file's own tests can see that, because the bug is in
//! the gap between them.
//!
//! So the installer is run — really run, against a release served off the
//! filesystem — for every target the workflow claims to build. What it fetches,
//! verifies, unpacks and installs is asserted as bytes on a disk, not as a
//! string in a script.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The repository root.
fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The release workflow, with whole-line comments stripped.
///
/// The workflows' *own* invariants are pinned in `tests/ci.rs`, which is where
/// that job belongs. What is read here is one value — the build matrix — and it
/// is read because the installer has to agree with it, not because the workflow
/// is under test. The comments come off for the reason `tests/ci.rs` explains
/// at length: these files document themselves, and a test that greps the raw
/// text asserts about the prose.
fn release_workflow() -> String {
    let path = repo().join(".github/workflows/release.yml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    text.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `target:` the release matrix names — the list the installer has to be
/// able to ask for, and the only place it is written down.
fn released_targets() -> Vec<String> {
    let targets: Vec<String> = release_workflow()
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            // The first target of a matrix entry carries the list's `- `.
            line.strip_prefix("- ")
                .unwrap_or(line)
                .strip_prefix("target: ")
                .map(str::to_string)
        })
        .collect();
    assert!(
        targets.len() >= 4,
        "the release matrix should cover the Pi, both desktops and Windows: {targets:?}"
    );
    targets
}

/// The archive a target ships in. Windows gets a zip because Windows has no
/// `tar` habit and every version of it can open a zip from Explorer.
fn archive_extension(target: &str) -> &'static str {
    match target.contains("windows") {
        true => "zip",
        false => "tar.gz",
    }
}

fn asset_name(version: &str, target: &str) -> String {
    format!(
        "radio-scout-{version}-{target}.{}",
        archive_extension(target)
    )
}

/// Run `install.sh`, with `PATH` prefixed by `shims` so a test can decide what
/// `uname` says about the machine.
fn install_sh(args: &[&str], shims: Option<&Path>) -> Output {
    let script = repo().join("install.sh");
    let mut command = Command::new("sh");
    command.arg(&script).args(args);
    if let Some(shims) = shims {
        let path = std::env::var("PATH").unwrap_or_default();
        command.env("PATH", format!("{}:{path}", shims.display()));
    }
    command.output().expect("run install.sh")
}

fn said(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// A directory holding a `uname` that answers whatever this test says it does.
///
/// Shadowing the real one on `PATH` keeps the detection seam out of the shipped
/// script: there is no test-only environment variable for a user to find and
/// no branch that only exists for us.
fn uname_saying(system: &str, machine: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = format!(
        "#!/bin/sh\n\
         case \"$1\" in\n\
         -s) echo {system} ;;\n\
         -m) echo {machine} ;;\n\
         *) echo {system} ;;\n\
         esac\n"
    );
    let path = dir.path().join("uname");
    std::fs::write(&path, script).expect("write uname");
    make_executable(&path);
    dir
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}

/// A release, on the filesystem, that `install.sh` can be pointed at with a
/// `file://` base URL — every byte of the real path except the hostname.
struct FakeRelease {
    _dir: tempfile::TempDir,
    root: PathBuf,
    version: String,
}

impl FakeRelease {
    /// One archive per target, each holding a `radio-scout` that says what it
    /// is, plus the `SHA256SUMS` the installer checks them against.
    fn published(targets: &[String]) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let version = "v9.9.9".to_string();
        let staging = root.join("staging");
        std::fs::create_dir_all(&staging).expect("staging");
        let binary = staging.join("radio-scout");
        std::fs::write(&binary, "#!/bin/sh\necho radio-scout 9.9.9\n").expect("write binary");
        make_executable(&binary);

        for target in targets {
            let asset = asset_name(&version, target);
            if archive_extension(target) == "zip" {
                // Windows is not installed by a shell script; the asset exists
                // so the checksum file covers it.
                std::fs::write(root.join(&asset), b"zip").expect("write zip");
                continue;
            }
            let status = Command::new("tar")
                .args(["-czf"])
                .arg(root.join(&asset))
                .args(["-C"])
                .arg(&staging)
                .arg("radio-scout")
                .status()
                .expect("tar");
            assert!(status.success(), "packing {asset}");
        }
        let release = FakeRelease {
            _dir: dir,
            root,
            version,
        };
        release.write_checksums();
        release
    }

    fn write_checksums(&self) {
        let mut names: Vec<String> = std::fs::read_dir(&self.root)
            .expect("read release")
            .map(|entry| entry.expect("entry").file_name().to_string_lossy().into())
            .filter(|name: &String| name.starts_with("radio-scout-"))
            .collect();
        names.sort();
        let mut sums = String::new();
        for name in names {
            let digest = sha256(&self.root.join(&name));
            sums.push_str(&format!("{digest}  {name}\n"));
        }
        std::fs::write(self.root.join("SHA256SUMS"), sums).expect("write SHA256SUMS");
    }

    fn base_url(&self) -> String {
        format!("file://{}", self.root.display())
    }
}

/// The digest, computed by a tool that is not the one the installer uses, so
/// the test and the script cannot be wrong in the same way.
fn sha256(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("read");
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(&bytes))
}

// ---------------------------------------------------------------------------
// The installer and the release, against each other.
// ---------------------------------------------------------------------------

/// The gap this whole file exists for: a machine the installer recognises has
/// to map to a target the release actually publishes.
#[test]
fn every_machine_the_installer_recognises_maps_to_a_published_target() {
    let released = released_targets();

    for (system, machine) in [
        ("Linux", "x86_64"),
        ("Linux", "amd64"),
        ("Linux", "aarch64"),
        ("Linux", "arm64"),
        ("Darwin", "arm64"),
        ("Darwin", "x86_64"),
    ] {
        let shims = uname_saying(system, machine);
        let output = install_sh(&["--dry-run", "--version", "v9.9.9"], Some(shims.path()));

        assert!(
            output.status.success(),
            "{system}/{machine} was not recognised: {}",
            said(&output)
        );
        let printed = said(&output);
        let target = released
            .iter()
            .find(|target| printed.contains(target.as_str()))
            .unwrap_or_else(|| {
                panic!("{system}/{machine} resolved to a target no release builds:\n{printed}")
            });
        assert!(
            printed.contains(&asset_name("v9.9.9", target)),
            "{system}/{machine} asks for something the release does not publish:\n{printed}"
        );
    }
}

/// A 32-bit Pi, a BSD, an s390x: all real, none built. Saying so is the whole
/// difference between "unsupported" and a 404 from a `curl | sh`.
#[test]
fn a_platform_no_release_covers_is_refused_by_name() {
    let shims = uname_saying("Linux", "armv7l");

    let output = install_sh(&["--dry-run", "--version", "v9.9.9"], Some(shims.path()));

    assert!(!output.status.success(), "{}", said(&output));
    let message = said(&output);
    assert!(message.contains("armv7l"), "{message}");
}

/// The whole path, for real: fetch, verify, unpack, install, and a binary that
/// runs afterwards.
#[test]
fn installing_a_release_leaves_a_binary_that_runs() {
    let release = FakeRelease::published(&released_targets());
    let into = tempfile::tempdir().expect("tempdir");
    let shims = uname_saying(host_system(), host_machine());

    let output = install_sh(
        &[
            "--version",
            &release.version,
            "--base-url",
            &release.base_url(),
            "--dir",
            &into.path().display().to_string(),
        ],
        Some(shims.path()),
    );

    assert!(output.status.success(), "{}", said(&output));
    let installed = into.path().join("radio-scout");
    assert!(
        installed.exists(),
        "nothing was installed: {}",
        said(&output)
    );
    let ran = Command::new(&installed).output().expect("run it");
    assert!(
        String::from_utf8_lossy(&ran.stdout).contains("radio-scout"),
        "the installed file does not run"
    );
}

/// A `curl | sh` installer downloads a binary over the network and runs it as
/// whatever user it was invoked as. Checking the digest is the only thing
/// standing between that and whatever the mirror served.
#[test]
fn a_download_that_does_not_match_its_checksum_installs_nothing() {
    let release = FakeRelease::published(&released_targets());
    let into = tempfile::tempdir().expect("tempdir");
    let shims = uname_saying(host_system(), host_machine());
    // Tampered *after* the checksums were written — exactly the shape of the
    // failure this guards against.
    let asset = asset_name(&release.version, &host_target(&released_targets()));
    std::fs::write(release.root.join(&asset), b"not the binary you asked for").expect("tamper");

    let output = install_sh(
        &[
            "--version",
            &release.version,
            "--base-url",
            &release.base_url(),
            "--dir",
            &into.path().display().to_string(),
        ],
        Some(shims.path()),
    );

    assert!(!output.status.success(), "{}", said(&output));
    assert!(
        said(&output).to_lowercase().contains("checksum"),
        "{}",
        said(&output)
    );
    assert!(
        !into.path().join("radio-scout").exists(),
        "a binary that failed its checksum was installed anyway"
    );
}

/// `--dry-run` is what an operator runs when they have been told never to pipe
/// a stranger's script into a shell, and it has to be true to its word.
#[test]
fn a_dry_run_downloads_nothing_and_installs_nothing() {
    let release = FakeRelease::published(&released_targets());
    let into = tempfile::tempdir().expect("tempdir");
    let shims = uname_saying(host_system(), host_machine());

    let output = install_sh(
        &[
            "--dry-run",
            "--version",
            &release.version,
            "--base-url",
            &release.base_url(),
            "--dir",
            &into.path().display().to_string(),
        ],
        Some(shims.path()),
    );

    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(
        std::fs::read_dir(into.path()).expect("read").count(),
        0,
        "a dry run installed something"
    );
}

fn host_system() -> &'static str {
    match std::env::consts::OS {
        "macos" => "Darwin",
        _ => "Linux",
    }
}

fn host_machine() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        _ => "x86_64",
    }
}

/// The target this host would install.
fn host_target(released: &[String]) -> String {
    let os = match std::env::consts::OS {
        "macos" => "apple-darwin",
        _ => "linux-musl",
    };
    released
        .iter()
        .find(|target| target.contains(os) && target.starts_with(std::env::consts::ARCH))
        .unwrap_or_else(|| panic!("no released target for this host: {released:?}"))
        .clone()
}

/// The uploadScript (#43) is a release asset too, and nothing at runtime says
/// so: `release.yml` publishes it, `docs/recorders.md` tells an operator to
/// fetch it, and the two agree only by having been written on the same day.
///
/// The same gap this file exists for, one asset along — so the same answer.
/// Asserted against the workflow with its comments stripped, since the file
/// explains itself at length and a test that read the prose would stay green
/// after the step it describes was deleted.
#[test]
fn the_trunk_recorder_upload_script_ships_with_the_release() {
    let name = "radio-scout-upload.sh";
    assert!(
        repo().join(name).is_file(),
        "{name} should be at the repository root, beside install.sh"
    );

    let workflow = release_workflow();
    assert!(
        workflow.contains(name),
        "{name} is never published, so `curl`ing it off a release 404s"
    );

    // It has to land in `dist/`, because that is the directory the checksum
    // step sums and the `gh release create` step uploads. A step that copied it
    // anywhere else would satisfy the assertion above and publish nothing.
    let staged = workflow
        .lines()
        .any(|line| line.contains(name) && line.contains("dist"));
    assert!(
        staged,
        "{name} is mentioned but never staged into dist/, which is what gets uploaded"
    );
}

/// ...and it is executable in the repository, because a release asset is served
/// as the bytes it is committed as. An operator who downloads a script without
/// its executable bit gets "Permission denied" from Trunk Recorder's `execvp`
/// and a call that never uploads.
#[test]
#[cfg(unix)]
fn the_trunk_recorder_upload_script_is_executable() {
    use std::os::unix::fs::PermissionsExt;

    let path = repo().join("radio-scout-upload.sh");
    let mode = std::fs::metadata(&path)
        .expect("stat the script")
        .permissions()
        .mode();
    assert!(
        mode & 0o111 != 0,
        "radio-scout-upload.sh is not executable (mode {mode:o})"
    );
}
