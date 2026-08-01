//! Invariants of the operator-facing documentation.
//!
//! `tests/packaging.rs` exists because three files have to agree on one string
//! and nothing connects them. The docs are the fourth file in that agreement,
//! and the weakest link in it: `release.yml` is executed, `install.sh` is
//! executed, `config.rs` is compiled — and `README.md` is read by a person who
//! has no way to tell that it went stale. Documentation that lies is worse than
//! documentation that is missing, because the reader trusts it.
//!
//! So the claims a reader could *act* on are pinned here: the settings named,
//! the targets tabulated, the installer's URL, and every link and image. What is
//! deliberately not pinned is prose — this is a test about facts that drift, not
//! a style checker.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The repository root.
fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    let path = repo().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

/// Every operator-facing document. Deliberately not `docs/agents/**` (written
/// for a different reader) or `docs/adr/**` (a dated record of a decision, which
/// is *supposed* to keep saying what it said at the time).
const DOCS: &[&str] = &[
    "README.md",
    "docs/deploy.md",
    "docs/operating.md",
    "docs/using.md",
    "docs/recorders.md",
    "docs/migrating-from-rdio-scanner.md",
    // Not under `docs/`, because it ships *inside* the Trunk Recorder plugin
    // tarball (#44) — the one operator-facing document that travels away from
    // this repository, and therefore the one whose links nobody would otherwise
    // follow again.
    "plugins/trunk-recorder/README.md",
];

/// Every shipped file that *reads* a `RADIO_SCOUT_*` variable — the binary, and
/// `radio-scout-upload.sh` (#43), which runs on the recorder, where the binary
/// is usually not installed at all. A spelling documented for the script has to
/// be true in exactly the way one documented for the binary does.
///
/// `install.sh` is deliberately absent: it reads none of these, and listing a
/// file that contributes nothing would make the gate look broader than it is.
const READERS: &[&str] = &["src", "radio-scout-upload.sh"];

/// Every `RADIO_SCOUT_*` spelling something the project ships actually reads.
///
/// Taken from the source rather than from `--write-config`'s output, because a
/// variable the docs invent would not appear in either, and comparing the docs
/// against the thing that reads the environment is the only comparison that
/// answers "will this work if a reader types it".
///
/// A shell script is read through [`reads_in_shell`] rather than whole, because
/// its `--help` text names the variables too — and a name that appeared only in
/// help output would be a documented setting nothing reads, which is the exact
/// failure this gate exists to catch, smuggled in through the back door.
fn known_env_vars() -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for reader in READERS {
        let path = repo().join(reader);
        let files = if path.is_dir() {
            walk(&path)
        } else {
            vec![path]
        };
        for entry in files {
            let Ok(text) = std::fs::read_to_string(&entry) else {
                continue;
            };
            match entry.extension().and_then(|ext| ext.to_str()) {
                Some("rs") => found.extend(env_vars_in(&text)),
                Some("sh") => found.extend(reads_in_shell(&text)),
                _ => {}
            }
        }
    }
    assert!(
        found.len() > 20,
        "suspiciously few settings found: {found:?}"
    );
    found
}

/// The `RADIO_SCOUT_*` variables a shell script actually reads: the ones inside
/// a `${…}` expansion. Prose that merely names one — usage text, a comment —
/// does not count, which is the whole point.
fn reads_in_shell(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = text;
    while let Some(at) = rest.find("${RADIO_SCOUT_") {
        rest = &rest[at + 2..];
        found.extend(env_vars_in(&rest[..rest.find('}').unwrap_or(rest.len())]));
    }
    found
}

/// Every `RADIO_SCOUT_*` token in `text`.
fn env_vars_in(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = text;
    while let Some(at) = rest.find("RADIO_SCOUT_") {
        let tail = &rest[at..];
        let end = tail
            .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
            .unwrap_or(tail.len());
        // A trailing `_` is a fragment of prose (`_SECRET_ACCESS_KEY`), not a
        // name — `RADIO_SCOUT_S3_ACCESS_KEY_ID / _SECRET_ACCESS_KEY` is written
        // that way in more than one place.
        found.insert(tail[..end].trim_end_matches('_').to_string());
        rest = &tail[end..];
    }
    found
}

/// Files under `dir`, recursively.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// A setting the docs tell an operator to set has to be one something reads.
///
/// The failure this catches is a rename: `config.rs` moves a variable, the code
/// and its tests move with it, and the document telling somebody to export the
/// old name stays green forever — silently, because an unset variable is not an
/// error, it is a default.
#[test]
fn every_setting_the_docs_name_is_one_the_binary_reads() {
    let known = known_env_vars();
    for doc in DOCS {
        for named in env_vars_in(&read(doc)) {
            assert!(
                known.contains(&named),
                "{doc} tells the reader to set `{named}`, which nothing in {READERS:?} reads"
            );
        }
    }
}

/// The three credentials that are not settings.
///
/// First run **writes** each of these, so none has a TOML key, a default or a
/// place in `--write-config`'s output — they live in `.env` and nowhere else
/// (ADR-0012). They are named here rather than derived because that is exactly
/// what makes them different from everything in [`SETTINGS`]: a fourth appearing
/// in `.env.example` should have to be argued for in this list.
const WRITTEN_CREDENTIALS: &[&str] = &[
    "RADIO_SCOUT_API_KEY",
    "RADIO_SCOUT_ADMIN_PASSWORD",
    "RADIO_SCOUT_VAPID_PRIVATE_KEY",
];

/// ...and the one variable that selects a file rather than a setting inside one.
const CONFIG_FILE_VAR: &str = "RADIO_SCOUT_CONFIG";

/// **`.env.example` is the environment layer's reference, so it comes under the
/// settings table (#87).**
///
/// Both directions, and each catches a different lie. A setting missing from the
/// file is one an operator configuring a container has no way to discover —
/// `--write-config` documents the TOML spelling, and this file is the only place
/// the variable spelling is written down. A variable *in* the file that is not a
/// setting is worse: it reads as configuration and does nothing, which is
/// indistinguishable from a default until someone works out why their value is
/// being ignored.
///
/// Matched on the assignment (`NAME=`) rather than anywhere in the prose, so the
/// paragraphs above each block can go on explaining a setting without being
/// mistaken for one.
#[test]
fn the_example_environment_file_shows_exactly_the_settings_that_exist() {
    let text = read(".env.example");
    let assigned: BTreeSet<String> = text
        .lines()
        .map(|line| line.trim_start_matches('#').trim())
        .filter_map(|line| line.split_once('='))
        .map(|(name, _)| name.trim().to_string())
        .filter(|name| name.starts_with("RADIO_SCOUT_") || name == "RUST_LOG")
        .collect();

    for setting in radio_scout::config::SETTINGS {
        assert!(
            assigned.contains(setting.var),
            ".env.example never shows `{}` (the environment spelling of `{}`)",
            setting.var,
            setting.key
        );
    }

    let known: BTreeSet<&str> = radio_scout::config::SETTINGS
        .iter()
        .map(|setting| setting.var)
        .chain(WRITTEN_CREDENTIALS.iter().copied())
        .chain(std::iter::once(CONFIG_FILE_VAR))
        .collect();
    for name in &assigned {
        assert!(
            known.contains(name.as_str()),
            ".env.example tells the reader to set `{name}`, which no setting reads"
        );
    }
}

/// ...and every value it shows is one that setting would accept.
///
/// Deliberately not "equal to the settings table's example": the two are
/// answering different questions. The table's example must be a value that is
/// *not* the default, or it could not prove a variable is read at all; the file
/// mostly shows an operator the default, which is the more useful thing to see
/// beside a commented-out line. What must be true of both is that they work —
/// so this asks the setting itself, which is the only thing that knows.
///
/// The failure it catches is the ordinary one: a default changes, or a value
/// stops being legal (`database_level = "debug"` did, with ADR-0011 rule 5), and
/// the line an operator would have copied goes on sitting there.
#[test]
fn every_value_the_example_environment_file_shows_is_one_that_setting_accepts() {
    let text = read(".env.example");

    for setting in radio_scout::config::SETTINGS {
        let shown = text
            .lines()
            .map(|line| line.trim_start_matches('#').trim())
            .find_map(|line| line.strip_prefix(setting.var)?.strip_prefix('='))
            .unwrap_or_else(|| panic!("{} is never assigned a value", setting.var));
        if let Err(error) = setting.apply(&mut Default::default(), shown) {
            panic!(
                ".env.example shows `{}={shown}`, which is refused: {error}",
                setting.var
            );
        }
    }
}

/// The README's platform table and the release matrix name the same targets.
///
/// Both directions matter, and for different reasons: a target in the table that
/// is never built is a download link to a 404, and a target that is built but
/// undocumented is a platform whose users never learn it is supported.
#[test]
fn the_readme_documents_exactly_the_targets_that_are_released() {
    let released: BTreeSet<String> = read(".github/workflows/release.yml")
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            line.strip_prefix("- ")
                .unwrap_or(line)
                .strip_prefix("target: ")
                .map(str::to_string)
        })
        .collect();
    assert!(
        released.len() >= 4,
        "the release matrix should cover the Pi, both desktops and Windows: {released:?}"
    );

    let readme = read("README.md");
    let documented: BTreeSet<String> = released
        .iter()
        .filter(|target| readme.contains(*target))
        .cloned()
        .collect();

    assert_eq!(
        documented,
        released,
        "the README's platform table and the release matrix disagree; \
         missing from the README: {:?}",
        released.difference(&documented).collect::<Vec<_>>()
    );
}

/// The `curl … | sh` command points at an installer that exists, in this
/// repository.
///
/// It is the first thing a reader runs and the one command they cannot check
/// before running it, so the URL being right is not a nicety. `install.sh` knows
/// its own repository slug; the docs must agree with that rather than with a
/// slug somebody typed.
#[test]
fn the_documented_installer_url_points_at_this_repositorys_installer() {
    let installer = read("install.sh");
    let slug = installer
        .lines()
        .find_map(|line| line.trim().strip_prefix("REPO=\"")?.strip_suffix('"'))
        .expect("install.sh declares its own REPO");

    for doc in DOCS {
        let text = read(doc);
        for line in text.lines().filter(|line| line.contains("install.sh | sh")) {
            assert!(
                line.contains(&format!("/{slug}/")),
                "{doc} fetches the installer from a repository that is not `{slug}`:\n  {line}"
            );
            assert!(
                line.ends_with("install.sh | sh")
                    || line.contains("install.sh | sh -s --")
                    || line.contains("install.sh | sh\n"),
                "{doc}: the piped installer command looks malformed:\n  {line}"
            );
        }
    }
    assert!(
        repo().join("install.sh").is_file(),
        "the installer the docs fetch is not in this repository"
    );
}

/// Every relative link and image in the docs resolves to something on disk.
///
/// The cheapest kind of documentation rot and the most common: a file is
/// renamed, and every link to it becomes a 404 that nothing notices because
/// nothing follows links. Images matter doubly — a README whose screenshots are
/// broken boxes says something about the software it is not trying to say.
#[test]
fn every_relative_link_and_image_in_the_docs_resolves() {
    for doc in DOCS {
        let text = read(doc);
        let dir = repo().join(doc).parent().expect("a parent").to_path_buf();
        for target in link_targets(&text) {
            // Strip an anchor: `operating.md#retention` is a link to a file.
            let file = target.split('#').next().unwrap_or_default();
            if file.is_empty() {
                continue; // a pure in-page anchor
            }
            assert!(
                dir.join(file).exists(),
                "{doc} links to `{target}`, which does not exist"
            );
        }
    }
}

/// Relative Markdown link and HTML `src` targets in `text`, skipping anything
/// absolute.
fn link_targets(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (opener, closer) in [("](", ')'), ("src=\"", '"')] {
        let mut rest = text;
        while let Some(at) = rest.find(opener) {
            let tail = &rest[at + opener.len()..];
            let end = tail.find(closer).unwrap_or(tail.len());
            let target = &tail[..end];
            if !target.starts_with("http") && !target.starts_with('#') {
                found.push(target.to_string());
            }
            rest = &tail[end..];
        }
    }
    found
}
