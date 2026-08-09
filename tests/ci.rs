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

/// Every workflow, as (file name, what the runner will actually do) — the
/// commentary [stripped](without_comments).
fn workflows() -> Vec<(String, String)> {
    let dir = repo_file(".github/workflows");
    let mut found: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("read {}: {err}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "yml"))
        .map(|path| {
            let text = std::fs::read_to_string(&path).expect("read workflow");
            (
                path.file_name().expect("name").to_string_lossy().into(),
                without_comments(&text),
            )
        })
        .collect();
    found.sort();
    assert!(!found.is_empty(), "no workflows in {}", dir.display());
    found
}

/// A pipeline file's settings, with its prose removed — **the load-bearing part
/// of every assertion here.**
///
/// These files explain themselves at length, and the words they argue about are
/// the same words an assertion looks for: `-D warnings` appears in `ci.yml`'s
/// header comment as well as in the clippy step, and `.config/nextest.toml`
/// spends a screen explaining why it sets neither `retries` nor `fail-fast`. A
/// test that searched the raw text would find the argument and stay green after
/// the setting itself was deleted — asserting about the writing instead of the
/// run, which is the one failure this file cannot afford.
///
/// Only whole-line comments go: a `#` can legitimately sit inside a value
/// (`COVERAGE_IGNORE`'s regex), and mangling one would be its own kind of wrong
/// answer.
fn without_comments(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
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

/// Every run of the suite is on the profile the config describes (#103).
///
/// `.config/nextest.toml` was written two days before `ci.yml` existed, with a
/// `[profile.ci]` in it for the pipeline to select — and the pipeline never
/// selected it. So CI has always run `[profile.default]`, and the retries, the
/// looser slow-timeout and the JUnit file that other profile promised were all
/// silently off. A profile nobody selects reads exactly like one everybody
/// does: green, fast, and describing a run that never happened.
///
/// Asked **per call site**, which is the part a set union over the workflows
/// would get wrong. What started this was two bare invocations, and a profile
/// named on one `cargo nextest` line but not its neighbours is the same failure
/// wearing a smaller hat — half the pipeline on the settings somebody wrote
/// down, half on the ones they didn't.
///
/// `default` is what a silent command line gets, so it is the profile an
/// un-naming call site is holding; the config may then define nothing else.
#[test]
fn every_run_of_the_suite_is_on_the_profile_the_config_defines() {
    let in_force = profiles_in_force();
    let (first_at, first) = in_force.first().expect("no workflow runs the suite");

    for (at, profile) in &in_force {
        assert_eq!(
            profile, first,
            "{at} runs the suite on nextest profile `{profile}` while {first_at} \
             runs it on `{first}` — half a pipeline on a profile is how this \
             started"
        );
    }

    let profiles = nextest_profiles();
    let defined: Vec<&str> = profiles
        .keys()
        .map(String::as_str)
        .filter(|name| *name != DEFAULT_PROFILE)
        .collect();
    let expected: Vec<&str> = std::iter::once(first.as_str())
        .filter(|profile| *profile != DEFAULT_PROFILE)
        .collect();
    assert_eq!(
        defined, expected,
        "`{NEXTEST_CONFIG}` defines {defined:?} while the pipeline runs on \
         `{first}`: a profile nobody selects has never run"
    );
}

/// [`profile_named`] can read a selection at all (#103).
///
/// This is the one assertion above that today's pipeline cannot make for
/// itself. **Nothing** in the repository names a nextest profile — that is the
/// decision — so every call site resolves to [`DEFAULT_PROFILE`], and a
/// [`profile_named`] that had quietly stopped parsing would agree with a
/// working one on every line there is. The guard against reintroducing an
/// unselected profile would be retired and the suite would stay green about it,
/// which is this file's own failure mode turned on itself.
///
/// So the spellings a future editor might reach for are exercised directly,
/// each written the way it would appear in a workflow — and the two lines that
/// select nothing are pinned too, since reading a selection where there is none
/// would raise a false alarm on a correct pipeline.
#[test]
fn the_profile_reader_can_read_a_selection_at_all() {
    for (line, spelling) in [
        ("      NEXTEST_PROFILE: ci", "a job's environment"),
        (
            "        run: NEXTEST_PROFILE=ci cargo nextest run --no-fail-fast",
            "an inline variable",
        ),
        (
            "      - run: cargo nextest run --profile ci --locked --no-fail-fast",
            "the long flag",
        ),
        (
            "      - run: cargo nextest run --profile=ci --locked --no-fail-fast",
            "the long flag, joined",
        ),
        (
            "      - run: cargo nextest run -P ci --locked --no-fail-fast",
            "nextest's short flag",
        ),
    ] {
        assert_eq!(
            profile_named(line).as_deref(),
            Some("ci"),
            "{spelling} selects a profile and is read as selecting none"
        );
    }

    for (line, why) in [
        (
            "      - run: cargo nextest run --locked --no-fail-fast",
            "a bare invocation selects nothing",
        ),
        (
            "      - run: cargo mutants --test-tool nextest --profile release --no-shuffle",
            "`cargo mutants --profile` names cargo's build profile, not nextest's",
        ),
    ] {
        assert_eq!(profile_named(line), None, "{why}");
    }
}

/// The suite does not retry (#103).
///
/// This is the decision the ticket turned on, and it is a *value in a file*
/// rather than anything the code does — so an assertion is the only thing that
/// can hold it. `[profile.ci]` said `retries = 2` while `[profile.default]`
/// said "a test that only passes on retry is reported flaky, not green", and
/// the contradiction sat there for months precisely because no test ever read
/// either line.
///
/// Every profile, not only the one in force: a retrying profile that nothing
/// selects is what the test above rejects, and this one should not have to
/// wait its turn to say the same thing about the setting itself.
#[test]
fn no_nextest_profile_retries_a_failing_test() {
    for (name, settings) in nextest_profiles() {
        let retries = match settings.get("retries") {
            None => 0,
            Some(toml::Value::Integer(count)) => *count,
            // nextest also spells it `{ backoff = "exponential", count = 2 }`.
            // An unrecognised shape counts as *some*, so a spelling this test
            // has not met fails loudly rather than passing quietly.
            Some(table) => table
                .get("count")
                .and_then(toml::Value::as_integer)
                .unwrap_or(1),
        };
        assert_eq!(
            retries, 0,
            "profile `{name}` retries a failing test {retries} times, so a \
             flaky test reports green"
        );
    }
}

/// The suite's fail-fast policy is written once, on the command line (#103).
///
/// `[profile.ci]` said `fail-fast = false` while every step that runs the suite
/// passed `--no-fail-fast` by hand, so two places claimed the same thing — and
/// which of them was actually in force was invisible, because the profile was
/// never selected. Deleting either would have looked effective and changed
/// nothing.
///
/// The command line is the copy that survived, because it is the one a reader
/// of the workflow can see without opening another file. So no profile may set
/// `fail-fast`, and every step that runs the suite must say `--no-fail-fast`
/// itself: one failure on one architecture or one dialect should report every
/// other failure in the same push, not the first. `cargo mutants` is not one of
/// these steps — see [`runs_the_suite_directly`].
#[test]
fn the_suite_never_stops_at_the_first_failure_and_says_so_in_one_place() {
    assert!(
        !nextest_config().contains("fail-fast"),
        "{NEXTEST_CONFIG} sets fail-fast, which the workflows already pass on \
         the command line — so one of the two is dead text and neither reader \
         can tell which"
    );

    for (name, workflow) in workflows() {
        for (job, block) in jobs(&workflow) {
            for step in block.lines().filter(|line| runs_the_suite_directly(line)) {
                assert!(
                    step.contains("--no-fail-fast"),
                    "{name}: job `{job}` stops the suite at the first failure \
                     and hides every other one in the same push: {step:?}"
                );
            }
        }
    }
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

/// The real-S3 run is a real run (#35) — ADR-0009's storage half, and the same
/// trap as the Postgres one above wearing a different hat.
///
/// `tests/s3.rs` skips when `TEST_S3_ENDPOINT` is unset. That is the right
/// answer on a laptop and a silent, permanent skip in CI: a job that stands
/// MinIO or Garage up and never gets the endpoint to the suite pays for the
/// store and then goes on testing offline signing, exactly as before the ticket
/// — green, and proving nothing about a round trip.
///
/// Both stores are named because ADR-0002 ships against both: Garage is the
/// first-class recommendation, MinIO is the one every contributor already has.
#[test]
fn every_job_that_provisions_an_object_store_runs_the_real_s3_suite_against_it() {
    let bring_up = bring_up_command();
    let mut provisioned: Vec<&str> = Vec::new();
    for (job, block) in jobs(&ci_workflow()) {
        let lines: Vec<&str> = block.lines().collect();
        let Some(brought_up) = lines.iter().position(|line| line.contains(bring_up)) else {
            continue;
        };
        // *After* the bring-up, not merely somewhere in the same job: the
        // endpoint reaches the suite through `$GITHUB_ENV`, which only steps
        // that run later read. A suite that ran first would skip every one of
        // its tests and say nothing about it.
        let tested = lines
            .iter()
            .position(|line| runs_the_suite(line))
            .unwrap_or_else(|| panic!("`{job}` provisions an object store and runs no suite"));
        assert!(
            tested > brought_up,
            "`{job}` runs the suite before the store it provisions exists, so every \
             real-S3 test skips"
        );
        provisioned.extend(
            ["minio", "garage"]
                .into_iter()
                .filter(|store| block.contains(&format!("{bring_up} {store}"))),
        );
    }
    provisioned.sort_unstable();
    assert_eq!(
        provisioned,
        ["garage", "minio"],
        "ADR-0002's two S3 backends are not both exercised by the pipeline"
    );

    // Bringing the store up is only half the link. The harness reads the
    // endpoint from the *environment* (`tests/common/s3.rs`), and `$GITHUB_ENV`
    // is the one way a step's export reaches the step that runs cargo — so a
    // bring-up that stopped writing it would leave every real-S3 test skipping,
    // inside the job built to run them.
    let script = std::fs::read_to_string(repo_file(BRING_UP_PATH))
        .unwrap_or_else(|err| panic!("read {BRING_UP_PATH}: {err}"));
    for handoff in ["TEST_S3_ENDPOINT", "GITHUB_ENV"] {
        assert!(
            script.contains(handoff),
            "{BRING_UP_PATH} never mentions {handoff}, so the suite is never told \
             where the store it just started is"
        );
    }
}

/// The bring-up script the real-S3 jobs run.
const BRING_UP_PATH: &str = ".github/scripts/object-store-up.sh";

/// What a workflow step calls it — derived rather than written twice, because
/// two spellings of one path are two things that can drift apart.
fn bring_up_command() -> &'static str {
    BRING_UP_PATH
        .rsplit('/')
        .next()
        .expect("a path has a last segment")
}

/// Whether this step actually *runs* the suite.
///
/// `cargo ` with a space, because `taiki-e/install-action`'s `tool:
/// cargo-nextest` names the runner without running it — and it does so several
/// steps *before* anything is provisioned, which is enough to make an
/// order-of-steps assertion answer about the wrong line.
fn runs_the_suite(step: &str) -> bool {
    step.contains("cargo ") && step.contains("nextest")
}

/// Whether this step runs the suite *itself*, rather than handing nextest to
/// something else that drives it.
///
/// `cargo mutants --test-tool nextest` is the exception, and both halves of the
/// distinction matter: mutants owns the arguments it passes (so it is not ours
/// to require `--no-fail-fast` of — inside a single mutant, stopping at the
/// first failing test *is* the answer), and its own `--profile` names a cargo
/// build profile rather than a nextest one.
fn runs_the_suite_directly(step: &str) -> bool {
    runs_the_suite(step) && !step.contains("--test-tool")
}

/// nextest's own configuration, which every `cargo nextest` in the project
/// reads — the local loop's and the pipeline's alike.
const NEXTEST_CONFIG: &str = ".config/nextest.toml";

/// What nextest runs when a command line names no profile.
const DEFAULT_PROFILE: &str = "default";

/// [`NEXTEST_CONFIG`]'s settings, the commentary [stripped](without_comments).
fn nextest_config() -> String {
    let text = std::fs::read_to_string(repo_file(NEXTEST_CONFIG))
        .unwrap_or_else(|err| panic!("read {NEXTEST_CONFIG}: {err}"));
    without_comments(&text)
}

/// The `[profile.*]` table of [`NEXTEST_CONFIG`], each profile with its
/// settings.
///
/// Parsed rather than grepped, because `[profile.ci.junit]` is a *setting of*
/// `ci` and not a second profile — a line-matching answer would have to know
/// that, and would be wrong about `[profile."ci"]` besides. There is no
/// actionlint for TOML, and the parser is already a dependency.
fn nextest_profiles() -> toml::Table {
    let config: toml::Table = toml::from_str(&nextest_config())
        .unwrap_or_else(|err| panic!("{NEXTEST_CONFIG} is not TOML: {err}"));
    config
        .get("profile")
        .and_then(toml::Value::as_table)
        .unwrap_or_else(|| panic!("{NEXTEST_CONFIG} defines no profiles at all"))
        .clone()
}

/// Every step in the pipeline that runs the suite, paired with the nextest
/// profile it will actually run on: `(where it is, which profile)`.
///
/// The profile is resolved the way the runner resolves it — the narrowest
/// scope that names one wins, so a step's own flag beats its job's environment,
/// which beats the workflow's, and a call site that names none is on
/// [`DEFAULT_PROFILE`]. Answering per step rather than per workflow is what
/// makes "one job selects it and three do not" visible, which is the shape the
/// original failure had.
fn profiles_in_force() -> Vec<(String, String)> {
    let mut in_force = Vec::new();
    for (name, workflow) in workflows() {
        let preamble: String = workflow
            .lines()
            .take_while(|line| !line.starts_with("jobs:"))
            .collect::<Vec<_>>()
            .join("\n");
        for (job, block) in jobs(&workflow) {
            for step in block.lines().filter(|line| runs_the_suite_directly(line)) {
                let profile = profile_named(step)
                    .or_else(|| profile_named(&block))
                    .or_else(|| profile_named(&preamble))
                    .unwrap_or_else(|| DEFAULT_PROFILE.to_string());
                in_force.push((format!("{name}: job `{job}`"), profile));
            }
        }
    }
    in_force
}

/// The nextest profile this text selects, if it names one at all.
///
/// Two spellings reach nextest, and both are read because either is a
/// reasonable thing for a future editor to write. `NEXTEST_PROFILE` in the
/// environment covers every call site in its scope at a stroke, including the
/// ones that reach nextest through `cargo llvm-cov`; `--profile` (or nextest's
/// `-P`) is read only off a line that runs the suite itself, because cargo
/// spells its own *build* profile the same way. Either separator is accepted
/// after the flag, since `--profile=ci` and `--profile ci` are the same request
/// and a parser that understood only one would raise a false alarm about the
/// other.
fn profile_named(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let named = match line.split_once("NEXTEST_PROFILE") {
            Some((_, rest)) => rest,
            None if runs_the_suite_directly(line) => {
                line.split_once("--profile")
                    .or_else(|| line.split_once(" -P"))?
                    .1
            }
            None => return None,
        };
        Some(
            named
                .trim_start_matches([':', '=', ' '])
                .split_whitespace()
                .next()?
                .trim_matches(['\'', '"'])
                .to_string(),
        )
    })
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

/// The image name is lowercased before it becomes a tag.
///
/// A Docker repository name must be lowercase, and `github.repository` is
/// `FxllenCode/radio-scout` — capitals and all. Interpolating it straight into a
/// tag makes buildx refuse the build outright, so the image is never published
/// under the name `docs/deploy.md` tells operators to pull.
///
/// This cost a release to find: the `image` job only runs on a tag, so no amount
/// of green CI could have caught it, and `workflow_dispatch` publishes nothing.
/// It is pinned here because the tempting "simplification" is to hoist the name
/// back into `env:` as `ghcr.io/${{ github.repository }}` — which reads fine and
/// has never worked.
#[test]
fn the_image_name_is_lowercased_before_it_is_used_as_a_tag() {
    let release = release_workflow();

    assert!(
        !release.contains("ghcr.io/${{ github.repository }}"),
        "`github.repository` carries the owner's capitals; buildx refuses a tag \
         that is not lowercase"
    );
    assert!(
        release.contains("${GITHUB_REPOSITORY,,}"),
        "nothing lowercases the image name, so the published tag is a guess"
    );
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

/// A path inside the repository, from a test binary that may run anywhere.
fn repo_file(relative: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn named_workflow(name: &str) -> String {
    workflows()
        .into_iter()
        .find(|(found, _)| found == name)
        .unwrap_or_else(|| panic!("`.github/workflows/{name}` is missing"))
        .1
}

/// Every piece of shell the project ships is linted.
///
/// `actionlint` reads `.github/workflows` only, so shell that lives anywhere
/// else is shell nothing checks. Two files are shipped *to users* rather than
/// merely run by CI — `install.sh`, which the README invites people to pipe
/// into their shell, and `radio-scout-upload.sh` (#43), which runs on somebody
/// else's recorder after every call — and both are named explicitly in the
/// shellcheck step rather than swept up by a glob, so adding a third is a
/// decision instead of an accident.
#[test]
fn every_shell_script_the_project_ships_is_shellchecked() {
    let ci = workflows()
        .into_iter()
        .find(|(name, _)| name == "ci.yml")
        .map(|(_, text)| text)
        .expect("ci.yml");
    let step = ci
        .lines()
        .find(|line| line.contains("shellcheck "))
        .unwrap_or_else(|| panic!("ci.yml runs no shellcheck at all:\n{ci}"));

    for script in ["install.sh", "radio-scout-upload.sh"] {
        assert!(
            step.contains(script),
            "{script} is shipped to users and never linted: {step:?}"
        );
    }
}

/// The Trunk Recorder plugin (#44) is the release's one non-Rust artifact, and
/// nothing in `cargo nextest` can build it: it is a C++ shared object compiled
/// against a recorder's headers, and `tests/trplugin.rs` deliberately drives
/// only the half of it that has no Trunk Recorder in scope. So the job that
/// compiles the other half — the `Call_Data_t` shim, and the CMakeLists an
/// operator's own build reads — is the only thing standing between a rename in
/// Trunk Recorder and a plugin that no longer builds on anybody's recorder.
///
/// Pinned to a **commit**, not a branch: a job tracking `master` turns somebody
/// else's merge into our red build, and the failure it would report is the one
/// thing this job must never cry wolf about.
#[test]
fn the_trunk_recorder_plugin_is_compiled_against_a_pinned_recorder() {
    let ci = ci_workflow();
    let (name, block) = jobs(&ci)
        .into_iter()
        .find(|(_, block)| block.contains("robotastic/trunk-recorder"))
        .expect("no job builds the Trunk Recorder plugin, so nothing compiles its shim");

    assert!(
        block.contains("user_plugins"),
        "{name} must build the plugin the way an operator does — from `user_plugins/`"
    );
    assert!(
        block.contains("radio_scout_uploader"),
        "{name} checks out the recorder but never builds our target"
    );
    let pinned = block
        .lines()
        .find(|line| line.contains("ref:"))
        .unwrap_or_else(|| panic!("{name} does not pin a Trunk Recorder commit:\n{block}"));
    let sha: String = pinned
        .rsplit(':')
        .next()
        .expect("a ref")
        .trim()
        .trim_matches(|c| c == '\'' || c == '"')
        .into();
    assert!(
        sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()),
        "{name} pins {sha:?}, which is a branch or a tag rather than a commit"
    );
}
