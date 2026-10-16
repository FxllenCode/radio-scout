//! The nightly mutation sweep's scripts, run for real (#121).
//!
//! `nightly.yml` is a few jobs of YAML around two scripts, and the scripts are
//! where its decisions live: what a schedule tick dispatches, which commit a
//! sweep starts from, how many shards it needs, whether cargo-mutants' exit
//! status means the sweep finished, and what gets filed about it. A workflow
//! can only be run by pushing it, and these decide things a green run cannot
//! show — a sweep that quietly starts from the wrong commit is green — so they
//! are run here. The decisions are functions of their arguments; the filing
//! runs against a stand-in `gh` that keeps its issues in a directory, so what
//! is asserted is what the script *left behind*.
//!
//! Each script is executed the way the workflow executes it — by its path, not
//! handed to `bash` — so a script that lost its executable bit fails here
//! rather than on every runner at once.
//!
//! `tests/ci.rs` pins the other half: that the workflow routes through these.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use rstest::rstest;

fn repo_file(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

const SWEEP: &str = ".github/scripts/nightly-sweep.sh";
const REPORT: &str = ".github/scripts/nightly-report.sh";

/// Run `nightly-sweep.sh` with these arguments and this standard input.
fn sweep_with(args: &[&str], input: &str) -> Output {
    let mut child = Command::new(repo_file(SWEEP))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("run {SWEEP} (is it executable?): {err}"));
    use std::io::Write;
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait")
}

fn sweep(args: &[&str]) -> Output {
    sweep_with(args, "")
}

/// What a script printed, for an assertion that it succeeded and said this.
fn said(output: &Output) -> String {
    assert!(
        output.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

// -- What a schedule tick dispatches -----------------------------------------

/// The weekly full sweep's schedule, as `nightly.yml` spells it.
const FULL: &str = "17 4 * * 3";
/// The daily incremental sweep's.
const DAILY: &str = "17 7 * * *";

/// **One schedule tick, one decision** — and the day the full sweep runs, the
/// incremental tick stands down, because a full sweep is everything an
/// incremental one would have been and more. The day is read off the full
/// sweep's own schedule rather than written down a second time, so moving it
/// moves both.
#[rstest]
#[case::the_daily_tick(DAILY, FULL, "2", "incremental")]
#[case::the_weekly_tick(FULL, FULL, "3", "full")]
#[case::the_daily_tick_on_the_full_sweeps_day(DAILY, FULL, "3", "skip")]
// cron spells Sunday 0 (or 7); `date +%u` spells it 7.
#[case::a_sunday_full_sweep_as_cron_spells_it(DAILY, "17 4 * * 0", "7", "skip")]
#[case::a_sunday_full_sweep_spelled_seven(DAILY, "17 4 * * 7", "7", "skip")]
fn a_schedule_tick_dispatches_one_kind_of_sweep(
    #[case] fired: &str,
    #[case] full: &str,
    #[case] weekday: &str,
    #[case] expected: &str,
) {
    assert_eq!(
        said(&sweep(&["dispatch-mode", fired, full, weekday])),
        expected
    );
}

// -- Where a sweep starts ----------------------------------------------------

/// The green sweep runs on a branch, newest first, as `gh run list` reports
/// them: head commit, a tab, the run's name.
fn runs(lines: &[(&str, &str)]) -> String {
    lines
        .iter()
        .map(|(sha, title)| format!("{sha}\t{title}\n"))
        .collect()
}

/// **The baseline is read off the run history, and nothing else** — the last
/// green sweep of either kind is where the next incremental one starts, and the
/// last green *full* one is what a weekly tick compares against. A run whose
/// range was picked by hand is neither: it proves nothing about the commits
/// before the one it was told to start from, so trusting it would skip them.
#[rstest]
#[case::nothing_yet(&[], "- -")]
#[case::an_incremental_then_a_full(
    &[("c3", "Mutation sweep (incremental) of next"), ("c2", "Mutation sweep (full) of next")],
    "c3 c2"
)]
#[case::only_incrementals(
    &[("c3", "Mutation sweep (incremental) of next"), ("c1", "Mutation sweep (incremental) of next")],
    "c3 -"
)]
#[case::a_hand_picked_range_is_passed_over(
    &[
        ("c9", "Mutation sweep (incremental) of next from c5"),
        ("c4", "Mutation sweep (incremental) of next"),
    ],
    "c4 -"
)]
fn the_baseline_is_the_last_sweep_that_covered_everything_before_it(
    #[case] history: &[(&str, &str)],
    #[case] expected: &str,
) {
    assert_eq!(said(&sweep_with(&["baselines"], &runs(history))), expected);
}

/// **What a dispatched sweep actually does**: `<mode> <baseline> <why>`.
///
/// The cases are the ways a night goes. The two that matter most are the
/// skips — a sweep with nothing new to look at runs nothing, which is half of
/// what #121 was for — and the fall-backs to a full sweep, because an
/// incremental sweep with no trustworthy starting point would otherwise be one
/// that silently covers nothing.
#[rstest]
#[case::an_ordinary_night("incremental", "c5", "c3", "c1", "-", "yes", "incremental c3")]
#[case::nothing_landed_since("incremental", "c3", "c3", "c1", "-", "yes", "skip -")]
#[case::the_branch_has_never_been_swept("incremental", "c5", "-", "-", "-", "no", "full -")]
#[case::the_last_swept_commit_was_rewritten_away(
    "incremental",
    "c5",
    "c3",
    "c1",
    "-",
    "no",
    "full -"
)]
#[case::a_hand_picked_baseline("incremental", "c5", "c3", "c1", "c2", "yes", "incremental c2")]
#[case::the_weekly_full("full", "c5", "c5", "c1", "-", "yes", "full -")]
#[case::the_weekly_full_with_nothing_new("full", "c5", "c5", "c5", "-", "yes", "skip -")]
fn a_sweep_decides_how_much_to_mutate(
    #[case] requested: &str,
    #[case] head: &str,
    #[case] green: &str,
    #[case] full: &str,
    #[case] chosen: &str,
    #[case] known: &str,
    #[case] expected: &str,
) {
    let decided = said(&sweep(&[
        "decide", requested, head, green, full, chosen, known,
    ]));

    let (mode_and_baseline, why) = decided
        .match_indices(' ')
        .nth(1)
        .map(|(at, _)| decided.split_at(at))
        .expect("<mode> <baseline> <why>");
    assert_eq!(mode_and_baseline, expected, "{decided}");
    assert!(!why.trim().is_empty(), "every decision says why: {decided}");
}

/// A hand-picked baseline this checkout has never heard of is a typo, and a
/// sweep of a typo is not a sweep — it refuses rather than guessing.
#[test]
fn a_hand_picked_baseline_that_is_not_a_commit_is_refused() {
    let refused = sweep(&["decide", "incremental", "c5", "c3", "c1", "nope", "no"]);

    assert_eq!(
        refused.status.code(),
        Some(2),
        "a typo'd baseline: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
}

/// **The tickets a swept range names**, for the comment that reports what it
/// found: every `#N` in its commit subjects, once each, in order — so a
/// survivor arrives with the ticket that probably introduced it.
#[test]
fn a_range_names_the_tickets_in_its_commits() {
    let subjects = "fix(enhance): every Call length resamples (#99)\n\
                    test(merge): the two mutants the sweep could not kill (#45, #46)\n\
                    docs: a commit that names no ticket\n\
                    fix(ui): what review of #78 found (#78)\n";

    assert_eq!(said(&sweep_with(&["tickets"], subjects)), "#45 #46 #78 #99");
    assert_eq!(said(&sweep_with(&["tickets"], "chore: nothing\n")), "");
}

// -- How many shards ---------------------------------------------------------

/// **Shards are sized to the work, not fixed** — about 85 mutants each, the
/// number that keeps a shard well inside its 210-minute limit at roughly a
/// minute and a half a mutant. So a ticket's worth of change is one or two
/// shards and the whole codebase is sixty-odd, and GitHub's ceiling of 256 jobs
/// a matrix is where it stops. The count and the matrix come out of one
/// answer, so `--shard k/N` can never name an N the matrix does not have — a
/// mismatch would leave mutants that no shard was ever handed. Nothing to
/// mutate is no shards, which is what makes the plan skip.
#[rstest]
#[case::nothing(0, 0)]
#[case::one_mutant(1, 1)]
#[case::exactly_a_shard(85, 1)]
#[case::one_over(86, 2)]
#[case::a_ticket(200, 3)]
#[case::the_whole_of_next(5_420, 64)]
#[case::past_the_matrix_ceiling(100_000, 256)]
fn shards_are_sized_to_the_mutants(#[case] mutants: usize, #[case] expected: usize) {
    let sized = said(&sweep(&["size", &mutants.to_string()]));

    let (total, matrix) = sized.split_once(' ').expect("<total> <matrix>");
    let matrix: Vec<usize> = serde_json::from_str(matrix).expect("a JSON array, for the matrix");
    assert_eq!(total, expected.to_string());
    assert_eq!(matrix, (0..expected).collect::<Vec<_>>());
}

// -- Whether it finished -----------------------------------------------------

/// **Survivors are findings, not breakage.** cargo-mutants exits 2 when a
/// mutant survives — a finding, filed in an issue — and 3 when one hung the
/// suite until it was stopped, which is a catch rather than a survival. Either
/// way it tested everything it was given, so the run is green. Anything else —
/// a usage error, a baseline suite that was already red (4), a diff that does
/// not match the tree (5, 6), its own crash (70) — means the sweep did not
/// happen, and that is the only thing a red run should mean.
#[rstest]
#[case::all_caught("0", true)]
#[case::survivors("2", true)]
#[case::timeouts("3", true)]
#[case::usage("1", false)]
#[case::baseline_already_red("4", false)]
#[case::diff_does_not_match("5", false)]
#[case::diff_unreadable("6", false)]
#[case::internal_error("70", false)]
fn only_a_sweep_that_did_not_run_is_a_failure(#[case] status: &str, #[case] completed: bool) {
    assert_eq!(
        sweep(&["completed", status]).status.success(),
        completed,
        "cargo-mutants exit {status}"
    );
}

// -- What gets filed ---------------------------------------------------------

/// A GitHub of one repository, as far as the report script can tell: a fake
/// `gh` first on the PATH, keeping its issues in a directory.
struct Github {
    dir: tempfile::TempDir,
}

/// One issue, as the fake left it.
#[derive(Debug)]
struct Issue {
    title: String,
    body: String,
    open: bool,
    labels: Vec<String>,
    comments: Vec<String>,
}

impl Github {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).expect("bin");
        let gh = bin.join("gh");
        std::fs::copy(repo_file("tests/fixtures/fake-gh.sh"), &gh).expect("install fake gh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755))
                .expect("chmod gh");
        }
        Github { dir }
    }

    /// Run `nightly-report.sh` against this GitHub, as a sweep of `next` would:
    /// the context in the environment, where the workflow puts it.
    fn report(&self, env: &[(&str, &str)], args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.dir.path().join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(repo_file(REPORT))
            .args(args)
            .env("PATH", path)
            .env("FAKE_GH_STATE", self.dir.path().join("state"))
            .env("BRANCH", "next")
            .env("RUN", "https://runs/42")
            .env("RANGE", "aaa1111..bbb2222")
            .env("TICKETS", "#99 #121")
            .envs(env.iter().copied())
            .output()
            .unwrap_or_else(|err| panic!("run {REPORT} (is it executable?): {err}"))
    }

    /// A file of survivors, one per line, as cargo-mutants' `missed.txt` holds.
    fn file(&self, name: &str, lines: &[&str]) -> String {
        let file = self.dir.path().join(name);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        let mut text = lines.join("\n");
        if !lines.is_empty() {
            text.push('\n');
        }
        std::fs::write(&file, text).expect("write survivors");
        file.to_str().expect("utf-8 path").to_string()
    }

    /// Report survivors from a completed sweep.
    fn survivors(&self, mode: &str, found: &[&str]) {
        let file = self.file("found.txt", found);
        said(&self.report(&[("MODE", mode)], &["survivors", &file]));
    }

    fn issues(&self) -> Vec<Issue> {
        let root = self.dir.path().join("state/issues");
        let Ok(entries) = std::fs::read_dir(&root) else {
            return Vec::new();
        };
        let mut numbered: Vec<(u32, PathBuf)> = entries
            .map(|entry| entry.expect("entry").path())
            .map(|path| {
                let number = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.parse().ok())
                    .expect("issues are numbered");
                (number, path)
            })
            .collect();
        numbered.sort();
        let read = |dir: &Path, file: &str| {
            std::fs::read_to_string(dir.join(file)).unwrap_or_else(|err| panic!("{file}: {err}"))
        };
        numbered
            .into_iter()
            .map(|(_, dir)| Issue {
                title: read(&dir, "title"),
                body: read(&dir, "body"),
                open: read(&dir, "state").trim() == "open",
                labels: read(&dir, "labels").lines().map(str::to_string).collect(),
                comments: read(&dir, "comments")
                    .split('\u{1e}')
                    .map(str::trim)
                    .filter(|comment| !comment.is_empty())
                    .map(str::to_string)
                    .collect(),
            })
            .collect()
    }

    /// The one issue with this title, whatever its state.
    fn titled(&self, title: &str) -> Issue {
        let mut matching: Vec<Issue> = self
            .issues()
            .into_iter()
            .filter(|issue| issue.title == title)
            .collect();
        assert_eq!(matching.len(), 1, "one `{title}`: {:?}", self.issues());
        matching.remove(0)
    }

    fn has(&self, title: &str) -> bool {
        self.issues().iter().any(|issue| issue.title == title)
    }
}

const SURVIVORS: &str = "Mutation survivors on next";
const BROKEN: &str = "Mutation sweep broken on next";

/// **The first survivors open the issue, for a person to triage** — not for an
/// agent to pick up, because which survivors become a ticket and which are
/// equivalent and want a proof in `.cargo/mutants.toml` instead is a judgement
/// (#83 found one "known equivalent" survivor that was nothing of the sort).
/// And the comment says where they came from: the commits swept, and the
/// tickets those commits name.
#[test]
fn the_first_survivors_open_an_issue_for_triage() {
    let github = Github::new();

    github.survivors(
        "incremental",
        &[
            "src/enhance.rs:559:13: replace > with < in normalize",
            "src/tone/detect.rs:228:16: replace >= with < in peak",
        ],
    );

    let issue = github.titled(SURVIVORS);
    assert!(issue.open);
    assert!(
        issue.labels.contains(&"needs-triage".to_string()),
        "{issue:?}"
    );
    assert!(
        issue.labels.contains(&"area:testing".to_string()),
        "{issue:?}"
    );
    for survivor in ["replace > with < in normalize", "replace >= with < in peak"] {
        assert!(
            issue.body.contains(survivor),
            "{survivor} not listed: {}",
            issue.body
        );
    }
    assert_eq!(issue.comments.len(), 1, "{:?}", issue.comments);
    let comment = &issue.comments[0];
    for context in ["aaa1111..bbb2222", "#99", "#121", "https://runs/42"] {
        assert!(
            comment.contains(context),
            "{context} missing from {comment}"
        );
    }
}

/// **A survivor is reported once, however far its line moves.** A line number
/// is where a mutant was when it was found; the code around it moves every
/// ticket, and an issue that re-announced every survivor whose line shifted
/// would be noise within a week. Only the genuinely new one is said.
#[test]
fn a_known_survivor_whose_line_moved_is_not_reported_again() {
    let github = Github::new();
    github.survivors(
        "incremental",
        &["src/enhance.rs:559:13: replace > with < in normalize"],
    );

    github.survivors(
        "incremental",
        &[
            "src/enhance.rs:571:13: replace > with < in normalize",
            "src/quiet/detect.rs:88:9: replace * with + in floor",
        ],
    );

    let issue = github.titled(SURVIVORS);
    assert_eq!(
        issue.body.matches("replace > with < in normalize").count(),
        1,
        "listed twice: {}",
        issue.body
    );
    assert!(
        issue.body.contains("replace * with + in floor"),
        "{}",
        issue.body
    );
    assert_eq!(issue.comments.len(), 2);
    assert!(
        !issue.comments[1].contains("in normalize"),
        "re-announced a known survivor: {}",
        issue.comments[1]
    );
    assert!(
        issue.comments[1].contains("in floor"),
        "{}",
        issue.comments[1]
    );
}

/// ...but **two survivors are two**, even when they differ only in position.
/// A function with two `+` in it has two `replace + with -` mutants, and
/// telling a survivor that moved from one that is new cannot mean collapsing
/// them: each listed survivor accounts for one found one, an exact position
/// first, and only the leftovers are new.
#[test]
fn two_survivors_that_differ_only_in_position_are_two() {
    let github = Github::new();
    github.survivors("incremental", &["src/a.rs:10:5: replace + with - in foo"]);

    github.survivors(
        "incremental",
        &[
            "src/a.rs:10:5: replace + with - in foo",
            "src/a.rs:20:9: replace + with - in foo",
        ],
    );

    let issue = github.titled(SURVIVORS);
    assert_eq!(
        issue.body.matches("replace + with - in foo").count(),
        2,
        "{}",
        issue.body
    );
    assert!(
        issue.comments[1].contains("src/a.rs:20:9"),
        "{:?}",
        issue.comments
    );
    assert!(
        !issue.comments[1].contains("src/a.rs:10:5"),
        "{:?}",
        issue.comments
    );
}

/// A night that finds nothing new says nothing — not even "nothing new" — and a
/// night that finds nothing at all files nothing.
#[test]
fn an_incremental_sweep_with_nothing_new_says_nothing() {
    let github = Github::new();

    github.survivors("incremental", &[]);
    assert!(github.issues().is_empty(), "{:?}", github.issues());

    github.survivors(
        "incremental",
        &["src/enhance.rs:559:13: replace > with < in normalize"],
    );
    github.survivors(
        "incremental",
        &["src/enhance.rs:559:13: replace > with < in normalize"],
    );
    assert_eq!(github.titled(SURVIVORS).comments.len(), 1);
}

/// **A full sweep is the authoritative list.** An incremental sweep only sees
/// what changed, so it can only add; a full one sees everything, so whatever it
/// does not find is no longer surviving — killed by a test, or excluded with a
/// proof — and comes off the list.
#[test]
fn a_full_sweep_rewrites_the_list() {
    let github = Github::new();
    github.survivors(
        "incremental",
        &[
            "src/a.rs:1:1: replace + with - in killed_since",
            "src/b.rs:2:2: replace + with - in still_there",
        ],
    );

    github.survivors(
        "full",
        &[
            "src/b.rs:2:2: replace + with - in still_there",
            "src/c.rs:3:3: replace + with - in cross_file",
        ],
    );

    let issue = github.titled(SURVIVORS);
    assert!(issue.open);
    assert!(!issue.body.contains("killed_since"), "{}", issue.body);
    assert!(issue.body.contains("still_there"), "{}", issue.body);
    assert!(issue.body.contains("cross_file"), "{}", issue.body);
    let summary = issue
        .comments
        .last()
        .expect("a full sweep says what it found");
    for counted in ["2 survivors", "1 new", "1 no longer surviving"] {
        assert!(
            summary.contains(counted),
            "{counted} missing from {summary}"
        );
    }
}

/// ...and a full sweep that finds none closes the issue, which is the only way
/// it ever closes: nothing short of a sweep of everything can say that nothing
/// survives.
#[test]
fn a_full_sweep_that_finds_none_closes_the_issue() {
    let github = Github::new();
    github.survivors(
        "incremental",
        &["src/enhance.rs:559:13: replace > with < in normalize"],
    );

    github.survivors("incremental", &[]);
    assert!(
        github.titled(SURVIVORS).open,
        "an incremental sweep cannot close it"
    );

    github.survivors("full", &[]);
    let issue = github.titled(SURVIVORS);
    assert!(!issue.open, "{issue:?}");
    assert!(
        issue
            .comments
            .last()
            .is_some_and(|c| c.contains("https://runs/42")),
        "{:?}",
        issue.comments
    );
}

/// **A broken sweep is one issue, however many nights it stays broken** — a
/// separate one from the survivors, because one needs CI fixing and the other
/// needs tests writing, and each night after the first is a comment on it.
#[test]
fn a_broken_sweep_is_one_issue_and_a_comment_a_night() {
    let github = Github::new();

    said(&github.report(
        &[("RUN", "https://runs/1")],
        &["broken", "Mutants 7/64: cancelled"],
    ));
    said(&github.report(
        &[("RUN", "https://runs/2")],
        &["broken", "Mutants 3/64: cancelled"],
    ));

    let issue = github.titled(BROKEN);
    assert!(issue.open);
    assert!(
        issue.labels.contains(&"needs-triage".to_string()),
        "{issue:?}"
    );
    assert!(issue.body.contains("https://runs/1"), "{}", issue.body);
    assert!(
        issue.body.contains("Mutants 7/64: cancelled"),
        "{}",
        issue.body
    );
    assert_eq!(issue.comments.len(), 1, "{:?}", issue.comments);
    assert!(
        issue.comments[0].contains("https://runs/2"),
        "{:?}",
        issue.comments
    );
    assert!(!github.has(SURVIVORS), "breakage is not a survivor");
}

// -- The report job, whole ---------------------------------------------------

/// The shard results a report job downloads: `mutants-shard-<k>/missed.txt`.
fn shards(github: &Github, results: &[&[&str]]) -> String {
    for (shard, missed) in results.iter().enumerate() {
        github.file(&format!("shards/mutants-shard-{shard}/missed.txt"), missed);
    }
    let dir = github.dir.path().join("shards");
    std::fs::create_dir_all(&dir).expect("shards");
    dir.to_str().expect("utf-8").to_string()
}

/// **A completed sweep files what it found, then calls the breakage over** — in
/// that order, so that a filing that fails is itself filed as breakage rather
/// than happening just after the breakage issue was closed.
#[test]
fn a_completed_sweep_files_its_survivors_and_closes_the_breakage() {
    let github = Github::new();
    said(&github.report(
        &[("RUN", "https://runs/1")],
        &["broken", "a shard was cancelled"],
    ));
    let results = shards(
        &github,
        &[
            &["src/a.rs:1:1: replace + with - in one"],
            &[],
            &["src/b.rs:2:2: replace + with - in two"],
        ],
    );

    said(&github.report(
        &[("MODE", "incremental"), ("RUN", "https://runs/2")],
        &["sweep", "success", "success", "3", &results],
    ));

    let survivors = github.titled(SURVIVORS);
    assert!(survivors.body.contains("in one") && survivors.body.contains("in two"));
    let broken = github.titled(BROKEN);
    assert!(!broken.open, "{broken:?}");
    assert!(
        broken
            .comments
            .last()
            .is_some_and(|c| c.contains("https://runs/2")),
        "{:?}",
        broken.comments
    );
}

/// ...and a sweep that did not complete files breakage, **never survivors** —
/// a partial full sweep must not rewrite the list from half the shards, and the
/// run is red, so it is never anybody's baseline and the range is swept again.
#[test]
fn a_sweep_that_did_not_complete_files_breakage_not_survivors() {
    let github = Github::new();
    let results = shards(&github, &[&["src/a.rs:1:1: replace + with - in one"]]);

    let reported = github.report(
        &[
            ("MODE", "full"),
            ("FAILED_JOBS", "- Mutants 1/2: cancelled"),
        ],
        &["sweep", "success", "failure", "2", &results],
    );

    assert!(!reported.status.success(), "a broken sweep is a red run");
    let broken = github.titled(BROKEN);
    assert!(
        broken.body.contains("Mutants 1/2: cancelled"),
        "{}",
        broken.body
    );
    assert!(!github.has(SURVIVORS), "{:?}", github.issues());
}

/// **A shard whose results never arrived is a sweep that did not complete** —
/// even when every job says it succeeded. Read as "that shard found nothing",
/// a full sweep would close the survivors issue on a third of the evidence.
#[test]
fn a_missing_shards_results_are_breakage_not_an_all_clear() {
    let github = Github::new();
    github.survivors("incremental", &["src/a.rs:1:1: replace + with - in one"]);
    let results = shards(&github, &[&[]]);

    let reported = github.report(
        &[("MODE", "full")],
        &["sweep", "success", "success", "3", &results],
    );

    assert!(
        !reported.status.success(),
        "results went missing and the run was green"
    );
    assert!(github.titled(SURVIVORS).open, "closed on missing evidence");
    assert!(
        github.titled(BROKEN).body.contains("shard 1"),
        "{:?}",
        github.issues()
    );
}

/// When filing the survivors fails, that is filed — as breakage, because the
/// sweep's findings went nowhere, which from where anybody stands is the same
/// as the sweep not having run.
#[test]
fn a_filing_that_fails_is_filed_as_breakage() {
    let github = Github::new();
    let results = shards(&github, &[&["src/a.rs:1:1: replace + with - in one"]]);

    let reported = github.report(
        &[("MODE", "incremental"), ("FAKE_GH_FAIL", "comment")],
        &["sweep", "success", "success", "1", &results],
    );

    assert!(
        !reported.status.success(),
        "the filing failed and the run was green"
    );
    assert!(github.titled(BROKEN).open, "{:?}", github.issues());
}

/// A completed sweep with nothing broken behind it touches nothing it has no
/// business with.
#[test]
fn a_completed_sweep_with_nothing_to_report_files_nothing() {
    let github = Github::new();
    let results = shards(&github, &[&[], &[]]);

    said(&github.report(
        &[("MODE", "incremental")],
        &["sweep", "success", "success", "2", &results],
    ));

    assert!(github.issues().is_empty(), "{:?}", github.issues());
}
