//! Log capture for integration tests.
//!
//! Logging is behavior (ADR-0011), so it is asserted over the real HTTP boundary
//! like everything else: a test installs a capturing subscriber, drives a
//! request, and reads back the line the server actually wrote.
//!
//! This mirrors `src/testing.rs`, which serves the crate's *unit* tests. The
//! duplication is the `#[cfg(test)]` boundary: an integration binary links the
//! library's public API, not its test-only module, and exporting a capture
//! harness from the shipped crate to avoid ~60 lines of test scaffolding would
//! put it in the binary an operator runs. The two are deliberately not
//! identical — this one filters to Radio-Scout's own targets (see
//! [`LogCapture::start`]) because an integration binary shares a process with
//! hyper and reqwest, which narrate every socket they open.
//!
//! ## Why [`ask_every_time`] exists
//!
//! `tracing` caches, per callsite, whether *anyone* is interested in it — but a
//! capture subscriber is thread-local and the cache is global. Under `cargo
//! test` (one process, tests on many threads) the first thread to reach a
//! callsite decides for all of them: a test with no subscriber gets there first,
//! the callsite is cached as "nobody cares", and the capturing test sees
//! nothing. [`ask_every_time`] installs a global subscriber that records nothing
//! but answers `Interest::sometimes`, forcing `tracing` to ask the *current
//! thread's* subscriber per event. (`cargo nextest` gives each test its own
//! process and would not need it — `cargo test` still works, and CLAUDE.md
//! documents both.)

use std::io;
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use tracing::level_filters::LevelFilter;
use tracing::subscriber::{DefaultGuard, Interest};
use tracing::{Event, Metadata, Subscriber, span};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

/// A global subscriber that records nothing and is interested in everything, so
/// no callsite is ever cached as uninteresting.
struct AskEveryTime;

impl Subscriber for AskEveryTime {
    fn register_callsite(&self, _: &Metadata<'_>) -> Interest {
        // Never `always` (which would skip filtering) and never `never` (which
        // would swallow another thread's events).
        Interest::sometimes()
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        // Keeps the process-wide max level at TRACE, so a capture can assert
        // about levels a filtered subscriber would otherwise clamp away.
        Some(LevelFilter::TRACE)
    }

    fn enabled(&self, _: &Metadata<'_>) -> bool {
        // A thread with no capture of its own records nothing.
        false
    }

    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
    fn event(&self, _: &Event<'_>) {}
    fn enter(&self, _: &span::Id) {}
    fn exit(&self, _: &span::Id) {}
}

static ASK_EVERY_TIME: Once = Once::new();

/// Make this process ask per event rather than trusting the callsite cache.
fn ask_every_time() {
    ASK_EVERY_TIME.call_once(|| {
        let _ = tracing::subscriber::set_global_default(AskEveryTime);
        // Callsites reached before this point were cached against whatever was
        // (not) installed then; ask again now.
        tracing_core::callsite::rebuild_interest_cache();
    });
}

/// Everything written by the subscriber, kept in memory for a test to read back.
#[derive(Clone, Default)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .expect("capture buffer")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// How long [`LogCapture::wait_for`] keeps looking, and how often. Two seconds
/// is slack, not a timing assumption: the line it waits for is already written
/// or about to be, and the budget only bounds a failure.
const LINE_POLLS: u32 = 2_000;
const LINE_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Captures everything logged on this thread — at **every** level, so a test can
/// assert a field appears at no level rather than just not at INFO — for as long
/// as it is alive.
///
/// `#[tokio::test]`'s current-thread runtime polls every task on this same
/// thread, including the spawned `axum::serve` task, so the server's own lines
/// land here too.
pub struct LogCapture {
    writer: CaptureWriter,
    _subscriber: DefaultGuard,
}

impl LogCapture {
    /// Start capturing. Do this *before* spawning the app, so nothing is missed.
    ///
    /// Only Radio-Scout's own targets are kept. hyper and reqwest narrate every
    /// socket they open at DEBUG/TRACE — including the loopback address of the
    /// test client — which would make "this IP appears nowhere" (ADR-0011 rule
    /// 5) pass or fail on somebody else's log line.
    pub fn start() -> Self {
        ask_every_time();
        let writer = CaptureWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("radio_scout=trace"))
            .with_writer(writer.clone())
            // Plain text, so an assertion can look for `status=404` without
            // escape sequences landing in the middle of it.
            .with_ansi(false)
            .finish();
        LogCapture {
            writer,
            _subscriber: tracing::subscriber::set_default(subscriber),
        }
    }

    /// Everything logged since [`LogCapture::start`].
    pub fn text(&self) -> String {
        String::from_utf8(self.writer.0.lock().expect("capture buffer").clone())
            .expect("utf-8 log output")
    }

    /// Every captured line containing `needle`.
    pub fn lines_containing(&self, needle: &str) -> Vec<String> {
        self.text()
            .lines()
            .filter(|line| line.contains(needle))
            .map(str::to_owned)
            .collect()
    }

    /// The one captured line containing `needle`; fails if there isn't exactly
    /// one, carrying the whole capture so "which lines did match" is answerable.
    pub fn only_line_containing(&self, needle: &str) -> String {
        let matched = self.lines_containing(needle);
        assert_eq!(
            matched.len(),
            1,
            "expected exactly one line containing {needle:?}, got {matched:#?}, in:\n{}",
            self.text()
        );
        matched.into_iter().next().expect("one match")
    }

    /// Wait for a line containing `needle`, returning the first one.
    ///
    /// A line a *connection* emits — the live feed's connect/disconnect — lands
    /// when the server's task next runs, not when the client's call returns, so
    /// reading the capture straight after the call is a race. Polling is the
    /// honest fix: there is no completion to await from the client's side.
    pub async fn wait_for(&self, needle: &str) -> String {
        for _ in 0..LINE_POLLS {
            if let Some(line) = self.lines_containing(needle).into_iter().next() {
                return line;
            }
            tokio::time::sleep(LINE_POLL_INTERVAL).await;
        }
        panic!("{needle:?} never appeared in:\n{}", self.text());
    }

    /// Assert `needle` appears nowhere in what was logged.
    pub fn assert_never_logged(&self, needle: &str) {
        let logged = self.text();
        assert!(
            !logged.contains(needle),
            "{needle:?} must never be logged, but was, in:\n{logged}"
        );
    }
}
