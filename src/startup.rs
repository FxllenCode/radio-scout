//! First-boot provisioning of the ingest key, and what boot says about it.
//!
//! Zero-config install means a scanner that has never been configured still
//! comes up with a working ingest credential (ADR-0008). Until #17 lands real
//! config, that credential is `RADIO_SCOUT_API_KEY` — from the environment or
//! the `.env` beside the binary — and it is registered on every boot, so a
//! recorder's key survives both a restart and a wiped database.
//!
//! The interesting case is the *first* boot with nothing configured. It used to
//! generate a key and print it to stdout, which ADR-0011 rule 2 now forbids: a
//! secret is never logged, at any level, in any form — startup output gets
//! pasted into issues and (once #29 lands the operator log surface) written to a
//! database. So the generated key goes where the operator's key was always meant
//! to live — `.env` — and the log line carries only the path. That is strictly
//! more recoverable than the banner it replaces, which was gone with the
//! scrollback.
//!
//! This lives in the library rather than `main.rs` because it is the one part of
//! bootstrap with rules worth testing: a key must never leak into a log line,
//! and a key the operator can't read must never be registered.

use std::fs::OpenOptions;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

use sea_orm::ConnectionTrait;
use sea_orm::DbErr;
use tracing::{error, info};

use crate::db::repo;

/// The environment variable — and `.env` key — that pins the ingest key.
pub const INGEST_KEY_VAR: &str = "RADIO_SCOUT_API_KEY";

/// The label a generated key is stored under, so an operator listing keys (#19)
/// can tell "the one the server made for me" from one they configured.
const GENERATED_LABEL: &str = "default (first run)";

/// What booting did about the ingest key.
#[derive(Debug)]
pub enum IngestKey {
    /// A configured key that wasn't in the database yet.
    Registered,
    /// A configured key that an earlier boot already registered. Re-registering
    /// never revives a key an operator disabled (ADR-0008).
    AlreadyKnown,
    /// First run with nothing configured: a key was generated and written to
    /// `env_file`.
    Generated { env_file: PathBuf },
    /// First run, but `env_file` could not be written — so **no key was
    /// registered**. A credential nobody can read is worse than none at all;
    /// leaving the database empty means the next boot tries again instead of
    /// locking the operator out behind a key only the server ever saw.
    NotPersisted { env_file: PathBuf, error: io::Error },
    /// Nothing configured, but the database already holds keys.
    AlreadyProvisioned { keys: u64 },
}

/// Make sure the server boots with an ingest key, and report what that took.
///
/// `configured` is the raw `RADIO_SCOUT_API_KEY` value (blank counts as unset);
/// `env_file` is where a generated key would be written.
pub async fn provision_ingest_key<C: ConnectionTrait>(
    db: &C,
    configured: Option<&str>,
    env_file: &Path,
    now_ms: i64,
) -> Result<IngestKey, DbErr> {
    if let Some(key) = configured.map(str::trim).filter(|key| !key.is_empty()) {
        return Ok(match repo::ensure_api_key(db, key, None, now_ms).await? {
            true => IngestKey::Registered,
            false => IngestKey::AlreadyKnown,
        });
    }

    let keys = repo::count_api_keys(db).await?;
    if keys > 0 {
        return Ok(IngestKey::AlreadyProvisioned { keys });
    }

    let key = uuid::Uuid::new_v4().simple().to_string();
    let env_file = env_file.to_path_buf();
    // Persist *before* registering: if the write fails, the row must not exist,
    // or the next boot would see a provisioned database and never generate the
    // key the operator can actually use.
    match persist_ingest_key(&env_file, &key) {
        Ok(()) => {
            repo::create_api_key(db, &key, None, Some(GENERATED_LABEL.to_string()), now_ms).await?;
            Ok(IngestKey::Generated { env_file })
        }
        Err(error) => Ok(IngestKey::NotPersisted { env_file, error }),
    }
}

/// Say what boot did about the ingest key — never *what* the key is.
pub fn log_ingest_key(outcome: &IngestKey) {
    match outcome {
        IngestKey::Registered => info!(source = INGEST_KEY_VAR, "ingest key registered"),
        IngestKey::AlreadyKnown => {
            info!(source = INGEST_KEY_VAR, "ingest key already registered");
        }
        IngestKey::Generated { env_file } => {
            let env_file = env_file.display();
            info!(
                %env_file,
                var = INGEST_KEY_VAR,
                "no ingest key configured; generated one and wrote it to the env file"
            );
        }
        IngestKey::NotPersisted { env_file, error } => {
            let env_file = env_file.display();
            error!(
                %env_file,
                var = INGEST_KEY_VAR,
                %error,
                "could not save a generated ingest key; none was registered — set it yourself and restart"
            );
        }
        IngestKey::AlreadyProvisioned { keys } => {
            info!(keys, "ingest keys already provisioned");
        }
    }
}

/// Write `key` into the env file at `path`, creating it if absent.
///
/// A file we create is created `0600` — it holds a credential, and the mode is
/// set at creation rather than after, so the secret is never briefly readable by
/// anyone else. A file that already exists keeps whatever mode the operator gave
/// it; tightening someone else's file is not ours to do.
fn persist_ingest_key(path: &Path, key: &str) -> io::Result<()> {
    match std::fs::read_to_string(path) {
        Ok(existing) => std::fs::write(path, env_text_with_ingest_key(&existing, key)),
        Err(err) if err.kind() == ErrorKind::NotFound => {
            create_private_file(path)?.write_all(env_text_with_ingest_key("", key).as_bytes())
        }
        Err(err) => Err(err),
    }
}

/// Create `path`, owner-readable only where the platform can say so.
fn create_private_file(path: &Path) -> io::Result<std::fs::File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Splice `RADIO_SCOUT_API_KEY=<key>` into the text of an env file: replace the
/// existing assignment if there is one, otherwise append.
///
/// Everything else in the file is preserved byte for byte — an operator's other
/// settings, their comments and their line endings — because this rewrites a
/// file we don't own. A commented-out `# RADIO_SCOUT_API_KEY=` line is a comment,
/// not an assignment, and is left where it is.
fn env_text_with_ingest_key(text: &str, key: &str) -> String {
    let assignment = format!("{INGEST_KEY_VAR}={key}");
    let mut out = String::with_capacity(text.len() + assignment.len() + 1);
    let mut replaced = false;

    for line in text.split_inclusive('\n') {
        if !replaced && is_ingest_key_assignment(line) {
            out.push_str(&assignment);
            out.push('\n');
            replaced = true;
        } else {
            out.push_str(line);
        }
    }

    if !replaced {
        // A file whose last line has no newline would otherwise swallow the
        // assignment into it.
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&assignment);
        out.push('\n');
    }
    out
}

/// Whether `line` assigns the ingest key. Leading whitespace is tolerated;
/// a comment is not an assignment, and neither is a longer name that merely
/// starts the same way.
fn is_ingest_key_assignment(line: &str) -> bool {
    line.trim_start().starts_with(&format!("{INGEST_KEY_VAR}="))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::testing::LogCapture;
    use rstest::rstest;
    use sea_orm::DatabaseConnection;

    /// A key an assertion can hunt for in log output.
    const SECRET: &str = "s3cr3t-ingest-key-do-not-log";

    const NOW: i64 = 1_000_000_000_000;

    /// A fresh database with no keys, plus the temp dir holding it (and standing
    /// in for the directory an env file is written to).
    async fn empty_db() -> (DatabaseConnection, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = db::connect(&crate::testing::sqlite_url(&tmp))
            .await
            .expect("db");
        (db, tmp)
    }

    /// The key `.env` ended up pinning, so a test can prove the operator can
    /// actually read back what was generated.
    fn key_in(env_file: &Path) -> String {
        std::fs::read_to_string(env_file)
            .expect("env file")
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{INGEST_KEY_VAR}=")))
            .expect("an ingest key assignment")
            .to_string()
    }

    /// The rule the whole module exists for: whatever boot does about the key,
    /// the key itself never reaches a log line — at any level.
    #[tokio::test]
    async fn a_configured_key_is_never_logged() {
        let (db, tmp) = empty_db().await;
        let env_file = tmp.path().join(".env");
        let capture = LogCapture::start();

        // Both boots with the same configured key: the one that registers it,
        // and the one that finds it already known.
        for _ in 0..2 {
            let outcome = provision_ingest_key(&db, Some(SECRET), &env_file, NOW)
                .await
                .expect("provision");
            log_ingest_key(&outcome);
        }

        capture.assert_never_logged(SECRET);
        assert!(capture.text().contains("ingest key registered"));
        assert!(capture.text().contains("ingest key already registered"));
    }

    /// Same rule for the key the server invents — the one case where the secret
    /// is *ours*, and the one the old `println!` banner leaked.
    #[tokio::test]
    async fn a_generated_key_is_written_to_the_env_file_and_never_logged() {
        let (db, tmp) = empty_db().await;
        let env_file = tmp.path().join(".env");
        let capture = LogCapture::start();

        let outcome = provision_ingest_key(&db, None, &env_file, NOW)
            .await
            .expect("provision");
        log_ingest_key(&outcome);

        let generated = key_in(&env_file);
        assert!(!generated.is_empty(), "a key should have been written");
        capture.assert_never_logged(&generated);
        // The operator is told where to look, which is the whole point.
        let logged = capture.text();
        assert!(logged.contains(".env"), "{logged}");
        assert!(
            matches!(outcome, IngestKey::Generated { .. }),
            "{outcome:?}"
        );
        // ...and the key that was written is the key that works.
        assert!(
            repo::authorize_ingest(&db, &generated, 1)
                .await
                .expect("auth"),
            "the key in the env file must be the registered one"
        );
    }

    /// A generated key is written `0600`: it is a credential sitting in the
    /// operator's working directory.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_created_env_file_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let (db, tmp) = empty_db().await;
        let env_file = tmp.path().join(".env");

        provision_ingest_key(&db, None, &env_file, NOW)
            .await
            .expect("provision");

        let mode = std::fs::metadata(&env_file)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
    }

    /// If the key can't be saved, it must not be registered either: a key only
    /// the server ever saw would lock the operator out of their own scanner,
    /// and the empty database is what makes the next boot try again.
    #[tokio::test]
    async fn a_key_that_cannot_be_saved_is_not_registered() {
        let (db, tmp) = empty_db().await;
        // A directory is not a writable env file.
        let env_file = tmp.path().join("not-a-file");
        std::fs::create_dir(&env_file).expect("mkdir");
        let capture = LogCapture::start();

        let outcome = provision_ingest_key(&db, None, &env_file, NOW)
            .await
            .expect("provision");
        log_ingest_key(&outcome);

        assert!(
            matches!(outcome, IngestKey::NotPersisted { .. }),
            "{outcome:?}"
        );
        assert_eq!(
            repo::count_api_keys(&db).await.expect("count"),
            0,
            "an unreadable key must leave the database untouched"
        );
        // ERROR, because an operator has to do something about it.
        let logged = capture.text();
        assert!(logged.contains("ERROR"), "{logged}");
        assert!(logged.contains(INGEST_KEY_VAR), "{logged}");
    }

    /// A boot that finds keys already there generates nothing and touches no
    /// file — and says how many it found, so "is my key still live?" has an
    /// answer without a shell.
    #[tokio::test]
    async fn an_already_provisioned_database_is_left_alone() {
        let (db, tmp) = empty_db().await;
        let env_file = tmp.path().join(".env");
        repo::create_api_key(&db, SECRET, None, None, NOW)
            .await
            .expect("seed");
        let capture = LogCapture::start();

        let outcome = provision_ingest_key(&db, None, &env_file, NOW)
            .await
            .expect("provision");
        log_ingest_key(&outcome);

        assert!(
            matches!(outcome, IngestKey::AlreadyProvisioned { keys: 1 }),
            "{outcome:?}"
        );
        assert!(!env_file.exists(), "no env file should have been written");
        capture.assert_never_logged(SECRET);
        assert!(capture.text().contains("keys=1"), "{}", capture.text());
    }

    /// A blank or whitespace-only value is what `RADIO_SCOUT_API_KEY=` in an env
    /// file produces. It means "unset", not "register the empty key".
    #[rstest]
    #[case(None)]
    #[case(Some(""))]
    #[case(Some("   "))]
    #[tokio::test]
    async fn a_blank_configured_key_means_unset(#[case] configured: Option<&str>) {
        let (db, tmp) = empty_db().await;
        let env_file = tmp.path().join(".env");

        let outcome = provision_ingest_key(&db, configured, &env_file, NOW)
            .await
            .expect("provision");

        assert!(
            matches!(outcome, IngestKey::Generated { .. }),
            "{outcome:?}"
        );
        assert!(!key_in(&env_file).is_empty());
    }

    /// Surrounding whitespace on a configured key is trimmed — env files collect
    /// trailing spaces, and a recorder's key must still authorize.
    #[tokio::test]
    async fn a_configured_key_is_trimmed() {
        let (db, tmp) = empty_db().await;
        let env_file = tmp.path().join(".env");

        provision_ingest_key(&db, Some(&format!("  {SECRET}\t")), &env_file, NOW)
            .await
            .expect("provision");

        assert!(repo::authorize_ingest(&db, SECRET, 1).await.expect("auth"));
    }

    /// Rewriting a file we don't own: the operator's other settings, comments and
    /// line endings survive, and there is exactly one assignment afterwards.
    #[rstest]
    // An empty (or missing) file just gets the assignment.
    #[case("", "RADIO_SCOUT_API_KEY=k\n")]
    // Appended, with the existing content untouched.
    #[case(
        "RADIO_SCOUT_PORT=3000\n",
        "RADIO_SCOUT_PORT=3000\nRADIO_SCOUT_API_KEY=k\n"
    )]
    // A file with no trailing newline doesn't swallow the assignment.
    #[case(
        "RADIO_SCOUT_PORT=3000",
        "RADIO_SCOUT_PORT=3000\nRADIO_SCOUT_API_KEY=k\n"
    )]
    // An existing (blank) assignment is replaced in place, not duplicated —
    // dotenvy takes the first occurrence, so appending a second would do nothing.
    #[case(
        "RADIO_SCOUT_API_KEY=\nRADIO_SCOUT_PORT=3000\n",
        "RADIO_SCOUT_API_KEY=k\nRADIO_SCOUT_PORT=3000\n"
    )]
    #[case(
        "A=1\nRADIO_SCOUT_API_KEY=old\nB=2\n",
        "A=1\nRADIO_SCOUT_API_KEY=k\nB=2\n"
    )]
    // Leading whitespace still assigns.
    #[case("  RADIO_SCOUT_API_KEY=old\n", "RADIO_SCOUT_API_KEY=k\n")]
    // A comment is not an assignment; it stays exactly where the operator put it.
    #[case(
        "# RADIO_SCOUT_API_KEY=old\n",
        "# RADIO_SCOUT_API_KEY=old\nRADIO_SCOUT_API_KEY=k\n"
    )]
    // Neither is a longer name that merely starts the same way.
    #[case(
        "RADIO_SCOUT_API_KEY_OLD=x\n",
        "RADIO_SCOUT_API_KEY_OLD=x\nRADIO_SCOUT_API_KEY=k\n"
    )]
    // CRLF line endings are preserved on the lines we don't touch.
    #[case("A=1\r\n", "A=1\r\nRADIO_SCOUT_API_KEY=k\n")]
    // Blank lines and comments are structure, not noise.
    #[case("# settings\n\nA=1\n", "# settings\n\nA=1\nRADIO_SCOUT_API_KEY=k\n")]
    fn env_file_is_rewritten_in_place(#[case] before: &str, #[case] expected: &str) {
        assert_eq!(env_text_with_ingest_key(before, "k"), expected);
    }

    /// Writing over an existing env file keeps the operator's settings and pins
    /// the generated key — end to end, through the filesystem.
    #[tokio::test]
    async fn an_existing_env_file_keeps_its_other_settings() {
        let (db, tmp) = empty_db().await;
        let env_file = tmp.path().join(".env");
        std::fs::write(&env_file, "RADIO_SCOUT_PORT=8080\n").expect("seed env");

        provision_ingest_key(&db, None, &env_file, NOW)
            .await
            .expect("provision");

        let text = std::fs::read_to_string(&env_file).expect("env file");
        assert!(text.contains("RADIO_SCOUT_PORT=8080"), "{text}");
        assert!(
            repo::authorize_ingest(&db, &key_in(&env_file), 1)
                .await
                .expect("auth")
        );
    }

    proptest::proptest! {
        /// However odd the file, the rewrite leaves exactly one uncommented
        /// assignment of the key, and never loses a line the operator wrote.
        #[test]
        fn rewrite_leaves_exactly_one_assignment(
            before in proptest::collection::vec("(#)?[A-Z_]{0,8}=?[a-z0-9]{0,8}", 0..6),
            key in "[a-z0-9]{1,32}",
        ) {
            let before = before.join("\n");
            let after = env_text_with_ingest_key(&before, &key);
            let assignments = after.lines().filter(|line| is_ingest_key_assignment(line)).count();
            proptest::prop_assert_eq!(assignments, 1, "in:\n{}", after);
            proptest::prop_assert!(after.ends_with('\n'));
            proptest::prop_assert!(
                after.contains(&format!("{INGEST_KEY_VAR}={key}")),
                "key missing from:\n{}", after
            );
        }
    }
}
