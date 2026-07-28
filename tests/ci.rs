//! Invariants of the CI pipeline itself (ticket #22).
//!
//! A workflow is proven by running it, and the branch's green run is that proof.
//! What a green run *cannot* tell you is that it still checks what it used to:
//! a job narrowed to `cargo test --lib` goes green in half the time while
//! silently skipping the recorder golden suite, and a Rust job that stops
//! downloading `client/dist` goes green while asserting against the fallback
//! page instead of the real UI. Both are silent losses of coverage that look
//! exactly like a fast build.
//!
//! So the handful of properties whose loss is invisible are pinned here. This is
//! deliberately not a YAML schema test — `actionlint` runs in the pipeline and
//! is far better at that than anything written here would be.

use std::path::Path;

/// Every workflow, as (file name, what the runner will actually do).
///
/// **The commentary is stripped**, and that is the load-bearing part. These
/// files explain themselves at length, so `-D warnings` appears in the header
/// comment as well as in the clippy step — and a test that searched the raw file
/// would stay green after the flag itself was deleted, asserting about the prose
/// instead of the pipeline. Only whole-line comments are removed: a `#` can
/// legitimately sit inside a value (`COVERAGE_IGNORE`'s regex), and mangling one
/// would be its own kind of wrong answer.
fn workflows() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let mut found: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("read {}: {err}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "yml"))
        .map(|path| {
            let text = std::fs::read_to_string(&path).expect("read workflow");
            let steps = text
                .lines()
                .filter(|line| !line.trim_start().starts_with('#'))
                .collect::<Vec<_>>()
                .join("\n");
            (
                path.file_name().expect("name").to_string_lossy().into(),
                steps,
            )
        })
        .collect();
    found.sort();
    assert!(!found.is_empty(), "no workflows in {}", dir.display());
    found
}

/// A workflow split into its jobs: `(job name, the job's block)`.
///
/// Jobs are the one top-level mapping under `jobs:`, so they are the lines
/// indented exactly two spaces — enough structure to ask a question per job
/// without taking on a YAML dependency for it. Input is the comment-stripped
/// text from [`workflows`], so a banner between two jobs cannot land in the
/// block of the one above it and answer a question on its behalf.
fn jobs(workflow: &str) -> Vec<(String, String)> {
    let mut jobs: Vec<(String, String)> = Vec::new();
    let mut in_jobs = false;
    for line in workflow.lines() {
        if line.starts_with("jobs:") {
            in_jobs = true;
            continue;
        }
        if !in_jobs {
            continue;
        }
        // A header may carry a trailing comment (`  backend:  # the main one`),
        // which is still a header. Only the header test tolerates that split —
        // a step's text is never trimmed, so a `#` inside a value survives.
        let header = line
            .strip_prefix("  ")
            .filter(|rest| !rest.starts_with(' '))
            .map(|rest| rest.split_once(" #").map_or(rest, |(before, _)| before))
            .and_then(|rest| rest.trim_end().strip_suffix(':'));
        match header {
            Some(name) => jobs.push((name.to_string(), String::new())),
            None => {
                if let Some((_, block)) = jobs.last_mut() {
                    block.push_str(line);
                    block.push('\n');
                }
            }
        }
    }
    jobs
}

/// Every job that runs cargo first gets the real SPA — by downloading the
/// `client-dist` artifact, or by building it itself where there is no artifact
/// to take (the nightly sweep runs in its own workflow).
///
/// `rust-embed` reads `client/dist` **at compile time** (`src/web.rs`), so a
/// Rust job without it compiles a binary serving the minimal fallback page — and
/// `tests/frontend.rs` then asserts the fallback, passing while proving nothing
/// about the app that ships. That makes the SPA a build input to every cargo
/// job, not an output beside them.
#[test]
fn every_cargo_job_takes_the_built_client_with_it() {
    for (name, workflow) in workflows() {
        for (job, block) in jobs(&workflow) {
            if !block.contains("cargo ") {
                continue;
            }
            assert!(
                block.contains("client-dist") || block.contains("npm run build"),
                "{name}: job `{job}` runs cargo without the built SPA, so rust-embed \
                 would compile in the fallback page and the frontend tests would \
                 assert against it"
            );
        }
    }
}

/// The suite CI runs is the whole suite.
///
/// `cargo test --lib` runs only the in-crate unit tests: it silently skips every
/// `tests/*.rs` binary, and with them the recorder golden suite that guards the
/// drop-in-replacement guarantee (#7). Doctests are the mirror-image gap —
/// nextest does not run them at all — so both spellings have to appear.
#[test]
fn ci_runs_the_integration_suites_and_the_doctests_not_just_the_lib() {
    let ci = ci_workflow();

    assert!(
        !ci.contains("--lib"),
        "a `--lib` run skips tests/golden.rs and every other integration binary"
    );
    assert!(ci.contains("nextest"), "the unit + integration suites");
    assert!(
        ci.contains("cargo test --doc"),
        "nextest does not run doctests; something has to"
    );
}

/// The dual-dialect run is a second, real run of the suite.
///
/// `TestApp` picks its dialect from `TEST_POSTGRES_URL` (`tests/common/mod.rs`),
/// so a Postgres job that stands a server up and forgets to hand the URL to the
/// suite passes by running SQLite twice — green, and half of what it claims.
#[test]
fn ci_points_the_suite_at_the_postgres_it_provisions() {
    let ci = ci_workflow();

    assert!(ci.contains("postgres"), "a Postgres service is provisioned");
    assert!(
        ci.contains("TEST_POSTGRES_URL"),
        "and the suite is told where it is"
    );
}

/// Merging is gated on formatting, lints, the ratcheting project floor **and**
/// 100% patch coverage — ADR-0010's headline gate, the one that makes "new code
/// ships with tests" true by construction rather than by review.
///
/// `-D warnings` is the half of clippy that matters: without it clippy reports
/// and exits zero, so the job stays green while the lints pile up.
#[test]
fn ci_gates_on_format_lints_and_both_coverage_rules() {
    let ci = ci_workflow();

    for gate in [
        "cargo fmt",
        "cargo clippy",
        "-D warnings",
        "--fail-under-lines",
        "patch-coverage",
    ] {
        assert!(ci.contains(gate), "the merge gate is missing `{gate}`");
    }
}

/// The suite runs on the architecture the scanner runs on (#38).
///
/// Every test here only ever ran on x86_64 until this job: the matrix
/// *compiles* for aarch64 and stops. The enhancement pipeline (#20) is
/// float-heavy and `nnnoiseless` picks SIMD paths per architecture, so a test
/// that passes on the runner and fails on the Pi would ship to exactly the user
/// this project is built for.
///
/// The failure this pins is the cheap one: a job narrowed to `cargo build` goes
/// green faster while proving only what the matrix already proved.
#[test]
fn the_suite_runs_on_arm64_rather_than_only_compiling_for_it() {
    let ci = ci_workflow();
    let (name, block) = jobs(&ci)
        .into_iter()
        .find(|(_, block)| block.contains("ubuntu-24.04-arm"))
        .expect("no job runs on an arm64 runner, so the Pi's architecture is never tested");

    assert!(
        block.contains("nextest run"),
        "{name} runs on arm64 without running the suite there"
    );
    assert!(
        !block.contains("continue-on-error"),
        "{name} tests the primary deployment target; it is a gate, not a signal"
    );
}

/// `ci.yml` builds every target in **debug** and uploads nothing, deliberately
/// (#22). `release.yml` (#23) is therefore the only place `--release` ever
/// runs, which makes it the only place `[profile.release]` — fat LTO, one
/// codegen unit — is exercised at all.
#[test]
fn the_release_workflow_is_the_one_that_builds_in_release_mode() {
    let release = release_workflow();

    assert!(
        release.contains("--release"),
        "a release built in debug is a release nobody wants on a Pi"
    );
    assert!(
        release.contains("--locked"),
        "a release must build the dependency versions that were tested"
    );
}

/// A checksum file nobody publishes is a checksum nobody can check — and
/// `install.sh` refuses to install without one, so this is the difference
/// between a working `curl | sh` and one that dies at the last step.
#[test]
fn the_release_workflow_publishes_the_checksums_the_installer_verifies() {
    assert!(release_workflow().contains("SHA256SUMS"));
}

/// The Pi is the target that matters and it is arm64, so an image built only
/// for amd64 is an image the scanner's own hardware cannot run.
#[test]
fn the_published_image_covers_both_architectures() {
    let release = release_workflow();

    for platform in ["linux/amd64", "linux/arm64"] {
        assert!(release.contains(platform), "missing {platform}:\n{release}");
    }
}

/// The image and the release both carry a version, and only the `version` job
/// checks that the version is the one `Cargo.toml` will report. A publishing
/// job that does not wait for it publishes past it.
#[test]
fn nothing_is_published_without_the_version_check() {
    for (job, block) in jobs(&release_workflow()) {
        if !block.contains("push: true") && !block.contains("gh release create") {
            continue;
        }
        assert!(
            block.contains("needs: [build, version]"),
            "{job} publishes without waiting for the tag to be checked:\n{block}"
        );
    }
}

/// `install.sh` is the one piece of hand-written shell this project invites a
/// user to pipe into their own. `actionlint` reads workflows only, so nothing
/// else would ever look at it.
#[test]
fn the_installer_is_shellchecked_by_the_pipeline() {
    let ci = ci_workflow();

    assert!(ci.contains("shellcheck"), "the pipeline runs no shellcheck");
    assert!(
        ci.contains("install.sh"),
        "shellcheck does not cover the installer:\n{ci}"
    );
}

/// The pull-request pipeline, which is where every merge gate lives.
fn ci_workflow() -> String {
    named_workflow("ci.yml")
}

/// The tag pipeline: what ships (#23).
fn release_workflow() -> String {
    named_workflow("release.yml")
}

fn named_workflow(name: &str) -> String {
    workflows()
        .into_iter()
        .find(|(found, _)| found == name)
        .unwrap_or_else(|| panic!("`.github/workflows/{name}` is missing"))
        .1
}
