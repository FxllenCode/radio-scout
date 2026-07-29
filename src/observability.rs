//! Observability wiring (ADR-0011): one `tracing` subscriber, to stdout, with
//! `RUST_LOG` as its control surface.
//!
//! Radio-Scout had no logging at all until an ingest bug (2026-07-25) took an
//! hour to diagnose because the server said nothing while every upload 500'd.
//! The rules that came out of that incident live in ADR-0011; this module is
//! only their wiring:
//!
//! - **stdout, never a file.** systemd/journald, Docker and a foreground
//!   terminal all capture it, so rotation and retention are inherited rather
//!   than shipped — a Pi whose disk retention (#10) is already rationing does
//!   not need a second disk-space policy.
//! - **The filter selects level *and* target**, defaulting to
//!   [`DEFAULT_DIRECTIVES`]. It is resolved by [`crate::config`] (#17) from
//!   `--log`, `RUST_LOG` and `[log] directives`, in that order, and validated
//!   there — this module only installs what it is handed.
//! - **Third-party chatter is filtered by default, not by accident.** Two
//!   demotions, both deliberate: sqlx logs every statement it executes on target
//!   `sqlx::query` at INFO, which on a scanner ingesting a Call a second is a
//!   log line per query — protocol detail, so DEBUG (ADR-0011 rule 7); and
//!   `sea_orm_migration` narrates migrations in formatted sentences that
//!   [`crate::db`] re-emits as structured events, so leaving it at INFO means
//!   saying everything twice. `RUST_LOG=debug` gets both back.

use std::io::{self, IsTerminal};

use tracing::Subscriber;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

use crate::logsink::LogSink;

/// The filter used when `RUST_LOG` says nothing: everything at INFO, with
/// sqlx's per-statement log and sea-orm's migration narration demoted to the
/// level they belong at.
pub const DEFAULT_DIRECTIVES: &str = "info,sqlx::query=warn,sea_orm_migration=warn";

/// The directives to run with, given what was configured.
///
/// A blank value — `RUST_LOG=` in an env file, or a `[log] directives = ""` —
/// means the default. Being blank must not mean *silence*: a scanner that logs
/// nothing is the state this ADR exists to end.
fn filter_directives(directives: &str) -> &str {
    match directives.trim().is_empty() {
        true => DEFAULT_DIRECTIVES,
        false => directives,
    }
}

/// Whether `tracing` can build a filter from `directives` — what
/// [`crate::config`] asks before accepting one, so a mistyped filter is a boot
/// error naming the setting rather than a silent drop to the default.
///
/// Blank is not valid: it is "say nothing", which nobody configuring a log
/// filter means.
pub fn directives_are_valid(directives: &str) -> bool {
    !directives.trim().is_empty() && EnvFilter::try_new(directives).is_ok()
}

/// Install the global subscriber, filtered by `directives` (#17 resolves those
/// from `[log]`, `RUST_LOG` and `--log`), with `sink` as a second destination
/// when the operator log surface is on (#30).
///
/// Returns whether this call is the one that installed it; a second call is a
/// no-op rather than a panic, so an embedded or test use can't bring the process
/// down over logging.
pub fn init(directives: &str, sink: Option<LogSink>) -> bool {
    // Colour only when a human is actually watching. Every way we ship captures
    // stdout into something that stores it verbatim (journald, Docker, a log
    // file an operator pipes to), where escape sequences are just noise in a
    // pasted excerpt.
    let ansi = io::stdout().is_terminal();
    subscriber(directives, io::stdout, ansi, sink)
        .try_init()
        .is_ok()
}

/// Build the subscriber `init` installs: `directives` (or
/// [`DEFAULT_DIRECTIVES`]) over `writer`, plus the database sink if there is
/// one.
///
/// The console's filter is attached to the **console layer**, not to the
/// subscriber, so the two destinations are filtered independently (#30): `[log]
/// directives` is what an operator sees on stdout and `[log] database_level` is
/// what the Logs view holds, and neither can silence the other. A subscriber-
/// wide filter would make `--log warn` empty the operator log surface as a side
/// effect.
///
/// Invalid directives fall back to the default rather than refusing to boot.
/// Configuration validates them first (`config::validate_directives`), so this
/// is the last resort behind that: a subscriber that failed to build would
/// leave us with the silence this module replaces.
fn subscriber<W>(
    directives: &str,
    writer: W,
    ansi: bool,
    sink: Option<LogSink>,
) -> impl Subscriber + Send + Sync
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    let directives = filter_directives(directives);
    let filter =
        EnvFilter::try_new(directives).unwrap_or_else(|_| EnvFilter::new(DEFAULT_DIRECTIVES));
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(ansi)
                .with_filter(filter),
        )
        // The sink carries its own level (`LogSinkConfig`), so it needs no
        // filter here — and must not be given the console's.
        .with(sink)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{CaptureWriter, ScopedSubscriber};
    use rstest::rstest;
    use tracing::{debug, info, warn};

    /// Emit one event at each level (plus one on sqlx's target) under a
    /// subscriber built from `directives`, and return what reached the writer.
    fn logged_with(directives: &str) -> String {
        let capture = CaptureWriter::default();
        {
            let _installed =
                ScopedSubscriber::install(subscriber(directives, capture.clone(), false, None));
            debug!("a-debug-event");
            info!("an-info-event");
            warn!("a-warn-event");
            info!(target: "sqlx::query", "a-query-event");
        }
        capture.text()
    }

    /// The filter is the control surface an operator reaches for at 2am: turn it
    /// up to chase a problem, down to quiet a Pi.
    #[rstest]
    // The default -> INFO and above, and not a word more.
    #[case(DEFAULT_DIRECTIVES, &["an-info-event", "a-warn-event"], &["a-debug-event"])]
    // Blank (`RUST_LOG=` in an env file) is "unset", never "silent".
    #[case("", &["an-info-event"], &["a-debug-event"])]
    #[case("   ", &["an-info-event"], &["a-debug-event"])]
    // Turned down: a quiet Pi still says when something was rejected.
    #[case("warn", &["a-warn-event"], &["an-info-event", "a-debug-event"])]
    // Turned up: everything, including the per-statement detail hidden by default.
    #[case("debug", &["a-debug-event", "a-query-event"], &[])]
    // Per-target directives work, which is the whole reason for `env-filter`.
    #[case("warn,radio_scout=debug", &["a-debug-event"], &["a-query-event"])]
    // Junk falls back to the default rather than booting a silent scanner.
    #[case("==", &["an-info-event"], &["a-debug-event"])]
    #[case("radio_scout=verbose", &["an-info-event"], &["a-debug-event"])]
    fn the_filter_selects_what_is_recorded(
        #[case] directives: &str,
        #[case] expected: &[&str],
        #[case] absent: &[&str],
    ) {
        let logged = logged_with(directives);
        for line in expected {
            assert!(logged.contains(line), "expected {line:?} in:\n{logged}");
        }
        for line in absent {
            assert!(!logged.contains(line), "unexpected {line:?} in:\n{logged}");
        }
    }

    /// sqlx logs every statement it runs at INFO on its own target. On a scanner
    /// taking a Call a second that is a line per query, so the default filter
    /// demotes it — the single reason `DEFAULT_DIRECTIVES` is not just `info`.
    #[test]
    fn a_query_is_not_an_info_event_by_default() {
        let logged = logged_with(DEFAULT_DIRECTIVES);
        assert!(logged.contains("an-info-event"), "{logged}");
        assert!(!logged.contains("a-query-event"), "{logged}");
    }

    /// Every line carries a timestamp, a level and a target — the three things
    /// missing from a `println!`.
    #[test]
    fn lines_are_levelled_and_timestamped() {
        let logged = logged_with(DEFAULT_DIRECTIVES);
        assert!(logged.contains("INFO"), "{logged}");
        assert!(logged.contains("radio_scout::observability"), "{logged}");
        // The default formatter's RFC-3339 timestamp leads the line.
        assert!(
            logged.starts_with("20"),
            "expected a leading timestamp in:\n{logged}"
        );
    }

    /// Captured stdout — journald, Docker, a piped file — must not collect
    /// escape sequences; a terminal watching live may as well have colour.
    #[rstest]
    #[case(false)]
    #[case(true)]
    fn colour_is_the_caller_s_choice(#[case] ansi: bool) {
        let capture = CaptureWriter::default();
        {
            let _installed = ScopedSubscriber::install(subscriber(
                DEFAULT_DIRECTIVES,
                capture.clone(),
                ansi,
                None,
            ));
            info!("an-info-event");
        }
        assert_eq!(
            capture.text().contains('\u{1b}'),
            ansi,
            "{}",
            capture.text()
        );
    }

    #[rstest]
    #[case("", DEFAULT_DIRECTIVES)]
    #[case(" \t ", DEFAULT_DIRECTIVES)]
    #[case("debug", "debug")]
    #[case("warn,radio_scout::ingest=trace", "warn,radio_scout::ingest=trace")]
    fn directives_default_only_when_nothing_was_asked_for(
        #[case] directives: &str,
        #[case] expected: &str,
    ) {
        assert_eq!(filter_directives(directives), expected);
    }

    /// What #17's config asks before accepting a `[log] directives` or a
    /// `RUST_LOG`: an operator who mistypes a filter is told, rather than left
    /// running at a level they did not choose.
    #[rstest]
    #[case("debug", true)]
    #[case("warn,radio_scout::ingest=trace", true)]
    #[case(DEFAULT_DIRECTIVES, true)]
    // Blank means "say nothing", which ADR-0011 exists to end — so it is not a
    // filter anyone can have meant.
    #[case("", false)]
    #[case("   ", false)]
    #[case("==", false)]
    #[case("radio_scout=verbose", false)]
    fn only_directives_tracing_understands_are_valid(
        #[case] directives: &str,
        #[case] expected: bool,
    ) {
        assert_eq!(directives_are_valid(directives), expected, "{directives:?}");
    }

    /// **The two sinks are filtered independently (#30).** An operator who
    /// quiets the console — a Pi writing to an SD card — must not thereby quiet
    /// the Logs view an operator with no shell reads; and one who turns the
    /// console up to chase a problem must not start recording DEBUG lines in a
    /// database. Per-layer filters, not one filter over both.
    #[tokio::test]
    async fn the_console_filter_and_the_database_level_do_not_govern_each_other() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = crate::db::connect(&crate::testing::sqlite_url(&tmp))
            .await
            .expect("db");
        let (sink, writer) = crate::logsink::channel(crate::logsink::LogSinkConfig::default())
            .expect("an enabled sink");
        let draining = writer.spawn(db.clone());

        let capture = CaptureWriter::default();
        {
            // The console turned down to WARN, the database left at INFO.
            let _installed =
                ScopedSubscriber::install(subscriber("warn", capture.clone(), false, Some(sink)));
            info!("an-info-event");
            warn!("a-warn-event");
        }
        draining.await.expect("the writer drains");

        let console = capture.text();
        assert!(!console.contains("an-info-event"), "{console}");
        assert!(console.contains("a-warn-event"), "{console}");

        use sea_orm::EntityTrait;
        let stored: Vec<String> = crate::db::entities::log_event::Entity::find()
            .all(&db)
            .await
            .expect("read stored events")
            .into_iter()
            .map(|event| event.message)
            .collect();
        assert_eq!(stored, ["an-info-event", "a-warn-event"]);
    }

    /// Installing must never panic and must never displace a subscriber that is
    /// already there: the binary calls this once, but a test binary or an
    /// embedded use may not, and logging is never worth a crash.
    #[test]
    fn init_never_panics_and_never_displaces_a_subscriber() {
        // This process already has one (every capture test needs it), which is
        // exactly the situation being asserted about.
        crate::testing::ask_every_time();
        assert!(
            !init(DEFAULT_DIRECTIVES, None),
            "init must not replace an installed subscriber"
        );
        assert!(
            !init(DEFAULT_DIRECTIVES, None),
            "...however many times it is called"
        );
    }
}
