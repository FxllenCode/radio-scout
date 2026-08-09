//! Test-only helpers shared across the crate's unit tests.
//!
//! Logging is behavior (ADR-0011), so it is asserted like any other: a test
//! captures what the subscriber actually wrote and reads it back. That is the
//! only way to prove the rules that matter — that a rejected outcome is logged
//! at the level it claims, and that a secret is logged nowhere at all.
//!
//! ## Why a test process needs [`ask_every_time`]
//!
//! `tracing` caches, per callsite, whether *anyone* is interested in it — but a
//! capture subscriber is thread-local, and the cache is global. The first thread
//! to reach a callsite decides: a test with no subscriber gets there first,
//! the callsite is cached as "nobody cares", and the capturing test's events
//! vanish. That is exactly what happened here — tests that passed alone and
//! failed in company, dropping a random prefix of the migration log.
//!
//! [`ask_every_time`] installs a global subscriber that captures nothing but
//! answers `Interest::sometimes` for every callsite, which forces `tracing` to
//! ask the *current thread's* subscriber per event instead of trusting the
//! cache. Every capture then sees its own thread's events and nobody else's, in
//! parallel, under `cargo test` as well as `cargo nextest`.

use std::io;
use std::sync::{Arc, Mutex, Once};

use tracing::level_filters::LevelFilter;
use tracing::subscriber::{DefaultGuard, Interest};
use tracing::{Event, Metadata, Subscriber, span};
use tracing_subscriber::fmt::MakeWriter;

/// A create-if-missing SQLite URL for a database inside `dir`.
pub(crate) fn sqlite_url(dir: &tempfile::TempDir) -> String {
    format!("sqlite://{}?mode=rwc", dir.path().join("t.db").display())
}

/// Every field a type serializes must be named in its `Debug` output.
///
/// The gate a hand-written `Debug` needs and a derived one gets for free: a
/// field added later vanishes from the impl silently, and if it is a credential
/// it does the opposite — printing where redacting was the impl's entire
/// purpose (#101).
///
/// **This covers two types, and it is worth knowing which two it cannot.** The
/// crate hand-writes `Debug` nine times and most of those redact something —
/// `instance::Credentials` holds three plaintext secrets, and
/// `startup::AdminPassword`, `startup::Vapid`, `webpush::VapidKey`,
/// `webpush::Recipient` and `blob::PresignedUrl` each hold one. The two this
/// gates are [`crate::config::Database`] and [`crate::blob::S3Config`], and what
/// makes them reachable is that they *are* their TOML section (#87), so they
/// derive `Serialize` — which is the reflection Rust does not otherwise have,
/// and the reason this needs no macro of its own. The other seven are runtime
/// types with nothing to reflect over; extending the gate to them means giving
/// them a field list by some other means, and no ticket has asked for that yet.
///
/// **This is half a gate, and the other half is at the call site.** The sample
/// must be an exhaustive struct literal with every field populated: the literal
/// is what stops compiling when a field is added, and a `None` behind a
/// `skip_serializing_if` serializes to nothing and would be waved through here.
pub(crate) fn assert_debug_names_every_field(value: &(impl serde::Serialize + std::fmt::Debug)) {
    let shown = format!("{value:?}");
    let serialized = serde_json::to_value(value).expect("the type serializes");
    let fields = serialized.as_object().expect("a struct, so it has fields");

    // A sample that serialized to nothing would pass the loop below without
    // asserting anything at all — which is precisely the drift this exists to
    // catch, arriving one level up.
    assert!(!fields.is_empty(), "nothing to gate: {shown}");
    for field in fields.keys() {
        // `name: ` rather than `name`, so a field is proven to be in *field*
        // position: a bare `contains` also matches a field name that happens to
        // appear inside some other field's value, which would let the very
        // omission this exists to catch slip past. Non-alternate `{:?}` is what
        // `debug_struct` renders that way, which is why `shown` is built with it.
        assert!(
            shown.contains(&format!("{field}: ")),
            "`{field}` is not named in the Debug output: {shown}"
        );
    }
}

/// A global subscriber that records nothing and is interested in everything, so
/// that no callsite is ever cached as uninteresting.
struct AskEveryTime;

impl Subscriber for AskEveryTime {
    fn register_callsite(&self, _: &Metadata<'_>) -> Interest {
        // The whole point: never `always` (which would skip filtering) and never
        // `never` (which would swallow another thread's events).
        Interest::sometimes()
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        // Keeps the process-wide max level at TRACE; a capture asserts about
        // levels a filtered subscriber would otherwise clamp away.
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

/// Make this process ask per event rather than trusting the callsite cache. Safe
/// to call from any test, any number of times; the first call wins.
pub(crate) fn ask_every_time() {
    ASK_EVERY_TIME.call_once(|| {
        // Ignore a global subscriber someone else already installed — then the
        // cache is their problem, and a capture still works on its own thread.
        let _ = tracing::subscriber::set_global_default(AskEveryTime);
        // Callsites reached before this point were cached against whatever was
        // (not) installed then; ask again now.
        tracing_core::callsite::rebuild_interest_cache();
    });
}

/// A `tracing` subscriber installed for this thread, for as long as this value
/// lives.
pub(crate) struct ScopedSubscriber(#[allow(dead_code)] DefaultGuard);

impl ScopedSubscriber {
    /// Install `subscriber` as this thread's default.
    pub(crate) fn install<S>(subscriber: S) -> Self
    where
        S: Subscriber + Send + Sync + 'static,
    {
        ask_every_time();
        ScopedSubscriber(tracing::subscriber::set_default(subscriber))
    }
}

/// A `tracing` writer that keeps everything in memory for a test to read back.
#[derive(Clone, Default)]
pub(crate) struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl CaptureWriter {
    /// Everything written so far, as text.
    pub(crate) fn text(&self) -> String {
        String::from_utf8(self.0.lock().expect("capture buffer").clone()).expect("utf-8 log output")
    }
}

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

/// Captures everything logged on this thread — at **every** level, so a test can
/// assert that a secret appears at no level rather than just not at INFO — for
/// as long as it is alive.
///
/// `#[tokio::test]`'s current-thread runtime polls the test's future on this
/// same thread, so events emitted across an `.await` are captured too.
pub(crate) struct LogCapture {
    writer: CaptureWriter,
    _subscriber: ScopedSubscriber,
}

impl LogCapture {
    /// Start capturing.
    pub(crate) fn start() -> Self {
        let writer = CaptureWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(LevelFilter::TRACE)
            .with_writer(writer.clone())
            // Plain text, so an assertion can look for `reason=duplicate`
            // without escape sequences landing in the middle of it.
            .with_ansi(false)
            .finish();
        LogCapture {
            writer,
            _subscriber: ScopedSubscriber::install(subscriber),
        }
    }

    /// Everything logged since [`LogCapture::start`].
    pub(crate) fn text(&self) -> String {
        self.writer.text()
    }

    /// Assert `needle` appears nowhere in what was logged. The failure message
    /// carries the whole capture, since "which line leaked it" is the first
    /// thing you want to know.
    pub(crate) fn assert_never_logged(&self, needle: &str) {
        let logged = self.text();
        assert!(
            !logged.contains(needle),
            "{needle:?} must never be logged, but was, in:\n{logged}"
        );
    }
}
