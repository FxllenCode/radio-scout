//! The operator log surface's sink (#30, ADR-0011): a second `tracing` layer,
//! writing what the server said to the `logs` table.
//!
//! An operator running Radio-Scout as a Service (#23) has nowhere to read
//! `journalctl` from — and "why did my recorder's Calls stop arriving?" is
//! exactly the question the console answers and a browser cannot. rdio-scanner
//! has this and operators expect it. Three things about it are worth doing
//! differently, and each is the reason for a decision here:
//!
//! - **Parameterised inserts.** rdio builds its insert with an interpolated
//!   `fmt.Sprintf` (`server/log.go`), so a message containing an apostrophe
//!   corrupts the row. Ours goes through [`repo::insert_log_events`], where the
//!   values are bind parameters and nothing is escaped by hand.
//! - **The console stays primary.** rdio puts a database write in the path of
//!   every log line. Here the two are separate layers: an event is *offered* to
//!   this sink through a bounded channel with [`Sender::try_send`], which never
//!   waits and never fails upward, and a background task does the writing. A
//!   database that is slow, broken or not there yet cannot slow down, fail, or
//!   be noticed by the request that produced the event.
//! - **Rule 5 applies to what is stored**, not just to what is printed. The
//!   sink **refuses to run below INFO** ([`StoredLevel::LEVELS`]): a listener's
//!   address rides only lines that are already DEBUG (`crate::http_log`), so
//!   flooring the sink puts them out of reach by construction rather than by a
//!   redaction pass that could be wrong. It also keeps a Pi from writing a row
//!   per range request, which rule 8 would have asked for anyway.
//!
//! The two halves are made together by [`channel`]: [`LogSink`] is the `Layer`
//! that goes into the subscriber at boot, and [`LogWriter`] is the task that
//! drains it once there is a database to drain into. They are separate because
//! logging starts before the database does — the migration lines an operator
//! most wants to read are written before anything could have stored them.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{Receiver, Sender, error::TryRecvError};
use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::{Event, Level, Subscriber, info, warn};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

use crate::db::repo::{self, NewLogEvent};
use crate::now_ms;
use crate::worker::{Meter, Shutdown, Ticket, Worker};

/// What this Worker is called on a status surface (#93).
pub const WORKER: &str = "log-sink";

/// This module's own target, which the sink never stores.
///
/// The sink reports its own health through `tracing` like everything else, and
/// an event about a failing database write that went back into the queue for
/// that same database would be a feedback loop. Its lines are console-only,
/// deliberately.
const SINK_TARGET: &str = module_path!();

/// The database driver's per-statement log — **the other feedback loop**, and
/// the less obvious one.
///
/// sqlx emits a line per statement at INFO, which on this sink's own `INSERT
/// INTO logs` means storing an event produces an event to store, without end.
/// The console keeps them: ADR-0011's default directives demote this target to
/// WARN there, and rule 7 puts per-statement detail at DEBUG — this is the same
/// judgement, applied where a level filter alone cannot make it.
const QUERY_TARGET: &str = "sqlx::query";

/// Whether storing `target` would feed the sink its own output (see
/// [`SINK_TARGET`] and [`QUERY_TARGET`]).
fn feeds_back(target: &str) -> bool {
    // Exact for our own, because this module is one module and its `tests`
    // submodule is not it — a prefix match there would leave the sink unable to
    // record anything its own tests emit.
    target == SINK_TARGET
        // A prefix for the driver's: sqlx names sub-targets under `sqlx::query`
        // and a rename that slipped past would silently restore the loop, which
        // is the failure mode worth being defensive about.
        || target.starts_with(QUERY_TARGET)
}

/// The `message` field `tracing` puts an event's static string in.
const MESSAGE_FIELD: &str = "message";

/// The span field carrying #28's correlation id.
const REQUEST_ID_FIELD: &str = "request_id";

/// How many events one insert may carry. A burst arrives as a handful of
/// statements rather than one per line, without holding a write lock on a Pi
/// long enough to matter.
const MAX_BATCH: usize = 128;

/// What the operator log surface stores (`[log] database_level`, #17, #30) —
/// the least severe level written to the `logs` table, or nothing at all.
///
/// A type rather than a `String` that something remembers to check, so a level
/// this sink refuses is refused wherever it was written (the
/// [`crate::config::ProxyNet`] move). Since #87 this *is* the setting: the
/// section holds a `StoredLevel`, and there is no second shape to keep in step.
///
/// The queue depth that used to sit beside it is now [`QUEUE_CAPACITY`]. It was
/// a field of a configuration type with no key, no environment spelling, no
/// template line and no validation — a knob nobody could turn, which is a worse
/// thing to leave in a settings type than in a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredLevel(Option<Level>);

impl Default for StoredLevel {
    fn default() -> Self {
        StoredLevel(Some(Level::INFO))
    }
}

impl StoredLevel {
    /// The levels the sink may be set to, loudest first — everything ADR-0011
    /// rule 7 says an operator must act on or was told about, and nothing
    /// below.
    ///
    /// DEBUG and TRACE are missing on purpose and this is the whole of rule 5's
    /// enforcement for stored logs: those are the levels a listener's address
    /// may ride on (`crate::http_log::logs_client_addr`), and a public instance
    /// must not accumulate a database of who listened and when. Configuration
    /// refuses them by name rather than silently clamping, so an operator who
    /// asked for DEBUG is told why they can't have it.
    pub const LEVELS: [(&'static str, Option<Level>); 4] = [
        ("off", None),
        ("error", Some(Level::ERROR)),
        ("warn", Some(Level::WARN)),
        ("info", Some(Level::INFO)),
    ];

    /// A sink that stores nothing.
    pub const OFF: StoredLevel = StoredLevel(None);

    /// The least severe level stored, or `None` for a sink that is off.
    pub fn level(self) -> Option<Level> {
        self.0
    }

    /// How this level is spelled in the file and the environment — so a written
    /// config parses back to what wrote it.
    pub fn name(self) -> &'static str {
        Self::LEVELS
            .iter()
            .find(|(_, candidate)| *candidate == self.0)
            .map(|(name, _)| *name)
            // Unreachable: the only constructors are `FromStr` and `OFF`, both
            // of which yield a level in the table. "off" is the safe reading of
            // a level this sink cannot store.
            .unwrap_or("off")
    }
}

impl std::str::FromStr for StoredLevel {
    /// What an unusable level is told it should have been — [`EXPECTED_LEVEL`],
    /// so the file and the environment cannot describe it differently.
    type Err = &'static str;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::LEVELS
            .iter()
            .find(|(name, _)| *name == text)
            .map(|(_, level)| StoredLevel(*level))
            .ok_or(EXPECTED_LEVEL)
    }
}

impl Serialize for StoredLevel {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for StoredLevel {
    /// Refused *here*, so a level the sink will not store fails at the line the
    /// operator wrote it on. The key names itself because a `serde` error is
    /// rendered by position alone, and this is the section's only setting of
    /// this type.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(|expected| {
            serde::de::Error::custom(crate::config::rejected(
                "log.database_level",
                format_args!("{text:?}"),
                expected,
            ))
        })
    }
}

/// What an unusable `[log] database_level` is told it should have been.
///
/// Spelled out rather than built from [`StoredLevel::LEVELS`], because
/// [`crate::config::ConfigError::Invalid`] carries a `&'static str`; a test
/// holds it to naming every level the sink accepts. The *reason* rides along
/// because "why can't I have debug?" is otherwise a mystery an operator would
/// read as a typo.
pub const EXPECTED_LEVEL: &str = "\"off\", \"error\", \"warn\" or \"info\" — \
     DEBUG and below can carry a listener's address, which is never stored (ADR-0011 rule 5)";

/// How many events may be waiting to be written before arriving ones are
/// dropped. Dropping is the point: the alternative is making a log call wait on
/// a database.
///
/// Not a setting. An operator has no way to know what number to write here, and
/// the failure it guards against — a writer falling behind — is reported
/// (`log events dropped`) rather than tuned.
const QUEUE_CAPACITY: usize = 1024;

/// Make a sink and the writer that drains it, or `None` when `[log]
/// database_level` is `off`.
pub fn channel(level: StoredLevel) -> Option<(LogSink, LogWriter)> {
    channel_sized(level, QUEUE_CAPACITY)
}

/// [`channel`] with the queue depth spelled out, so the drop path can be driven
/// by filling a small queue rather than by outrunning a real one.
fn channel_sized(level: StoredLevel, capacity: usize) -> Option<(LogSink, LogWriter)> {
    let level = level.level()?;
    let (tx, rx) = tokio::sync::mpsc::channel(capacity.max(1));
    let dropped = Arc::new(AtomicU64::new(0));
    let meter = Meter::new();
    Some((
        LogSink {
            tx,
            level,
            dropped: dropped.clone(),
            meter: meter.clone(),
        },
        LogWriter { rx, dropped, meter },
    ))
}

/// The `tracing` layer half: turns an event into a row-to-be and offers it to
/// the writer, never waiting for one.
pub struct LogSink {
    tx: Sender<(NewLogEvent, Ticket)>,
    level: Level,
    /// Events the queue had no room for. Counted rather than logged where they
    /// happen — a full queue means the writer is behind, and a line per dropped
    /// event would be the hot loop rule 8 forbids.
    dropped: Arc<AtomicU64>,
    /// How far behind the writer is (#93). A `fetch_add` on the admitting side
    /// and nothing else, because this side is the path of the log call itself:
    /// publishing a composite value here would put a lock where rule 1 of this
    /// module's four says nothing may wait.
    meter: Arc<Meter>,
}

impl<S> Layer<S> for LogSink
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    /// The level *this layer* wants, so the registry's hint is the union of the
    /// console's filter and ours and neither can silence the other.
    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::from_level(self.level))
    }

    /// Remember a span's request id, if it has one, for the events logged
    /// inside it. Read once when the span opens rather than walked out of its
    /// fields per event.
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        let mut visitor = RequestIdField::default();
        attrs.record(&mut visitor);
        if let Some(request_id) = visitor.0
            && let Some(span) = ctx.span(id)
        {
            span.extensions_mut().insert(RequestId(request_id));
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let metadata = event.metadata();
        // A `Level` compares by verbosity: DEBUG is *greater* than INFO.
        if *metadata.level() > self.level || feeds_back(metadata.target()) {
            return;
        }

        let mut visitor = EventFields::default();
        event.record(&mut visitor);

        let stored = NewLogEvent {
            at_ms: now_ms(),
            level: metadata.level().as_str().to_owned(),
            target: metadata.target().to_owned(),
            message: visitor.message,
            fields: visitor.fields.render(),
            request_id: request_id_of(event, &ctx),
        };
        // Never `send().await`: a log call must not be a point where a request
        // waits for a database, and an event nobody has room for is worth less
        // than the request that was writing it.
        //
        // The Ticket rides with the event, so a full queue settles what it
        // dropped by dropping it — the depth reads as "behind", never as "owed
        // forever".
        if self.tx.try_send((stored, self.meter.admit())).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The writing half: drains the queue into the `logs` table.
pub struct LogWriter {
    rx: Receiver<(NewLogEvent, Ticket)>,
    dropped: Arc<AtomicU64>,
    meter: Arc<Meter>,
}

impl LogWriter {
    /// Start draining into `db`, as this Instance's log-sink Worker (#93).
    ///
    /// The one Worker that outlives a **restart**: the subscriber feeding it
    /// belongs to the process rather than to a run, so `Instance::restart`
    /// carries it across and only a full stop ends it. The loop also ends on
    /// its own when the sink is dropped — which in the binary is never, and in
    /// a test is the deterministic "everything has been written" moment.
    pub fn start(self, db: DatabaseConnection) -> Worker {
        let meter = self.meter.clone();
        Worker::start(WORKER, meter, move |stop| self.run(db, stop))
    }

    async fn run(mut self, db: DatabaseConnection, mut stop: Shutdown) {
        // Whether the last write failed, so a database that is down says so
        // once rather than once per batch — and says so again when it comes
        // back, which is the line an operator actually waits for.
        let mut failing = false;
        loop {
            let first = tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                received = self.rx.recv() => match received {
                    Some(event) => event,
                    None => break,
                },
            };
            failing = self.write_batch_from(&db, first, failing).await;
        }
        // Asked to stop, not asked to forget. Everything the sink was already
        // holding is written before this returns — a stop is the moment an
        // operator most wants the last few lines, and they are already accepted
        // work rather than something new to wait for.
        //
        // `close` first, and that is what *makes* this bounded: it refuses new
        // sends, so a process that is still logging while it shuts down cannot
        // keep feeding the drain it is waiting on. Without it `try_recv` would
        // happily pick up events that arrived during the previous batch's
        // write, and `Instance::stop` would have no upper bound at all. Events
        // offered after this are dropped and counted, exactly as they are when
        // the queue is full.
        self.rx.close();
        while let Ok(event) = self.rx.try_recv() {
            failing = self.write_batch_from(&db, event, failing).await;
        }
    }

    /// Take up to [`MAX_BATCH`] events starting with one already in hand, write
    /// them, and settle them.
    async fn write_batch_from(
        &mut self,
        db: &DatabaseConnection,
        first: (NewLogEvent, Ticket),
        failing: bool,
    ) -> bool {
        let mut batch = vec![first];
        while batch.len() < MAX_BATCH {
            match self.rx.try_recv() {
                Ok(event) => batch.push(event),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        let (events, tickets): (Vec<_>, Vec<_>) = batch.into_iter().unzip();
        let failing = write_batch(db, &events, failing).await;
        // Settled whether or not the write landed: a failed batch degrades to
        // console-only (the console already has every one of these), and an
        // event nobody will ever write is not one the sink still owes.
        drop(tickets);
        self.report_dropped();
        failing
    }

    /// Say how many events the queue had no room for, and forget them.
    ///
    /// Once per drained batch rather than once per loss — the count is the
    /// news, and a line per dropped event would be exactly the hot loop rule 8
    /// forbids at the moment the process is least able to afford one.
    fn report_dropped(&self) {
        let dropped = self.dropped.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            warn!(
                target: SINK_TARGET,
                dropped,
                "log events dropped; the console has them all"
            );
        }
    }
}

/// Write one batch, returning whether the sink is now in a failing state.
///
/// A failure here is never propagated anywhere: the console already has every
/// one of these events, so the honest response is to say so and carry on. This
/// is what "a sink failure degrades to console-only" means in code.
async fn write_batch(db: &DatabaseConnection, batch: &[NewLogEvent], failing: bool) -> bool {
    match repo::insert_log_events(db, batch).await {
        Ok(()) => {
            if failing {
                info!(target: SINK_TARGET, "log sink is storing events again");
            }
            false
        }
        Err(error) => {
            if !failing {
                warn!(
                    target: SINK_TARGET,
                    %error,
                    "log sink could not store events; the console is unaffected"
                );
            }
            true
        }
    }
}

/// The request id of the innermost enclosing span that has one.
///
/// `ingest{…}` is nested inside `http{request_id}`, so the walk goes outward
/// from the event: the id belongs to the request, whatever subsystem span the
/// event was actually written in.
fn request_id_of<S>(event: &Event<'_>, ctx: &Context<'_, S>) -> Option<String>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    ctx.event_scope(event)?
        .find_map(|span| span.extensions().get::<RequestId>().map(|id| id.0.clone()))
}

/// A span's remembered request id.
struct RequestId(String);

/// Reads a `request_id` field off a span's attributes and nothing else.
#[derive(Default)]
struct RequestIdField(Option<String>);

impl Visit for RequestIdField {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == REQUEST_ID_FIELD {
            self.0 = Some(value.to_owned());
        }
    }

    /// Where the id actually arrives: `request_id = %request_id` records a
    /// `Display` value, which `tracing` hands to a visitor as `Debug`.
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == REQUEST_ID_FIELD {
            self.0 = Some(format!("{value:?}"));
        }
    }
}

/// An event's fields, read out as it is recorded.
#[derive(Default)]
struct EventFields {
    /// The static message — `tracing` records it as a field called `message`.
    message: String,
    fields: Fields,
}

/// Everything that isn't the message, as the JSON object the row keeps.
#[derive(Default)]
struct Fields(serde_json::Map<String, serde_json::Value>);

impl Fields {
    /// The object as stored, or `None` when the event carried only a message.
    fn render(self) -> Option<String> {
        match self.0.is_empty() {
            true => None,
            false => Some(serde_json::Value::Object(self.0).to_string()),
        }
    }
}

impl EventFields {
    /// Record one field, keeping `message` apart from the rest.
    fn put(&mut self, field: &Field, value: serde_json::Value) {
        if field.name() == MESSAGE_FIELD {
            // The message arrives as a `Display`/`Debug` value, so it is
            // already the string; anything else is a `message = 42` nobody
            // writes, and rendering it is better than dropping it.
            self.message = match value {
                serde_json::Value::String(text) => text,
                other => other.to_string(),
            };
        } else {
            self.fields.0.insert(field.name().to_owned(), value);
        }
    }
}

impl Visit for EventFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, value.into());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, value.into());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, value.into());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, value.into());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        // A non-finite float has no JSON spelling; its text is still readable.
        self.put(
            field,
            serde_json::Number::from_f64(value)
                .map_or_else(|| value.to_string().into(), serde_json::Value::Number),
        );
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.put(field, value.to_string().into());
    }

    /// The catch-all, and the one that carries the message: `%x`, `?x` and the
    /// `format_args!` behind every event all arrive here.
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.put(field, format!("{value:?}").into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::db::entities::log_event;
    use crate::testing::{LogCapture, ScopedSubscriber, sqlite_url};
    use rstest::rstest;
    use sea_orm::{ConnectionTrait, DatabaseConnection, EntityTrait, QueryOrder};
    use tracing::info;
    use tracing_subscriber::layer::SubscriberExt;

    /// A database with the schema on it, and the temp dir holding it.
    async fn database() -> (DatabaseConnection, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = db::connect(&sqlite_url(&tmp)).await.expect("db");
        (db, tmp)
    }

    /// Emit events under a subscriber whose only layer is the sink, then read
    /// back what was stored.
    ///
    /// Dropping the sink closes the channel, which is the writer's own "nothing
    /// more is coming" signal — so a test awaits a finished task rather than
    /// polling a table and hoping.
    ///
    /// The capture is started *after* the events are emitted and before the
    /// writer is awaited, so what it holds is exactly what the sink said about
    /// itself. Nothing has run in between: a `#[tokio::test]`'s current-thread
    /// runtime only polls the writer at an await point, and `emit` has none —
    /// which is also what makes the overflow case deterministic rather than a
    /// race with the drain.
    async fn stored_after(
        db: &DatabaseConnection,
        level: StoredLevel,
        emit: impl FnOnce(),
    ) -> Vec<log_event::Model> {
        drain(db, level, QUEUE_CAPACITY, emit).await.0
    }

    /// [`stored_after`], keeping what the sink logged about itself too.
    async fn drain(
        db: &DatabaseConnection,
        level: StoredLevel,
        capacity: usize,
        emit: impl FnOnce(),
    ) -> (Vec<log_event::Model>, String) {
        let console = console_after(db, level, capacity, emit).await;
        let stored = log_event::Entity::find()
            .order_by_asc(log_event::Column::Id)
            .all(db)
            .await
            .expect("read stored events");
        (stored, console)
    }

    /// The one stored event. Fails if there isn't exactly one, because a test
    /// that meant "the first of several" should say so — `TestApp::the_call`'s
    /// rule, applied here.
    fn only(stored: &[log_event::Model]) -> &log_event::Model {
        assert_eq!(stored.len(), 1, "expected one stored event: {stored:#?}");
        &stored[0]
    }

    /// Drive the sink and keep only what it said about *itself* — for the case
    /// where the table is gone and there is nothing to read back.
    async fn console_after(
        db: &DatabaseConnection,
        level: StoredLevel,
        capacity: usize,
        emit: impl FnOnce(),
    ) -> String {
        let (sink, writer) = channel_sized(level, capacity).expect("an enabled sink");
        let draining = writer.start(db.clone());
        {
            let _installed = ScopedSubscriber::install(tracing_subscriber::registry().with(sink));
            emit();
        }
        let console = LogCapture::start();
        // Ends on its own, because the sink it was draining is gone — the
        // deterministic "everything that was going to be written has been".
        draining.join().await;
        console.text()
    }

    /// **Stopping drains rather than discards** (#93). A stop is the moment an
    /// operator most wants the last few lines — the shutdown they are about to
    /// investigate is in them — and the events are already *accepted* work, not
    /// something new to start waiting for.
    ///
    /// The sink is deliberately kept alive across the stop, which is what makes
    /// this a real test of the drain: the writer's ordinary exit is the sink
    /// being dropped, and with one still installed the only way out is the
    /// shutdown path.
    #[tokio::test]
    async fn stopping_the_writer_writes_what_it_was_already_holding() {
        let (db, _tmp) = database().await;
        let (sink, writer) =
            channel_sized(StoredLevel::default(), QUEUE_CAPACITY).expect("an enabled sink");
        let draining = writer.start(db.clone());

        let _installed = ScopedSubscriber::install(tracing_subscriber::registry().with(sink));
        info!("a line written just before the lights went out");

        // No await between the event and the stop, so a `#[tokio::test]`'s
        // current-thread runtime has not polled the writer at all: the event is
        // provably still in the queue, and only the drain can put it in the
        // table.
        draining.stop().await;

        let stored = log_event::Entity::find()
            .all(&db)
            .await
            .expect("read stored events");
        assert_eq!(
            only(&stored).message,
            "a line written just before the lights went out",
            "a stop threw away an event the sink had already accepted"
        );
    }

    /// An event reaches the table as its parts — level, target, message, and the
    /// structured fields ADR-0011 rule 6 asks every line to carry instead of a
    /// formatted sentence.
    #[tokio::test]
    async fn an_event_becomes_a_row() {
        let (db, _tmp) = database().await;

        let stored = stored_after(&db, StoredLevel::default(), || {
            info!(reason = "blacklisted", "ingest dropped");
        })
        .await;

        let event = only(&stored);
        assert_eq!(event.level, "INFO");
        assert_eq!(event.message, "ingest dropped");
        assert_eq!(event.target, "radio_scout::logsink::tests");
        assert_eq!(event.fields.as_deref(), Some(r#"{"reason":"blacklisted"}"#));
    }

    /// **The rdio bug.** `server/log.go` builds its insert with an interpolated
    /// `fmt.Sprintf`, so a message carrying an apostrophe closes the string
    /// literal and corrupts the row. Ours binds parameters, so the awkward text
    /// comes back exactly as it went in — including in a *field*, which is the
    /// half that also has to survive being JSON.
    #[rstest]
    #[case::apostrophe("recorder's upload failed")]
    #[case::quotes(r#"unknown key "foo" in [server]"#)]
    #[case::sql(r"'); DROP TABLE logs; --")]
    #[case::newlines("first line\nsecond line\r\nthird")]
    #[case::non_ascii("talkgroup Pompiers — Sûreté du Québec 🚒")]
    #[case::backslashes(r"C:\radio-scout\audio\a\b.wav")]
    #[case::json_shaped(r#"{"looks":"like json","but":"isn't"}"#)]
    #[tokio::test]
    async fn awkward_text_round_trips_intact(#[case] awkward: &str) {
        let (db, _tmp) = database().await;

        let stored = stored_after(&db, StoredLevel::default(), || {
            info!(detail = awkward, "{}", awkward);
        })
        .await;

        let event = only(&stored);
        assert_eq!(event.message, awkward);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                event.fields.as_deref().expect("a fields object")
            )
            .expect("stored fields are JSON")["detail"],
            serde_json::json!(awkward)
        );
    }

    /// The sink stores its own level and everything louder, and nothing quieter
    /// — the knob an operator turns down to keep a Pi's database small.
    #[rstest]
    #[case("info", &["ERROR", "WARN", "INFO"])]
    #[case("warn", &["ERROR", "WARN"])]
    #[case("error", &["ERROR"])]
    #[tokio::test]
    async fn only_events_at_or_above_the_level_are_stored(
        #[case] level: &str,
        #[case] expected: &[&str],
    ) {
        let (db, _tmp) = database().await;
        let level: StoredLevel = level.parse().expect("a level the sink accepts");
        let stored = stored_after(&db, level, || {
            tracing::error!("an error");
            tracing::warn!("a warning");
            tracing::info!("an info");
            tracing::debug!("a debug");
            tracing::trace!("a trace");
        })
        .await;

        let levels: Vec<&str> = stored.iter().map(|event| event.level.as_str()).collect();
        assert_eq!(levels, expected);
    }

    /// **ADR-0011 rule 5, structurally.** DEBUG is the level a listener's
    /// address may ride on, so the sink has no setting that reaches it — an
    /// operator who turns the *console* up to chase a problem does not thereby
    /// start recording who listened. `off` is the only other answer, and it
    /// stores nothing at all.
    #[rstest]
    #[case("off", None)]
    #[case("error", Some(Level::ERROR))]
    #[case("warn", Some(Level::WARN))]
    #[case("info", Some(Level::INFO))]
    #[case("debug", None)]
    #[case("trace", None)]
    #[case("DEBUG", None)]
    #[case("", None)]
    #[case("verbose", None)]
    fn only_levels_that_cannot_carry_a_listener_are_accepted(
        #[case] text: &str,
        #[case] expected: Option<Level>,
    ) {
        // `None` = not a level this sink accepts; `Some(None)` = off, which it
        // does. The two spellings of "nothing stored" are deliberately distinct.
        let accepted = text.parse::<StoredLevel>();
        match text {
            "off" => assert_eq!(
                accepted,
                Ok(StoredLevel::OFF),
                "off is a level, not a refusal"
            ),
            _ => assert_eq!(
                accepted.map(StoredLevel::level),
                expected.map(Some).ok_or(EXPECTED_LEVEL),
                "{text:?}"
            ),
        }
    }

    /// Every accepted level has one spelling, and it survives the round trip a
    /// written config makes — `--write-config` renders it, and the boot that
    /// reads that file back must land on the same sink.
    #[test]
    fn every_level_is_named_by_the_name_it_parses_from() {
        for (name, level) in StoredLevel::LEVELS {
            let parsed: StoredLevel = name.parse().expect("a level the sink accepts");
            assert_eq!(parsed.level(), level);
            assert_eq!(parsed.name(), name);
        }
    }

    /// The refusal names every level that *is* accepted — spelled out as a
    /// `&'static str`, so nothing but a test keeps it honest as levels change.
    #[test]
    fn the_refused_level_names_every_level_that_works() {
        for (name, _) in StoredLevel::LEVELS {
            assert!(
                EXPECTED_LEVEL.contains(name),
                "{name} missing from {EXPECTED_LEVEL:?}"
            );
        }
    }

    /// A sink set to `off` is not built at all — no queue, no task, no rows.
    #[test]
    fn an_off_sink_is_never_built() {
        assert!(channel(StoredLevel::OFF).is_none());
    }

    /// **The property the request path depends on.** A writer that has fallen
    /// behind must never make a log call wait, so a full queue drops what
    /// arrives — and says how much, once, rather than a line per loss (rule 8).
    /// rdio would have blocked here: its log write is synchronous, in the path
    /// of the line that produced it.
    #[tokio::test]
    async fn a_full_queue_drops_events_rather_than_waiting() {
        let (db, _tmp) = database().await;
        let (stored, console) = drain(&db, StoredLevel::default(), 2, || {
            for n in 0..5 {
                info!(n, "an event");
            }
        })
        .await;

        assert_eq!(stored.len(), 2, "the queue's worth, and not one more");
        assert!(console.contains("dropped=3"), "{console}");
        assert!(console.contains("WARN"), "{console}");
        assert_eq!(
            console.matches("log events dropped").count(),
            1,
            "one line for the batch, never one per dropped event:\n{console}"
        );
    }

    /// A sink whose table is gone degrades to console-only: it says so **once**,
    /// keeps accepting events, and never propagates the failure to whatever was
    /// logging. The console already has every one of these lines, which is the
    /// whole reason a failure here is survivable.
    #[tokio::test]
    async fn a_sink_that_cannot_store_says_so_once_and_carries_on() {
        let (db, _tmp) = database().await;
        db.execute_unprepared("DROP TABLE logs")
            .await
            .expect("take the table away");

        let console = console_after(&db, StoredLevel::default(), QUEUE_CAPACITY, || {
            info!("an event");
            info!("another event");
        })
        .await;

        assert!(console.contains("WARN"), "{console}");
        assert!(
            console.contains("no such table: logs"),
            "the operator needs the cause, not just the fact:\n{console}"
        );
        assert_eq!(
            console.matches("could not store events").count(),
            1,
            "a broken database says so once, not once per batch:\n{console}"
        );
    }

    /// ...and says when it starts working again, which is the line an operator
    /// who fixed the database is actually waiting for.
    #[tokio::test]
    async fn a_recovered_sink_says_it_is_storing_again() {
        let (db, _tmp) = database().await;
        let batch = [NewLogEvent {
            message: "an event".into(),
            ..Default::default()
        }];

        let capture = LogCapture::start();
        let still_failing = write_batch(&db, &batch, true).await;

        assert!(!still_failing);
        assert!(
            capture.text().contains("storing events again"),
            "{}",
            capture.text()
        );
    }

    /// ...and a sink that was *already* failing says nothing more. A database
    /// that is down is down for every batch after the first, and a line each
    /// would bury the console under the failure of the thing meant to relieve
    /// it.
    #[tokio::test]
    async fn a_sink_that_is_already_failing_stays_quiet() {
        let (db, _tmp) = database().await;
        db.execute_unprepared("DROP TABLE logs")
            .await
            .expect("take the table away");
        let batch = [NewLogEvent {
            message: "an event".into(),
            ..Default::default()
        }];

        let capture = LogCapture::start();
        let still_failing = write_batch(&db, &batch, true).await;

        assert!(still_failing);
        assert_eq!(capture.text(), "", "the first line already said it");
    }

    /// Every kind of field a `tracing` call can carry survives as the JSON type
    /// it is, so a stored event can be read (and one day queried) as data
    /// rather than as text. ADR-0011 rule 6 is what makes this worth doing:
    /// the variable half of a line is fields, not a sentence.
    #[tokio::test]
    async fn every_kind_of_field_survives_as_itself() {
        let (db, _tmp) = database().await;
        let failure = std::io::Error::other("the store said no");
        // Coerced here rather than inside the macro: `&x as &dyn Error` in a
        // field position is one expression `tracing` expands into a place
        // coverage cannot see into.
        let boom: &dyn std::error::Error = &failure;

        let stored = stored_after(&db, StoredLevel::default(), || {
            info!(
                enabled = true,
                target_lufs = -16.5,
                calls = 3_i64,
                bytes = 4_u64,
                object_key = "aa/1.wav",
                error = boom,
                // A float with no JSON spelling keeps its text rather than
                // failing the whole event.
                ratio = f64::INFINITY,
                "enhanced call"
            );
        })
        .await;

        let fields: serde_json::Value =
            serde_json::from_str(only(&stored).fields.as_deref().expect("fields"))
                .expect("stored fields are JSON");
        assert_eq!(fields["enabled"], serde_json::json!(true));
        assert_eq!(fields["target_lufs"], serde_json::json!(-16.5));
        assert_eq!(fields["calls"], serde_json::json!(3));
        assert_eq!(fields["bytes"], serde_json::json!(4));
        assert_eq!(fields["object_key"], serde_json::json!("aa/1.wav"));
        assert_eq!(fields["error"], serde_json::json!("the store said no"));
        assert_eq!(fields["ratio"], serde_json::json!("inf"));
    }

    /// A `message` that isn't a string is not something anyone writes on
    /// purpose — but dropping it would leave a row with no message at all,
    /// which is worse than rendering the value.
    #[tokio::test]
    async fn a_message_that_is_not_a_string_is_still_a_message() {
        let (db, _tmp) = database().await;

        let stored = stored_after(&db, StoredLevel::default(), || {
            info!(message = 42);
        })
        .await;

        assert_eq!(only(&stored).message, "42");
    }

    /// A sink that was healthy and still is says nothing at all — the steady
    /// state is every batch, forever, and an INFO line per batch would be the
    /// hourly-notice mistake retention's own sweep line avoids.
    #[tokio::test]
    async fn a_healthy_sink_narrates_nothing() {
        let (db, _tmp) = database().await;
        let batch = [NewLogEvent {
            message: "an event".into(),
            ..Default::default()
        }];

        let capture = LogCapture::start();
        let failing = write_batch(&db, &batch, false).await;

        assert!(!failing);
        assert_eq!(capture.text(), "", "a working sink is a quiet one");
    }

    /// An event logged while serving a request carries that request's id (#28),
    /// so the `internal error (ref: …)` a listener reads out over the phone
    /// finds its cause in the Logs view — which is the whole point of the ref
    /// for an operator who has no shell to grep from.
    #[tokio::test]
    async fn an_event_inside_a_request_carries_its_id() {
        let (db, _tmp) = database().await;

        let stored = stored_after(&db, StoredLevel::default(), || {
            tracing::error_span!("http", request_id = "0123456789abcdef").in_scope(|| {
                info!("inside the request");
            });
            info!("outside any request");
        })
        .await;

        assert_eq!(stored.len(), 2, "expected two stored events: {stored:#?}");
        let (inside, outside) = (&stored[0], &stored[1]);
        assert_eq!(inside.request_id.as_deref(), Some("0123456789abcdef"));
        assert_eq!(outside.request_id, None);
    }

    /// The id comes from the nearest span that has one, so an event inside
    /// ingest's own `ingest{…}` span — nested in the request span — is still
    /// correlated with the upload it belonged to.
    #[tokio::test]
    async fn a_nested_span_still_finds_the_request_id() {
        let (db, _tmp) = database().await;

        let stored = stored_after(&db, StoredLevel::default(), || {
            tracing::error_span!("http", request_id = "fedcba9876543210").in_scope(|| {
                tracing::error_span!("ingest", system_ref = 11).in_scope(|| {
                    info!("stored call");
                });
            });
        })
        .await;

        let event = only(&stored);
        assert_eq!(event.request_id.as_deref(), Some("fedcba9876543210"));
    }

    /// The sink never stores the two targets that would make it **recursive**:
    /// its own (a line about a failed write, queued for the database that
    /// failed) and sqlx's per-statement log, which narrates the very insert
    /// this sink just made — storing one would queue another, for ever, on a
    /// Pi. The console keeps both.
    #[rstest]
    #[case::its_own(SINK_TARGET)]
    #[case::per_statement("sqlx::query")]
    #[tokio::test]
    async fn the_targets_that_would_feed_back_are_never_stored(#[case] target: &str) {
        let (db, _tmp) = database().await;

        let stored = stored_after(&db, StoredLevel::default(), || {
            match target {
                SINK_TARGET => info!(target: SINK_TARGET, "a line about the sink"),
                _ => info!(target: "sqlx::query", "INSERT INTO logs …"),
            }
            info!("a line about anything else");
        })
        .await;

        let event = only(&stored);
        assert_eq!(event.message, "a line about anything else");
    }
}
