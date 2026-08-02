//! First-boot provisioning of the two credentials, and what boot says about
//! them.
//!
//! Zero-config install means a scanner that has never been configured still
//! comes up with a working ingest credential *and* a gated admin surface
//! (ADR-0008). Both live in the same place and for the same reason — they are
//! the only two settings that stay out of `radio-scout.toml` (#17, ADR-0012),
//! because first run **writes** them:
//!
//! - **`RADIO_SCOUT_API_KEY`** ([`provision_ingest_key`]) is registered on every
//!   boot, so a recorder's key survives both a restart and a wiped database.
//! - **`RADIO_SCOUT_ADMIN_PASSWORD`** ([`provision_admin_password`], #19) is
//!   read on every boot; there is nothing in the database to check it against,
//!   because the environment *is* where it lives. rdio-scanner instead ships a
//!   known default password and nags until it is changed, so a fresh instance is
//!   open to anyone who has read its source.
//!
//! Both go through the same [`persist`] splice, so writing the second never eats
//! the first — nor any other setting, comment or line ending in a file we do not
//! own.
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
//! bootstrap with rules worth testing: a credential must never leak into a log
//! line, and a credential the operator can't read must never be put into
//! service — an unreadable ingest key leaves the database untouched so the next
//! boot tries again, and an unreadable admin password leaves the admin surface
//! shut rather than open on something only the server ever saw.

use std::fs::OpenOptions;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

use sea_orm::ConnectionTrait;
use sea_orm::DbErr;
use tracing::{error, info};

use crate::db::repo;
use crate::webpush::VapidKey;

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
    match persist(&env_file, INGEST_KEY_VAR, &key) {
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

/// The environment variable — and `.env` key — that gates the admin surface
/// (ADR-0008, spec US 38).
pub const ADMIN_PASSWORD_VAR: &str = "RADIO_SCOUT_ADMIN_PASSWORD";

/// What booting did about the admin password, and the password it settled on.
///
/// The secret rides in the variant, so `Debug` is written by hand rather than
/// derived: this type is exactly the kind of thing that ends up in a `?` chain
/// on an ERROR line, and ADR-0011 rule 2 has no exception for that.
pub enum AdminPassword {
    /// The operator's own password, from the environment or `.env`.
    Configured(String),
    /// First run with nothing configured: one was generated and written to
    /// `env_file`.
    Generated { password: String, env_file: PathBuf },
    /// First run, but `env_file` could not be written — so **no password is
    /// set** and the admin surface stays closed. A credential only the server
    /// ever saw would lock the operator out of their own configuration while
    /// leaving them convinced they had one.
    NotPersisted { env_file: PathBuf, error: io::Error },
}

impl std::fmt::Debug for AdminPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdminPassword::Configured(_) => f.write_str("Configured(<redacted>)"),
            AdminPassword::Generated { env_file, .. } => f
                .debug_struct("Generated")
                .field("password", &"<redacted>")
                .field("env_file", env_file)
                .finish(),
            AdminPassword::NotPersisted { env_file, error } => f
                .debug_struct("NotPersisted")
                .field("env_file", env_file)
                .field("error", error)
                .finish(),
        }
    }
}

impl AdminPassword {
    /// The password to gate the admin surface with, or `None` to leave it shut.
    pub fn password(&self) -> Option<&str> {
        match self {
            AdminPassword::Configured(password) => Some(password),
            AdminPassword::Generated { password, .. } => Some(password),
            AdminPassword::NotPersisted { .. } => None,
        }
    }
}

/// Make sure the admin surface has a password, and report what that took.
///
/// The shape is the ingest key's, for the same reasons: `configured` is the raw
/// `RADIO_SCOUT_ADMIN_PASSWORD` (blank counts as unset), and with nothing
/// configured a random one is generated and written to `env_file` — never
/// logged. Unlike the ingest key there is nothing in the database to check
/// against, because this credential is only ever read from the environment; the
/// file *is* where it lives.
///
/// rdio-scanner instead ships a known default (`rdio-scanner`) and sets a
/// `passwordNeedChange` flag, so a fresh instance is open to anyone who has read
/// its source until somebody notices the prompt.
pub fn provision_admin_password(configured: Option<&str>, env_file: &Path) -> AdminPassword {
    if let Some(password) = configured.map(str::trim).filter(|value| !value.is_empty()) {
        return AdminPassword::Configured(password.to_string());
    }

    let password = uuid::Uuid::new_v4().simple().to_string();
    let env_file = env_file.to_path_buf();
    match persist(&env_file, ADMIN_PASSWORD_VAR, &password) {
        Ok(()) => AdminPassword::Generated { password, env_file },
        Err(error) => AdminPassword::NotPersisted { env_file, error },
    }
}

/// Say what boot did about the admin password — never what it is.
pub fn log_admin_password(outcome: &AdminPassword) {
    match outcome {
        AdminPassword::Configured(_) => {
            info!(source = ADMIN_PASSWORD_VAR, "admin password configured");
        }
        AdminPassword::Generated { env_file, .. } => {
            let env_file = env_file.display();
            info!(
                %env_file,
                var = ADMIN_PASSWORD_VAR,
                "no admin password configured; generated one and wrote it to the env file"
            );
        }
        AdminPassword::NotPersisted { env_file, error } => {
            let env_file = env_file.display();
            error!(
                %env_file,
                var = ADMIN_PASSWORD_VAR,
                %error,
                "could not save a generated admin password; the admin surface is closed — set one yourself and restart"
            );
        }
    }
}

/// The environment variable — and `.env` key — holding the server's VAPID
/// identity (#16, ADR-0005).
pub const VAPID_KEY_VAR: &str = "RADIO_SCOUT_VAPID_PRIVATE_KEY";

/// What booting did about the Web Push identity.
///
/// The third credential with this shape, and for the third time the same
/// reason: first run **writes** it, so it lives in the environment and `.env`
/// rather than in a file `--write-config` generates.
///
/// One thing is different. An identity that cannot be read back is not merely
/// unusable — a new one every boot would silently invalidate every subscription
/// a browser had already pinned to the old public key, and a listener would
/// stop being notified with nothing anywhere saying why. So a key that cannot
/// be saved, or one that cannot be parsed, leaves push **off** and says so,
/// rather than running on an identity that will not survive the restart.
pub enum Vapid {
    /// The operator's own key, from the environment or `.env`.
    Configured(VapidKey),
    /// First run with nothing configured: one was generated and written to
    /// `env_file`.
    Generated { key: VapidKey, env_file: PathBuf },
    /// `env_file` could not be written, so no identity is in service.
    NotPersisted { env_file: PathBuf, error: io::Error },
    /// A configured value that is not a P-256 private key.
    Invalid,
}

impl std::fmt::Debug for Vapid {
    /// The key redacts itself; this exists so the enum around it does too.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Vapid::Configured(_) => f.write_str("Configured(<redacted>)"),
            Vapid::Generated { env_file, .. } => f
                .debug_struct("Generated")
                .field("key", &"<redacted>")
                .field("env_file", env_file)
                .finish(),
            Vapid::NotPersisted { env_file, error } => f
                .debug_struct("NotPersisted")
                .field("env_file", env_file)
                .field("error", error)
                .finish(),
            Vapid::Invalid => f.write_str("Invalid"),
        }
    }
}

impl Vapid {
    /// The identity to send notifications with, or `None` to leave Web Push
    /// off.
    pub fn key(self) -> Option<VapidKey> {
        match self {
            Vapid::Configured(key) => Some(key),
            Vapid::Generated { key, .. } => Some(key),
            Vapid::NotPersisted { .. } | Vapid::Invalid => None,
        }
    }
}

/// Make sure the server has a Web Push identity, and report what that took.
pub fn provision_vapid_key(configured: Option<&str>, env_file: &Path) -> Vapid {
    if let Some(text) = configured.map(str::trim).filter(|value| !value.is_empty()) {
        return match VapidKey::parse(text) {
            Ok(key) => Vapid::Configured(key),
            Err(_) => Vapid::Invalid,
        };
    }

    let key = VapidKey::generate();
    let env_file = env_file.to_path_buf();
    match persist(&env_file, VAPID_KEY_VAR, &key.secret_base64url()) {
        Ok(()) => Vapid::Generated { key, env_file },
        Err(error) => Vapid::NotPersisted { env_file, error },
    }
}

/// Say what boot did about the Web Push identity — never what the key is. The
/// **public** half is logged when there is one: it is not a secret, it is what
/// a browser pins, and an operator debugging a subscription needs to be able to
/// tell which identity is in service.
pub fn log_vapid_key(outcome: &Vapid) {
    match outcome {
        Vapid::Configured(key) => info!(
            source = VAPID_KEY_VAR,
            public_key = %key.public_base64url(),
            "web push identity configured"
        ),
        Vapid::Generated { key, env_file } => {
            let env_file = env_file.display();
            info!(
                %env_file,
                var = VAPID_KEY_VAR,
                public_key = %key.public_base64url(),
                "no web push identity configured; generated one and wrote it to the env file"
            );
        }
        Vapid::NotPersisted { env_file, error } => {
            let env_file = env_file.display();
            error!(
                %env_file,
                var = VAPID_KEY_VAR,
                %error,
                "could not save a generated web push identity; notifications are off — set one yourself and restart"
            );
        }
        Vapid::Invalid => error!(
            var = VAPID_KEY_VAR,
            "web push identity is not a valid key; notifications are off"
        ),
    }
}

/// Write `key` into the env file at `path`, creating it if absent.
///
/// A file we create is created `0600` — it holds a credential, and the mode is
/// set at creation rather than after, so the secret is never briefly readable by
/// anyone else. A file that already exists keeps whatever mode the operator gave
/// it; tightening someone else's file is not ours to do.
fn persist(path: &Path, var: &str, value: &str) -> io::Result<()> {
    match std::fs::read_to_string(path) {
        Ok(existing) => std::fs::write(path, env_text_with(&existing, var, value)),
        Err(err) if err.kind() == ErrorKind::NotFound => {
            create_private_file(path)?.write_all(env_text_with("", var, value).as_bytes())
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

/// Splice `<var>=<value>` into the text of an env file: replace the existing
/// assignment if there is one, otherwise append.
///
/// Everything else in the file is preserved byte for byte — an operator's other
/// settings, their comments and their line endings, *and the other credential*
/// — because this rewrites a file we don't own. A commented-out `# <var>=` line
/// is a comment, not an assignment, and is left where it is.
fn env_text_with(text: &str, var: &str, value: &str) -> String {
    let assignment = format!("{var}={value}");
    let mut out = String::with_capacity(text.len() + assignment.len() + 1);
    let mut replaced = false;

    for line in text.split_inclusive('\n') {
        if !replaced && is_assignment(line, var) {
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

/// Whether `line` assigns `var`. Leading whitespace is tolerated; a comment is
/// not an assignment, and neither is a longer name that merely starts the same
/// way.
fn is_assignment(line: &str, var: &str) -> bool {
    line.trim_start().starts_with(&format!("{var}="))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::db::Db;
    use crate::testing::LogCapture;
    use rstest::rstest;

    /// A key an assertion can hunt for in log output.
    const SECRET: &str = "s3cr3t-ingest-key-do-not-log";

    const NOW: i64 = 1_000_000_000_000;

    /// A fresh database with no keys, plus the temp dir holding it (and standing
    /// in for the directory an env file is written to).
    async fn empty_db() -> (Db, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = db::connect(&crate::testing::sqlite_url(&tmp))
            .await
            .expect("db");
        (db, tmp)
    }

    /// What `.env` ended up pinning `var` to, so a test can prove the operator
    /// can actually read back what was generated.
    fn value_in(env_file: &Path, var: &str) -> String {
        std::fs::read_to_string(env_file)
            .expect("env file")
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{var}=")))
            .unwrap_or_else(|| panic!("an assignment of {var}"))
            .to_string()
    }

    /// [`value_in`] for the ingest key, which most of this module is about.
    fn key_in(env_file: &Path) -> String {
        value_in(env_file, INGEST_KEY_VAR)
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
        assert_eq!(env_text_with(before, INGEST_KEY_VAR, "k"), expected);
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

    // -- The admin password (#19, ADR-0008) ---------------------------------

    /// A password an assertion can hunt for in log output.
    const ADMIN_SECRET: &str = "s3cr3t-admin-password-do-not-log";

    /// The same rule as the ingest key's, for the credential that gates every
    /// configuration change: whatever boot does about it, it never reaches a
    /// log line.
    #[test]
    fn a_configured_admin_password_is_used_and_never_logged() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_file = tmp.path().join(".env");
        let capture = LogCapture::start();

        let outcome = provision_admin_password(Some(ADMIN_SECRET), &env_file);
        log_admin_password(&outcome);

        assert_eq!(outcome.password(), Some(ADMIN_SECRET));
        capture.assert_never_logged(ADMIN_SECRET);
        assert!(!env_file.exists(), "a configured password writes nothing");
        let logged = capture.text();
        assert!(logged.contains("admin password"), "{logged}");
    }

    /// `Debug` is written by hand precisely so a `{:?}` — in a `?` chain, an
    /// `assert!` message, a future panic handler — cannot be the thing that
    /// leaks the credential ADR-0011 rule 2 protects. Every variant, because
    /// a redaction that covers two of three is not one.
    #[test]
    fn debugging_an_admin_password_never_shows_it() {
        let outcomes = [
            AdminPassword::Configured(ADMIN_SECRET.to_string()),
            AdminPassword::Generated {
                password: ADMIN_SECRET.to_string(),
                env_file: PathBuf::from("/srv/.env"),
            },
            AdminPassword::NotPersisted {
                env_file: PathBuf::from("/srv/.env"),
                error: io::Error::new(ErrorKind::PermissionDenied, "denied"),
            },
        ];

        for outcome in &outcomes {
            let rendered = format!("{outcome:?}");
            assert!(!rendered.contains(ADMIN_SECRET), "{rendered}");
            assert!(rendered.contains("redacted") || rendered.contains("NotPersisted"));
        }
        // ...and the path, which is not a secret, still comes through — a
        // redaction that hid it would leave the operator nowhere to look.
        assert!(format!("{:?}", outcomes[1]).contains("/srv/.env"));
        assert!(format!("{:?}", outcomes[2]).contains("denied"));
    }

    /// First run with nothing configured. rdio-scanner ships a *known* default
    /// password (`rdio-scanner`, `defaults.go`) and nags until it is changed, so
    /// an instance exposed before anyone reads the nag is open. Radio-Scout
    /// never has a guessable credential to begin with.
    #[test]
    fn a_generated_admin_password_goes_to_the_env_file_and_never_to_a_log() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_file = tmp.path().join(".env");
        let capture = LogCapture::start();

        let outcome = provision_admin_password(None, &env_file);
        log_admin_password(&outcome);

        let generated = outcome
            .password()
            .expect("a generated password")
            .to_string();
        assert!(!generated.is_empty());
        capture.assert_never_logged(&generated);
        // ...and it is the password sitting in the file the operator can read.
        assert_eq!(value_in(&env_file, ADMIN_PASSWORD_VAR), generated);
        assert!(capture.text().contains(".env"), "{}", capture.text());
    }

    /// A password only the server ever saw is worse than none: the operator
    /// could never log in, and every restart would invent another. So the admin
    /// surface stays closed and the failure is an ERROR to act on — the same
    /// bargain the ingest key makes.
    #[test]
    fn an_admin_password_that_cannot_be_saved_is_not_used() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_file = tmp.path().join("not-a-file");
        std::fs::create_dir(&env_file).expect("mkdir");
        let capture = LogCapture::start();

        let outcome = provision_admin_password(None, &env_file);
        log_admin_password(&outcome);

        assert_eq!(outcome.password(), None);
        let logged = capture.text();
        assert!(logged.contains("ERROR"), "{logged}");
        assert!(logged.contains(ADMIN_PASSWORD_VAR), "{logged}");
    }

    /// `RADIO_SCOUT_ADMIN_PASSWORD=` in an env file means "unset", not "the
    /// empty password" — which would otherwise be a credential anyone can send.
    #[rstest]
    #[case(None)]
    #[case(Some(""))]
    #[case(Some("   "))]
    fn a_blank_admin_password_means_unset(#[case] configured: Option<&str>) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_file = tmp.path().join(".env");

        let outcome = provision_admin_password(configured, &env_file);

        assert!(
            matches!(outcome, AdminPassword::Generated { .. }),
            "{outcome:?}"
        );
        assert!(!value_in(&env_file, ADMIN_PASSWORD_VAR).is_empty());
    }

    /// Whitespace an env file collects around a value must not change the
    /// password the operator thinks they set.
    #[test]
    fn a_configured_admin_password_is_trimmed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_file = tmp.path().join(".env");

        let outcome = provision_admin_password(Some(&format!("  {ADMIN_SECRET}\t")), &env_file);

        assert_eq!(outcome.password(), Some(ADMIN_SECRET));
    }

    /// The two credentials share one file, so writing the second must not eat
    /// the first — nor the operator's other settings.
    #[test]
    fn the_two_generated_credentials_coexist_in_one_env_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_file = tmp.path().join(".env");
        std::fs::write(&env_file, "RADIO_SCOUT_PORT=8080\n").expect("seed env");

        provision_admin_password(None, &env_file);
        // The ingest key writes through the same splice.
        std::fs::write(
            &env_file,
            env_text_with(
                &std::fs::read_to_string(&env_file).expect("read"),
                INGEST_KEY_VAR,
                "an-ingest-key",
            ),
        )
        .expect("write");

        let text = std::fs::read_to_string(&env_file).expect("env file");
        assert!(text.contains("RADIO_SCOUT_PORT=8080"), "{text}");
        assert_eq!(value_in(&env_file, INGEST_KEY_VAR), "an-ingest-key");
        assert!(
            !value_in(&env_file, ADMIN_PASSWORD_VAR).is_empty(),
            "{text}"
        );
    }

    /// A generated admin password is written `0600` for the same reason the
    /// ingest key is: it is a credential in the operator's working directory.
    #[cfg(unix)]
    #[test]
    fn a_created_env_file_holding_the_admin_password_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let env_file = tmp.path().join(".env");

        provision_admin_password(None, &env_file);

        let mode = std::fs::metadata(&env_file)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
    }

    // -- The Web Push identity (#16, ADR-0005) -------------------------------

    /// The identity has to be the *same* one next boot: a browser pins the
    /// public key at subscribe time, so an identity that changes on restart
    /// silently stops every existing subscription from ever being notified
    /// again. That is why it is written to a file rather than kept in memory.
    #[test]
    fn a_generated_vapid_key_is_written_to_the_env_file_and_survives_a_restart() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_file = tmp.path().join(".env");
        let capture = LogCapture::start();

        let first = provision_vapid_key(None, &env_file);
        log_vapid_key(&first);
        let secret = value_in(&env_file, VAPID_KEY_VAR);
        // The next boot reads what the first one wrote.
        let second = provision_vapid_key(Some(&secret), &env_file);
        log_vapid_key(&second);

        let public = first
            .key()
            .expect("a generated identity")
            .public_base64url();
        assert_eq!(
            second.key().expect("the same identity").public_base64url(),
            public,
            "a restart must not invent a new identity"
        );
        // The private half never reaches a log line; the public half does,
        // because it is what a browser pinned and is not a secret.
        capture.assert_never_logged(&secret);
        assert!(capture.text().contains(&public), "{}", capture.text());
    }

    /// A credential only the server ever saw is worse than none: every restart
    /// would invent another and quietly orphan the last one's subscriptions.
    #[test]
    fn a_vapid_key_that_cannot_be_saved_leaves_push_off() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_file = tmp.path().join("not-a-file");
        std::fs::create_dir(&env_file).expect("mkdir");
        let capture = LogCapture::start();

        let outcome = provision_vapid_key(None, &env_file);
        log_vapid_key(&outcome);

        assert!(outcome.key().is_none());
        let logged = capture.text();
        assert!(logged.contains("ERROR"), "{logged}");
        assert!(logged.contains(VAPID_KEY_VAR), "{logged}");
    }

    /// A typo'd key is a configuration mistake, but not one worth refusing to
    /// serve audio over: notifications go off, loudly, and the scanner keeps
    /// scanning.
    #[test]
    fn a_vapid_key_that_is_not_a_key_leaves_push_off() {
        let capture = LogCapture::start();

        let outcome = provision_vapid_key(Some("not-a-key"), Path::new("/nowhere/.env"));
        log_vapid_key(&outcome);

        assert!(matches!(outcome, Vapid::Invalid), "{outcome:?}");
        assert!(outcome.key().is_none());
        assert!(capture.text().contains("ERROR"), "{}", capture.text());
    }

    #[rstest]
    #[case(None)]
    #[case(Some(""))]
    #[case(Some("   "))]
    fn a_blank_vapid_key_means_unset(#[case] configured: Option<&str>) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_file = tmp.path().join(".env");

        let outcome = provision_vapid_key(configured, &env_file);

        assert!(matches!(outcome, Vapid::Generated { .. }), "{outcome:?}");
        assert!(!value_in(&env_file, VAPID_KEY_VAR).is_empty());
    }

    /// `Debug` is hand-written for the same reason [`AdminPassword`]'s is —
    /// every variant, because a redaction that covers three of four is not one.
    #[test]
    fn debugging_a_vapid_outcome_never_shows_the_key() {
        let key = VapidKey::generate();
        let secret = key.secret_base64url();
        let outcomes = [
            Vapid::Configured(VapidKey::parse(&secret).expect("key")),
            Vapid::Generated {
                key,
                env_file: PathBuf::from("/srv/.env"),
            },
            Vapid::NotPersisted {
                env_file: PathBuf::from("/srv/.env"),
                error: io::Error::new(ErrorKind::PermissionDenied, "denied"),
            },
            Vapid::Invalid,
        ];

        for outcome in &outcomes {
            let rendered = format!("{outcome:?}");
            assert!(!rendered.contains(&secret), "{rendered}");
        }
        assert!(format!("{:?}", outcomes[1]).contains("/srv/.env"));
        assert!(format!("{:?}", outcomes[2]).contains("denied"));
    }

    /// All three credentials share one file, and none of them may eat another.
    #[test]
    fn the_three_generated_credentials_coexist_in_one_env_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_file = tmp.path().join(".env");
        std::fs::write(&env_file, "RADIO_SCOUT_PORT=8080\n").expect("seed env");

        provision_admin_password(None, &env_file);
        provision_vapid_key(None, &env_file);
        std::fs::write(
            &env_file,
            env_text_with(
                &std::fs::read_to_string(&env_file).expect("read"),
                INGEST_KEY_VAR,
                "an-ingest-key",
            ),
        )
        .expect("write");

        let text = std::fs::read_to_string(&env_file).expect("env file");
        assert!(text.contains("RADIO_SCOUT_PORT=8080"), "{text}");
        assert_eq!(value_in(&env_file, INGEST_KEY_VAR), "an-ingest-key");
        assert!(
            !value_in(&env_file, ADMIN_PASSWORD_VAR).is_empty(),
            "{text}"
        );
        assert!(!value_in(&env_file, VAPID_KEY_VAR).is_empty(), "{text}");
    }

    /// A path that cannot be *read* is reported as the read failure it is —
    /// only a genuinely absent file is created (#83).
    ///
    /// The distinction is the whole content of the `NotFound` guard, and it is
    /// what an operator is shown: `persist`'s error goes straight into the WARN
    /// that says no credential was saved. Told "File exists" about a path that
    /// is really a directory — or that they have no permission to read — they
    /// debug the wrong thing, on the one boot where nothing else works either.
    #[test]
    fn a_path_that_cannot_be_read_reports_the_read_failure_not_a_create_failure() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("env-is-a-directory");
        std::fs::create_dir(&path).expect("directory");

        let error = persist(&path, INGEST_KEY_VAR, SECRET).expect_err("a directory is not a file");

        assert_eq!(
            error.kind(),
            ErrorKind::IsADirectory,
            "the read's own error, not `create_new`'s AlreadyExists: {error}"
        );
        assert!(
            path.is_dir(),
            "nothing was written over the operator's path"
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
            let after = env_text_with(&before, INGEST_KEY_VAR, &key);
            let assignments = after.lines().filter(|line| is_assignment(line, INGEST_KEY_VAR)).count();
            proptest::prop_assert_eq!(assignments, 1, "in:\n{}", after);
            proptest::prop_assert!(after.ends_with('\n'));
            proptest::prop_assert!(
                after.contains(&format!("{INGEST_KEY_VAR}={key}")),
                "key missing from:\n{}", after
            );
        }
    }
}
