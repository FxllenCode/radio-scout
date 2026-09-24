//! **Is this Instance healthy** (#70, spec US 48–49) — the status document an
//! Operator reads and the Prometheus text a scraper reads, over one value.
//!
//! [`Status`] is that value. The admin surface answers it as JSON and
//! [`render`] turns the same struct into the text exposition format, so the two
//! surfaces cannot come to disagree about what this Instance is doing — a
//! second aggregation behind `/metrics` would be a second implementation of the
//! thing it claims to describe, which is the argument #50 makes about a
//! preview.
//!
//! `GET /api/admin/status` answers the JSON; `/metrics` the exposition. The two
//! would otherwise agree right up until one of them moved.
//!
//! # Two kinds of number, and only one of them costs anything
//!
//! What the **process** knows — how long it has been up, what it has admitted,
//! what it has refused, what every **Worker** owes, how many Listeners are
//! connected — is read from atomics and costs nothing, so it is read on every
//! request and is what "refreshes live" actually means.
//!
//! What the **database and the disk** know — how many Calls there are, how many
//! bytes of audio, when each System last keyed — costs a scan of the `calls`
//! table, and a status page held open beside a Grafana scraping every second
//! would run that scan continuously on the hardware least able to afford it. So
//! [`Gauges`] is read at most once every [`GAUGE_TTL`] and shared by both
//! surfaces, and **when it was read rides on the wire** ([`Status::gauges_at_ms`])
//! rather than being guessed at by either client — [`crate::catalog`]'s
//! `activityWindowMs` rule, which exists so a client cannot come to describe a
//! window that has moved.
//!
//! [`GAUGE_TTL`] is fifteen seconds — Prometheus' own default scrape interval —
//! so a status page held open beside a Grafana scraping every second costs the
//! database the same as neither. `tests/metrics.rs` asserts that as a
//! statement-count **difference** between the first ask and the second rather
//! than a pinned number.
//!
//! # Counted where it is answered
//!
//! A refusal is counted in the **request middleware**, from a [`Refused`] the
//! response carries — [`crate::failure::Broke`]'s own shape, and for its reason:
//! the middleware sees every outcome of every route, including the ones a
//! handler never wrote. Three funnels insert it and there are no others, because
//! [`crate::failure::Reason::respond`] is the one rendering of a refusal,
//! [`crate::share::page::Rendered`] is the one HTML rendering of one, and
//! [`crate::ingest::Recorded`] is the one rendering of an **Admission**.
//!
//! So a route added later is counted by construction — `failure::redact`'s reason
//! for living in the middleware too. A process-global registry was the obvious
//! alternative and is what every Rust metrics crate does; it is wrong here
//! because it would not isolate between tests sharing a process under `cargo
//! test`.
//!
//! The exception is a refusal that answers *nobody*: a live-feed connection over
//! its **Access code**'s limit has already been upgraded and has no status line
//! left to write into. [`Metrics::refuse`] is what those surfaces call — it
//! writes the line *and* counts, so the counter cannot be forgotten there
//! either.
//!
//! # Nothing here can name anybody
//!
//! ADR-0011 rule 5, and it is the rule that shapes what is absent. There is a
//! Listener **count** and no breakdown of it; there are per-**System** rates and
//! deliberately no per-**Talkgroup** ones, which would be a public record of
//! which channel was busy at 3am on an Instance whose listener chart is behind
//! the admin session for exactly that reason. Every label value here comes from
//! a **closed** vocabulary — [`crate::failure::Reason::slug`],
//! [`crate::failure::Stage::slug`], [`crate::ingest::Admission::slug`], a
//! Worker's own name — so no request can put unbounded cardinality behind one,
//! which is the other half of why #92 made those vocabularies closed.
//!
//! [`crate::ingest::Admission::slug`] is **derived** from `Reason::slug` rather
//! than respelled, so `outcome="duplicate"` and `reason="duplicate"` are the same
//! word for the same ending; `Admission::OUTCOMES` is the list that seeds a
//! series per ending at boot (so a dashboard reads `0` rather than "no data"),
//! and `mod tests` holds it to being *exactly* the set `slug` can produce, in
//! both directions.
//!
//! # The token is the switch
//!
//! `[metrics] token` absent means `/metrics` is not served at all; set means a
//! scraper presents it as a bearer. There is deliberately no third state. The
//! numbers here are the same ones #62 put behind the admin session — CONTEXT.md's
//! **Listener count** says it plainly: *an open Archive does not make how many
//! people listen to an Instance public* — so an endpoint that could be switched
//! on openly would undo that decision from a sibling route. A credential that is
//! also the switch cannot be turned on by accident; it is the **Webhook** URL's
//! bargain, one surface along.
//!
//! It has no command-line flag (`[storage.s3]`'s reason: `ps` is world-readable),
//! and the refusal is a `404` rather than a `401`, because with no token there is
//! nothing to authenticate *to*. The **route is registered either way**: left
//! out, the SPA fallback would answer `/metrics` with the app's own HTML and a
//! `200`, which is worse than any refusal. `/metrics` also joins `/healthz` in
//! `http_log`'s `Chatty` class — a scrape is the same traffic on the same cadence
//! — and a 4xx still escalates it to WARN, which is how a scraper with the wrong
//! token stays findable.
//!
//! # Two names are load-bearing
//!
//! `/code-review` found both. The Worker families are `worker_in_hand` /
//! `worker_settled_total` and deliberately **not** `worker_queue_depth` /
//! `worker_completed_total`, because #93's own rule is that *what one unit of a
//! Worker's work is belongs to that Worker*: the two delivery senders count "I
//! have caught up", so a family called `queue_depth` reads as a backlog it is not
//! and `rate(worker_completed_total{worker="downstream"})` reads as a delivery
//! rate it is not. The queue depth an Operator means is `sink_queue_depth`, which
//! is the durable row count and really is one.
//!
//! And a **System's label rides on `radio_scout_system_info`** rather than on its
//! numeric series — `build_info`'s shape — because a label is an Operator's to
//! change and a numeric series carrying one becomes a *different* series the day
//! they rename it, quietly orphaning every `by (system)` panel built before the
//! rename.
//!
//! # What a depth cannot say
//!
//! **A Worker's liveness.** `Worker::is_running` existed and lived on the handle
//! the `Instance` owns, which a handler can never see; `worker::Alive` is that
//! bit shared the way a `Meter` is, cleared by a guard riding *inside* the task
//! so a panic (`panic = "unwind"` is deliberate in `[profile.release]`) and a
//! cancellation both go through the same drop. Without it a worker that fell over
//! reads as perfectly idle, because it settles every Ticket it was holding on the
//! way out.
//!
//! **A Sink's roster is folded, and the fold carries when it last worked.**
//! `failing` and `queued` are the two readings #52 and #54 already put on their
//! own screens; neither answers *is anything still getting through* on a roster
//! whose peers are merely quiet, which is why `SinkHealth` also carries the
//! newest `last_success_ms` across the roster. What it deliberately does not
//! carry is a **counter** of deliveries by verdict: `delivery::Failed`'s slugs
//! are not `Reason`s, and threading a `Metrics` down to `delivery::say` would
//! reach through two senders' `Outbox` traits for a rate the acceptance criteria
//! do not ask for. Which peer, and what it last said, stays on that sink's screen
//! — a fold cannot name a row.
//!
//! # Two things are deliberately not verdicts
//!
//! That is `client/src/lib/status.ts`'s own rule: a **System** that has gone
//! quiet (a county is silent at 3am and a rural system for a day — every
//! threshold is wrong for somebody, so the age is shown and the Operator reads
//! it), and an Archive sitting *at* its size cap (Retention prunes down to it, so
//! that is the policy working). What *is* flagged is frozen **Event** audio over
//! the cap on its own, which is the one storage state nothing can prune and which
//! `retention::sweep` already reports once per sweep.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, FromQueryResult, QueryFilter, QuerySelect,
};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use crate::AppState;
use crate::db::entities::{call, downstream, system, webhook};
use crate::failure::{Failure, Reason, Stage};

/// Where a scraper finds the text exposition. Outside `/api` because it is not
/// this app's API — it is the one URL a third party is configured with, and
/// `/metrics` is what every Prometheus example in the world already says.
pub const METRICS_PATH: &str = "/metrics";

/// The exposition format's own content type, version and all. Prometheus is
/// content-type agnostic in practice; a `curl` is not, and an Operator checking
/// their token by hand should get text in a terminal rather than a download.
const EXPOSITION: &str = "text/plain; version=0.0.4; charset=utf-8";

/// How long a reading of the database and the disk is reused for.
///
/// Fifteen seconds, which is Prometheus' own default scrape interval: a scraper
/// on that cadence sees a fresh reading every time, and an Operator holding the
/// status page open at a faster poll costs the database nothing extra. The
/// bound is on the *readings*, not on either caller, so neither can make this
/// expensive by asking more often.
const GAUGE_TTL: Duration = Duration::from_secs(15);

/// How far back a per-System rate counts, and a Call inside it makes a System
/// "heard from recently".
///
/// An hour rather than [`crate::catalog::ACTIVITY_WINDOW_MS`]'s day, because the
/// question here is *is this receiver still working* rather than *is this
/// channel busy*. It rides on the wire for that module's reason.
pub const RATE_WINDOW_MS: i64 = 60 * 60 * 1000;

/// `[metrics]` — the Prometheus surface, whose credential is its switch.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case", deny_unknown_fields)]
pub struct MetricsConfig {
    /// What a scraper presents as `Authorization: Bearer …`. **Absent means
    /// `/metrics` is not served**, which is what ships — see the module docs.
    ///
    /// Deliberately has no command-line flag, for `[storage.s3]`'s reason: `ps`
    /// is world-readable.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "written_token"
    )]
    pub token: Option<String>,
}

/// What an unusable `[metrics] token` is told it should have been.
///
/// Shared by the two layers that can carry it — the file, through [`token`], and
/// the environment, through the settings table — so an Operator who wrote a
/// blank one is told the same thing whichever they wrote it in.
pub const EXPECTED_TOKEN: &str = "a non-empty token, or no key at all to serve no metrics";

/// A token that could actually protect something, or why this one cannot.
///
/// **A blank token is refused rather than read as "off"**, and the difference
/// matters: absent means the endpoint does not exist, where `token = ""` would
/// *publish* it behind a credential every scrape in the world already presents.
/// Two spellings of "off" — one of which is "on, to everybody" — is exactly the
/// third state the module docs say there is not.
pub fn checked_token(value: &str) -> Result<String, &'static str> {
    // **What is validated is what is stored.** Trimming for the check and
    // keeping the original would accept `" abc "` as a token whose surrounding
    // spaces an Operator cannot see, cannot retype, and would spend an evening
    // not finding — and a shell exporting a variable is exactly where one gets
    // in.
    match value.trim() {
        "" => Err(EXPECTED_TOKEN),
        trimmed => Ok(trimmed.to_owned()),
    }
}

/// The one place a written token is refused.
///
/// Refused *here* rather than in a later validation pass, which is
/// [`crate::retention`]'s move and [`crate::config::ProxyNet`]'s before it: the
/// type that owns the value refuses it wherever it was written, so the message
/// carries the line and column of the key the Operator has to edit. The value
/// itself is **never** in that message (ADR-0011 rule 2) — which is why it names
/// the key and says nothing about what was there.
/// `String` rather than `Option<String>`: the field's own `#[serde(default)]`
/// answers an absent key without ever reaching here, and TOML has no null for a
/// present one — so "no metrics" is a key that isn't written, and an arm for it
/// would be unreachable. There is no serializing counterpart, because the value
/// goes out exactly as it is held.
fn written_token<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    let written = String::deserialize(deserializer)?;
    checked_token(&written).map(Some).map_err(|expected| {
        serde::de::Error::custom(format!(
            "`metrics.token` is not usable: expected {expected}"
        ))
    })
}

impl std::fmt::Debug for MetricsConfig {
    /// The boot summary prints the resolved configuration, and this one field is
    /// a credential (ADR-0011 rule 2). Whether one is *set* is not a secret and
    /// is the whole diagnostic — "the scraper is refused" and "there is no
    /// token" look identical without it, which is [`crate::curate::downstreams`]'
    /// `has_key` one surface along.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetricsConfig")
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// What this Instance has been doing, as both surfaces read it.
///
/// Held by [`AppState`], so a handler can reach it; the counters are atomics and
/// small maps rather than anything a request has to wait on.
#[derive(Clone)]
pub struct Metrics(Arc<Inner>);

struct Inner {
    config: MetricsConfig,
    /// When this process started, in unix milliseconds — the one number here
    /// that is not a count.
    started_at_ms: i64,
    /// Ingest endings by [`crate::ingest::Admission::slug`].
    ingest: Counters,
    /// Refusals by [`Reason::slug`], every surface.
    refused: Counters,
    /// Server errors by [`Stage::slug`].
    broke: Counters,
    /// What this Instance is: where the audio lives, and what Retention is
    /// holding to.
    wiring: Wiring,
    /// The last reading of the database and the disk — which carries when it
    /// was taken ([`Gauges::taken_at_ms`]).
    ///
    /// That [`GAUGE_TTL`] is doing its job is deliberately **not** counted here:
    /// it would be a number nothing outside this module could reach, and the
    /// honest proof is one layer down — `TestApp::statements_issued` either side
    /// of two asks, which is the same seam #86 used to make an N+1 visible from
    /// outside.
    gauges: Mutex<Option<Gauges>>,
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics::new(MetricsConfig::default(), Wiring::default(), crate::now_ms())
    }
}

impl Metrics {
    /// This run's metrics.
    pub fn new(config: MetricsConfig, wiring: Wiring, started_at_ms: i64) -> Self {
        let ingest = Counters::default();
        // Every ingest ending exists from boot, so a dashboard on a quiet
        // Instance reads `0` rather than "no data" — the one vocabulary here
        // small enough and central enough to be worth seeding.
        ingest.seed(crate::ingest::Admission::OUTCOMES);
        Metrics(Arc::new(Inner {
            config,
            started_at_ms,
            ingest,
            refused: Counters::default(),
            broke: Counters::default(),
            wiring,
            gauges: Mutex::new(None),
        }))
    }

    /// Count one ingest ending.
    pub fn admitted(&self, outcome: &'static str) {
        self.0.ingest.add(outcome);
    }

    /// Count one refusal, by [`Reason::slug`].
    pub fn refused(&self, reason: &'static str) {
        self.0.refused.add(reason);
    }

    /// Count one server error, by [`Stage::slug`].
    pub fn broke(&self, stage: &'static str) {
        self.0.broke.add(stage);
    }

    /// **Write a refusal down and count it**, for a surface that answers nobody.
    ///
    /// A live-feed connection over its **Access code**'s limit has already been
    /// upgraded, so there is no response to carry a [`Refused`] to the
    /// middleware. One call rather than two beside each other, so the counter is
    /// not a thing that surface can forget.
    pub fn refuse(&self, reason: &Reason) {
        reason.record();
        self.refused(reason.slug());
    }
}

/// A counter per label value, bounded by the closed vocabulary that supplies it.
///
/// A `BTreeMap` rather than a `HashMap` so a status document and an exposition
/// are ordered the same way every time they are read — a table an Operator
/// reads should not reshuffle itself between refreshes, which is the argument
/// [`crate::worker::Workers`] makes about registration order.
#[derive(Default)]
struct Counters(Mutex<BTreeMap<&'static str, u64>>);

impl Counters {
    fn add(&self, name: &'static str) {
        *self.0.lock().expect("metrics").entry(name).or_default() += 1;
    }

    /// Start every one of `names` at zero, so a series exists before anything
    /// has happened.
    fn seed(&self, names: &[&'static str]) {
        let mut counts = self.0.lock().expect("metrics");
        for name in names {
            counts.entry(name).or_default();
        }
    }

    fn read(&self) -> BTreeMap<&'static str, u64> {
        self.0.lock().expect("metrics").clone()
    }
}

/// A refusal on its way out, as the request middleware finds it in the
/// response's extensions.
///
/// [`crate::failure::Broke`]'s shape for the other kind of not-answering, and
/// there for its reason: the middleware is the one place that sees every
/// response, so counting there covers a route added later by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused(&'static str);

impl Refused {
    /// Mark a response as this refusal.
    pub(crate) fn mark(response: &mut Response, reason: &'static str) {
        response.extensions_mut().insert(Refused(reason));
    }

    /// Which refusal it was.
    pub(crate) fn reason(&self) -> &'static str {
        self.0
    }
}

/// An ingest ending on its way out, as the request middleware finds it.
///
/// [`Refused`]'s sibling: an **Admission** that stored a Call is not a refusal
/// and still has to be counted, because "how many Calls are arriving" is the
/// first question a status surface exists to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Admitted(&'static str);

impl Admitted {
    pub(crate) fn mark(response: &mut Response, outcome: &'static str) {
        response.extensions_mut().insert(Admitted(outcome));
    }

    pub(crate) fn outcome(&self) -> &'static str {
        self.0
    }
}

/// Everything both surfaces say, in one value.
///
/// The document the admin surface answers with *and* the thing [`render`] turns
/// into the text exposition — one aggregation, so "the same truths" is true by
/// construction rather than by two implementations agreeing today.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// What is running, so a bug report and a dashboard name the same release.
    pub version: &'static str,
    pub started_at_ms: i64,
    pub uptime_seconds: u64,
    /// How many **Listeners** are connected right now. A count, and there is
    /// nothing beside it — see the module docs.
    pub listeners: i64,
    /// Ingest endings since this process started, by
    /// [`crate::ingest::Admission::slug`]. Every outcome is present from boot,
    /// so a dashboard reads `0` rather than "no data" on a quiet Instance.
    pub ingest: BTreeMap<&'static str, u64>,
    /// Refusals since this process started, by [`Reason::slug`] — every
    /// surface, not only ingest.
    pub refused: BTreeMap<&'static str, u64>,
    /// Server errors since this process started, by [`Stage::slug`].
    pub errors: BTreeMap<&'static str, u64>,
    /// What every background **Worker** owes, and whether it is still going.
    pub workers: Vec<WorkerHealth>,
    /// When the readings below were taken — see [`GAUGE_TTL`]. On the wire so a
    /// client can say how old they are rather than implying they are live.
    pub gauges_at_ms: i64,
    /// How far back [`SystemHealth::calls`] counts.
    pub rate_window_ms: i64,
    pub archive: ArchiveHealth,
    pub storage: StorageHealth,
    pub retention: RetentionHealth,
    /// Every **System**, busiest-first — where the traffic is and when each one
    /// was last heard from. Deliberately not per-**Talkgroup**: see the module
    /// docs.
    pub systems: Vec<SystemHealth>,
    /// The two **Sink** rosters (#54's word for what a **Downstream** and a
    /// **Webhook** both are), summarised.
    pub sinks: Vec<SinkHealth>,
}

/// One **Worker**'s reading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerHealth {
    pub name: &'static str,
    /// Work admitted and not yet settled.
    pub depth: u64,
    /// Work settled since it started — monotonic, so two readings give a rate.
    pub done: u64,
    /// Whether its loop is still going. `false` on a running Instance is the one
    /// reading here an Operator has to act on (`crate::worker::Alive`).
    pub running: bool,
}

/// How much Archive there is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveHealth {
    pub calls: u64,
    /// Stored Call audio, off the `audio_size` column the **Retention** sweeper
    /// measures its own cap from — so the number here and the number that
    /// triggers a prune are the same number.
    pub audio_bytes: u64,
    /// **Event** audio frozen under a key of its own (#67), which the size cap
    /// counts and can never take.
    pub frozen_audio_bytes: u64,
    /// The oldest Call still held — what a retention window looks like from the
    /// far end.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_call_at_ms: Option<i64>,
}

/// How much room is left.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageHealth {
    /// Bytes free on the volume the audio lives on. **Absent on the S3
    /// backend**, where the question belongs to somebody else's machine — and
    /// absent rather than zero, because "no room" and "not this machine's
    /// problem" must not read the same.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_bytes: Option<u64>,
    /// That volume's size, so free can be read as a fraction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
}

/// What the retention policy is holding to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionHealth {
    /// `[retention] days`, `0` being "keep forever".
    pub days: u32,
    /// `[retention] max_size_gb` in bytes, when there is a cap at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_size_bytes: Option<u64>,
}

/// One **System**'s traffic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemHealth {
    pub r#ref: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Calls inside [`RATE_WINDOW_MS`] — the rate, in the units an Operator
    /// reads rather than a per-second figure nothing else here is in.
    pub calls: i64,
    /// When this System was last heard from, over the **whole** Archive rather
    /// than the window above: a receiver that stopped two days ago is exactly
    /// what a status page exists to show, and a window would draw it as silence
    /// indistinguishable from a System that has never keyed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_call_at_ms: Option<i64>,
}

/// One **Sink** roster, summarised.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SinkHealth {
    /// `downstream` or `webhook`.
    pub sink: &'static str,
    pub total: u64,
    pub disabled: u64,
    /// How many have failed at least once since they last succeeded — the
    /// reading `curate::downstreams` already carries per row, folded.
    pub failing: u64,
    /// The **durable** queue depth across the roster: Calls written down and not
    /// yet taken, which is what survives a restart.
    pub queued: i64,
    /// When **anything** on this roster last delivered successfully.
    ///
    /// `failing` says how many are in trouble and `queued` says how much is
    /// piling up; neither answers *is this still working at all*, which on a
    /// roster whose peers are simply quiet reads the same either way. The
    /// per-row detail — which peer, and what it last said — stays on that sink's
    /// own curation screen (#52, #54), because a fold cannot name a row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_success_ms: Option<i64>,
}

/// What only the database and the disk can say — read at most once every
/// [`GAUGE_TTL`] and shared by both surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gauges {
    /// When they were taken. **On the value rather than beside it**, so a
    /// reading and its age travel together: a caller handed the two separately
    /// could pair one refresh's numbers with another refresh's clock, and the
    /// page would say its numbers were fresher than they are.
    pub taken_at_ms: i64,
    pub archive: ArchiveHealth,
    pub storage: StorageHealth,
    pub systems: Vec<SystemHealth>,
    pub sinks: Vec<SinkHealth>,
}

/// Turn a [`Status`] into Prometheus' text exposition format.
///
/// Hand-written, which is what "minimal dependencies" in the ticket buys: the
/// format is a name, optional labels, and a number, and a registry crate would
/// bring a second place for every one of these numbers to live. Pure, so every
/// rule about it — the escaping, the families, what is absent — is a value a
/// test constructs rather than an endpoint it drives.
pub fn render(status: &Status) -> String {
    let mut out = Exposition::default();

    out.family("build_info", GAUGE, "Which release is running; always 1.");
    out.sample("build_info", &[("version", status.version)], 1);

    out.family(
        "uptime_seconds",
        GAUGE,
        "How long this process has been up.",
    );
    out.sample("uptime_seconds", &[], status.uptime_seconds);

    out.family(
        "ingest_total",
        COUNTER,
        "Uploads by what Ingest decided about them.",
    );
    out.counts("ingest_total", "outcome", &status.ingest);

    out.family(
        "refused_total",
        COUNTER,
        "Requests refused, by the reason they were refused for.",
    );
    out.counts("refused_total", "reason", &status.refused);

    out.family(
        "errors_total",
        COUNTER,
        "Server errors, by the stage the server was at.",
    );
    out.counts("errors_total", "stage", &status.errors);

    // **Not `worker_queue_depth`**, and the name is the whole point (#93): what
    // one unit of a Worker's work *is* belongs to that Worker, so these two are
    // not comparable between workers — the **Downstream** and **Webhook**
    // senders' unit is "I have caught up", not "one Call". A family called
    // `queue_depth` would read as a backlog, and
    // `rate(worker_completed_total{worker="downstream"})` would read as a
    // delivery rate and be neither. The queue depth an Operator means is
    // `sink_queue_depth` below, which really is one.
    out.family(
        "worker_in_hand",
        GAUGE,
        "Work a background worker has been handed and not yet settled. A unit is that worker's own and is not comparable between workers.",
    );
    for worker in &status.workers {
        out.sample("worker_in_hand", &[("worker", worker.name)], worker.depth);
    }
    out.family(
        "worker_settled_total",
        COUNTER,
        "Work a background worker has settled since it started, in that worker's own units.",
    );
    for worker in &status.workers {
        out.sample(
            "worker_settled_total",
            &[("worker", worker.name)],
            worker.done,
        );
    }
    out.family(
        "worker_running",
        GAUGE,
        "Whether a background worker's loop is still going.",
    );
    for worker in &status.workers {
        out.sample(
            "worker_running",
            &[("worker", worker.name)],
            u8::from(worker.running),
        );
    }

    out.family("listeners", GAUGE, "Live-feed connections open right now.");
    out.sample("listeners", &[], status.listeners);

    out.family("calls", GAUGE, "Calls held in the archive.");
    out.sample("calls", &[], status.archive.calls);
    out.family("audio_bytes", GAUGE, "Stored call audio.");
    out.sample("audio_bytes", &[], status.archive.audio_bytes);
    out.family(
        "frozen_audio_bytes",
        GAUGE,
        "Event audio frozen past retention.",
    );
    out.sample("frozen_audio_bytes", &[], status.archive.frozen_audio_bytes);
    out.family(
        "oldest_call_timestamp_seconds",
        GAUGE,
        "When the oldest call still held was received.",
    );
    out.instant(
        "oldest_call_timestamp_seconds",
        &[],
        status.archive.oldest_call_at_ms,
    );

    out.family("retention_days", GAUGE, "The age window; 0 keeps forever.");
    out.sample("retention_days", &[], status.retention.days);
    out.family(
        "audio_bytes_limit",
        GAUGE,
        "The cap on stored call audio, when there is one.",
    );
    out.maybe("audio_bytes_limit", &[], status.retention.max_size_bytes);

    out.family(
        "storage_free_bytes",
        GAUGE,
        "Room left where the audio lives; absent unless it lives on this machine.",
    );
    out.maybe("storage_free_bytes", &[], status.storage.free_bytes);
    out.family(
        "storage_total_bytes",
        GAUGE,
        "How big that volume is; absent for the same reason.",
    );
    out.maybe("storage_total_bytes", &[], status.storage.total_bytes);

    // Both System families are emitted whole before the next begins: samples of
    // one family have to be contiguous, so a loop per family rather than one
    // loop writing two. The **Ref** is rendered once and lent to both, which is
    // also what keeps the two label sets identical.
    let systems: Vec<(String, &SystemHealth)> = status
        .systems
        .iter()
        .map(|system| (system.r#ref.to_string(), system))
        .collect();
    // **The name rides on an info family and the numbers do not**, which is
    // `build_info`'s shape and is what keeps a `by (system)` panel whole across
    // a rename: a label is an Operator's to change, so a numeric series carrying
    // one is a *different* series the day they change it. Grafana joins the two
    // on `system`; nothing has to re-derive a name from a Ref.
    out.family(
        "system_info",
        GAUGE,
        "A system this instance receives, and the label it is curated under; always 1.",
    );
    for (r#ref, system) in &systems {
        let mut labels = vec![("system", r#ref.as_str())];
        labels.push(("label", system.label.as_deref().unwrap_or_default()));
        out.sample("system_info", &labels, 1);
    }
    out.family(
        "system_calls",
        GAUGE,
        "Calls a system took inside the rate window.",
    );
    for (r#ref, system) in &systems {
        out.sample("system_calls", &[("system", r#ref.as_str())], system.calls);
    }
    out.family(
        "system_last_call_timestamp_seconds",
        GAUGE,
        "When a system was last heard from, over the whole archive.",
    );
    for (r#ref, system) in &systems {
        out.instant(
            "system_last_call_timestamp_seconds",
            &[("system", r#ref.as_str())],
            system.last_call_at_ms,
        );
    }

    out.family("sinks", GAUGE, "Downstream peers and webhooks configured.");
    for sink in &status.sinks {
        out.sample("sinks", &[("sink", sink.sink)], sink.total);
    }
    out.family(
        "sinks_disabled",
        GAUGE,
        "How many of those are switched off.",
    );
    for sink in &status.sinks {
        out.sample("sinks_disabled", &[("sink", sink.sink)], sink.disabled);
    }
    out.family(
        "sink_failing",
        GAUGE,
        "How many have failed at least once since they last succeeded.",
    );
    for sink in &status.sinks {
        out.sample("sink_failing", &[("sink", sink.sink)], sink.failing);
    }
    out.family(
        "sink_queue_depth",
        GAUGE,
        "Deliveries written down and not yet taken. The durable depth, which survives a restart.",
    );
    for sink in &status.sinks {
        out.sample("sink_queue_depth", &[("sink", sink.sink)], sink.queued);
    }
    out.family(
        "sink_last_success_timestamp_seconds",
        GAUGE,
        "When anything on this roster last delivered successfully.",
    );
    for sink in &status.sinks {
        out.instant(
            "sink_last_success_timestamp_seconds",
            &[("sink", sink.sink)],
            sink.last_success_ms,
        );
    }

    out.text
}

/// Every family here is `radio_scout_…`, spelled once.
const PREFIX: &str = "radio_scout_";
const GAUGE: &str = "gauge";
const COUNTER: &str = "counter";

/// The exposition being written.
///
/// A writer rather than a `format!` per line, because the format's two rules
/// that are easy to break by hand are *structural* — a family is declared once,
/// and its samples are contiguous — and a writer makes both the shape of the
/// calling code rather than things to remember.
///
/// A family is held back until its first sample, so a reading this Instance
/// never takes is **absent entirely** rather than declared and then empty: an
/// S3 instance does not claim to have a `storage_free_bytes`, so a dashboard
/// built against one is told it is missing rather than shown a blank series.
#[derive(Default)]
struct Exposition {
    text: String,
    /// The family declared and not yet spent on a sample.
    pending: Option<String>,
}

impl Exposition {
    /// Declare a family: what it is called, whether a scraper may take a rate of
    /// it, and what it means.
    fn family(&mut self, name: &str, kind: &str, help: &str) {
        self.pending = Some(format!(
            "# HELP {PREFIX}{name} {help}\n# TYPE {PREFIX}{name} {kind}\n"
        ));
    }

    /// One sample of it.
    fn sample(&mut self, name: &str, labels: &[(&str, &str)], value: impl std::fmt::Display) {
        if let Some(declaration) = self.pending.take() {
            self.text.push_str(&declaration);
        }
        self.text.push_str(PREFIX);
        self.text.push_str(name);
        if let Some(((first_name, first_value), rest)) = labels.split_first() {
            self.text
                .push_str(&format!("{{{first_name}=\"{}\"", escape(first_value)));
            for (label, value) in rest {
                self.text
                    .push_str(&format!(",{label}=\"{}\"", escape(value)));
            }
            self.text.push('}');
        }
        self.text.push_str(&format!(" {value}\n"));
    }

    /// One sample, or none at all — a reading nobody took is **absent**, never
    /// zero, because `0` free bytes is a disk that is full and `0` for a cap is
    /// one that permits nothing.
    fn maybe(
        &mut self,
        name: &str,
        labels: &[(&str, &str)],
        value: Option<impl std::fmt::Display>,
    ) {
        if let Some(value) = value {
            self.sample(name, labels, value);
        }
    }

    /// A unix-millisecond instant, as the seconds Prometheus states timestamps
    /// in. Three decimals rather than a bare division, so a `_seconds` family is
    /// not quietly a `_seconds`-rounded-down one.
    fn instant(&mut self, name: &str, labels: &[(&str, &str)], at_ms: Option<i64>) {
        self.maybe(name, labels, at_ms.map(seconds));
    }

    /// Every count in a vocabulary, under one label.
    fn counts(&mut self, name: &str, label: &str, counts: &BTreeMap<&'static str, u64>) {
        for (value, count) in counts {
            self.sample(name, &[(label, value)], count);
        }
    }
}

/// A unix-millisecond instant as seconds with milliseconds after the point.
///
/// `div_euclid`/`rem_euclid` rather than `/` and `%`, so an instant before 1970
/// — which nothing here can produce and a hand-written fixture can — renders as
/// a smaller number rather than as one with a negative fraction.
fn seconds(at_ms: i64) -> String {
    format!("{}.{:03}", at_ms.div_euclid(1_000), at_ms.rem_euclid(1_000))
}

/// A label value, with the three characters the format reserves spelled out.
///
/// Escaping rather than stripping: a System an Operator named `Fire "A"` should
/// appear under the name they gave it, and a value that silently lost characters
/// would be a different series from the one their dashboard names.
fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str(r"\\"),
            '"' => escaped.push_str(r#"\""#),
            '\n' => escaped.push_str(r"\n"),
            _ => escaped.push(character),
        }
    }
    escaped
}

/// What this Instance is, decided at boot and unchanged while it runs.
///
/// Configuration the status surface reports but does not own: where the audio
/// lives (so the disk can be asked how much room is left) and what **Retention**
/// is holding to. Passed in rather than reached for, so this module reads no
/// configuration of its own beyond `[metrics]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Wiring {
    /// Where the audio lives, when it lives on this machine. `None` on the S3
    /// backend — the question belongs to somebody else's machine.
    pub audio_root: Option<PathBuf>,
    /// The policy the **Retention** sweeper is holding to.
    pub retention: RetentionHealth,
}

impl Metrics {
    /// What a scraper has to present, when this Instance publishes at all.
    fn token(&self) -> Option<&str> {
        self.0.config.token.as_deref()
    }

    /// The readings only the database and the disk can give, at most once every
    /// [`GAUGE_TTL`].
    ///
    /// The lock is taken twice and never held across the read, because it is a
    /// `std::sync::Mutex` and a status request must not be able to block another
    /// one behind a query. Two requests arriving inside the same cold window
    /// therefore both read — harmless, since the read is idempotent and the
    /// later answer wins — and [`Metrics::gauge_reads`] counts honestly either
    /// way.
    async fn gauges(&self, state: &AppState) -> Result<Gauges, Failure> {
        let now = state.clock.now_ms();
        if let Some(gauges) = self.0.gauges.lock().expect("metrics").as_ref()
            && now.saturating_sub(gauges.taken_at_ms) < GAUGE_TTL.as_millis() as i64
        {
            return Ok(gauges.clone());
        }

        let gauges = read_gauges(&state.db, self.0.wiring.audio_root.as_deref(), now)
            .await
            .map_err(Stage::ReadStatus.failed())?;
        *self.0.gauges.lock().expect("metrics") = Some(gauges.clone());
        Ok(gauges)
    }
}

/// Everything both surfaces answer with — the one aggregation.
pub async fn read(state: &AppState) -> Result<Status, Failure> {
    let metrics = &state.metrics;
    let now = state.clock.now_ms();
    let gauges = metrics.gauges(state).await?;

    Ok(Status {
        version: env!("CARGO_PKG_VERSION"),
        started_at_ms: metrics.0.started_at_ms,
        // Saturating, because a clock that went backwards must cost a status
        // page a zero rather than an underflow.
        uptime_seconds: now.saturating_sub(metrics.0.started_at_ms).max(0) as u64 / 1_000,
        listeners: state.listeners.current(),
        ingest: metrics.0.ingest.read(),
        refused: metrics.0.refused.read(),
        errors: metrics.0.broke.read(),
        workers: state
            .workers
            .loads()
            .into_iter()
            .map(|reading| WorkerHealth {
                name: reading.name,
                depth: reading.load.depth,
                done: reading.load.done,
                running: reading.running,
            })
            .collect(),
        gauges_at_ms: gauges.taken_at_ms,
        rate_window_ms: RATE_WINDOW_MS,
        archive: gauges.archive,
        storage: gauges.storage,
        retention: metrics.0.wiring.retention,
        systems: gauges.systems,
        sinks: gauges.sinks,
    })
}

/// `GET /api/admin/status` — is this Instance healthy.
pub async fn status(State(state): State<AppState>) -> Result<Status, Failure> {
    read(&state).await
}

// The status document is answered as its JSON, decided beside the type (#92).
crate::answers_json!(Status);

/// `GET /metrics` — the same truths, in the text exposition format.
pub async fn expose(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Response, Failure> {
    // **The token is the switch**, so an Instance that has not been given one
    // has no endpoint rather than a locked one — see the module docs.
    let Some(expected) = state.metrics.token() else {
        return Err(Reason::MetricsDisabled.into());
    };
    if !presented(&headers).is_some_and(|token| matches(token, expected)) {
        // The address is resolved the request log's way (#17): the TCP peer
        // unless the Operator named that peer in `[server] trusted_proxies`,
        // because a forwarded header is attacker-controlled and this line names
        // whoever is knocking.
        let client_addr = state.trusted_proxies.client_of(peer.ip(), &headers);
        return Err(Reason::InvalidMetricsToken { client_addr }.into());
    }

    let status = read(&state).await?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, EXPOSITION)],
        render(&status),
    )
        .into_response())
}

/// The bearer token a scrape presented, if it presented one in the one shape
/// Prometheus sends.
fn presented(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())?
        .strip_prefix("Bearer ")
}

/// Whether the presented token is the configured one, compared in constant
/// time — the admin CSRF token's rule (#19), and for its reason: a comparison
/// that stops at the first wrong byte is a comparison that leaks the right ones.
fn matches(presented: &str, expected: &str) -> bool {
    presented.as_bytes().ct_eq(expected.as_bytes()).into()
}

/// Read what only the database and the disk can say.
///
/// Nine statements and one `statvfs`, run at most once every [`GAUGE_TTL`] and
/// shared by both surfaces — which is the whole reason the cache exists: a
/// status page held open beside a Grafana scraping every second must not run a
/// `SUM` over a county's `calls` table continuously on a Pi.
///
/// The two per-System reads are deliberately two statements rather than one
/// with a `CASE` in it: both walk `idx_calls_system_time`, one bounded to the
/// rate window and one not, and a portable conditional aggregate would be a
/// dialect divergence bought for nothing (ADR-0003).
pub async fn read_gauges<C: ConnectionTrait>(
    db: &C,
    audio_root: Option<&std::path::Path>,
    now_ms: i64,
) -> Result<Gauges, DbErr> {
    let totals = Totals::read(db).await?;
    let frozen = crate::db::repo::frozen_audio_bytes(db).await?;

    Ok(Gauges {
        taken_at_ms: now_ms,
        archive: ArchiveHealth {
            calls: totals.calls.max(0) as u64,
            audio_bytes: totals.audio_bytes.unwrap_or_default().max(0) as u64,
            frozen_audio_bytes: frozen,
            oldest_call_at_ms: totals.oldest_at_ms,
        },
        storage: room_left(audio_root),
        systems: systems(db, now_ms - RATE_WINDOW_MS).await?,
        sinks: sinks(db).await?,
    })
}

/// How much Archive there is, in one statement.
#[derive(Debug, FromQueryResult)]
struct Totals {
    calls: i64,
    /// `NULL` on an empty Archive, which `SUM` answers with rather than `0`.
    audio_bytes: Option<i64>,
    oldest_at_ms: Option<i64>,
}

impl Totals {
    async fn read<C: ConnectionTrait>(db: &C) -> Result<Totals, DbErr> {
        Ok(call::Entity::find()
            .select_only()
            .column_as(call::Column::Id.count(), "calls")
            .column_as(
                crate::db::sum_bigint(call::Column::AudioSize),
                "audio_bytes",
            )
            .column_as(call::Column::CallAtMs.min(), "oldest_at_ms")
            .into_model::<Totals>()
            .one(db)
            .await?
            // An aggregate with no `GROUP BY` always answers with a row; the
            // fallback is for the type rather than for a case.
            .unwrap_or(Totals {
                calls: 0,
                audio_bytes: None,
                oldest_at_ms: None,
            }))
    }
}

/// Every **System**, busiest first.
///
/// Read from the roster rather than from the Calls, so a System that has not
/// keyed since it was created is still on the page — which is exactly the row an
/// Operator setting up a new receiver is looking for.
async fn systems<C: ConnectionTrait>(db: &C, since_ms: i64) -> Result<Vec<SystemHealth>, DbErr> {
    let recent: std::collections::HashMap<i64, i64> = call::Entity::find()
        .select_only()
        .column(call::Column::SystemId)
        .column_as(call::Column::Id.count(), "calls")
        .filter(call::Column::CallAtMs.gte(since_ms))
        .group_by(call::Column::SystemId)
        .into_tuple::<(i64, i64)>()
        .all(db)
        .await?
        .into_iter()
        .collect();
    // Unbounded on purpose, unlike [`crate::catalog::ACTIVITY_WINDOW_MS`]: a
    // receiver that stopped two days ago is precisely what this page exists to
    // show, and a windowed answer would draw it as silence indistinguishable
    // from a System that has never keyed.
    let last: std::collections::HashMap<i64, i64> = call::Entity::find()
        .select_only()
        .column(call::Column::SystemId)
        .column_as(call::Column::CallAtMs.max(), "at_ms")
        .group_by(call::Column::SystemId)
        .into_tuple::<(i64, i64)>()
        .all(db)
        .await?
        .into_iter()
        .collect();

    let mut systems: Vec<SystemHealth> = system::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|row| SystemHealth {
            r#ref: row.r#ref,
            label: row.label,
            calls: recent.get(&row.id).copied().unwrap_or_default(),
            last_call_at_ms: last.get(&row.id).copied(),
        })
        .collect();
    // Busiest first, then the quiet ones by Ref — a table an Operator reads
    // should put the traffic at the top and should not reshuffle between
    // refreshes, which a sort on the label alone would do the moment one is
    // curated.
    systems.sort_by_key(|system| (std::cmp::Reverse(system.calls), system.r#ref));
    Ok(systems)
}

/// Both **Sink** rosters, summarised.
async fn sinks<C: ConnectionTrait>(db: &C) -> Result<Vec<SinkHealth>, DbErr> {
    let peers = downstream::Entity::find().all(db).await?;
    let peer_depths = crate::db::repo::delivery_depths(db).await?;
    let hooks = webhook::Entity::find().all(db).await?;
    let hook_depths = crate::db::repo::webhook_depths(db).await?;

    Ok(vec![
        SinkHealth {
            sink: "downstream",
            total: peers.len() as u64,
            disabled: peers.iter().filter(|peer| peer.disabled).count() as u64,
            failing: peers
                .iter()
                .filter(|peer| peer.consecutive_failures > 0)
                .count() as u64,
            queued: peer_depths.values().sum(),
            last_success_ms: peers.iter().filter_map(|peer| peer.last_success_ms).max(),
        },
        SinkHealth {
            sink: "webhook",
            total: hooks.len() as u64,
            disabled: hooks.iter().filter(|hook| hook.disabled).count() as u64,
            failing: hooks
                .iter()
                .filter(|hook| hook.consecutive_failures > 0)
                .count() as u64,
            queued: hook_depths.values().sum(),
            last_success_ms: hooks.iter().filter_map(|hook| hook.last_success_ms).max(),
        },
    ])
}

/// How much room is left where the audio lives.
///
/// One `statvfs`, which is what `df` costs and is bounded by [`GAUGE_TTL`] like
/// everything else here. **A failure is an absence, never a zero**: a path that
/// cannot be stated — a store that has not been created yet, a mount that has
/// gone — must not read as a disk that is full, which is the one reading an
/// Operator would act on in the middle of the night.
fn room_left(audio_root: Option<&std::path::Path>) -> StorageHealth {
    let Some(stats) = audio_root.and_then(|root| fs4::statvfs(root).ok()) else {
        return StorageHealth::default();
    };
    StorageHealth {
        free_bytes: Some(stats.available_space()),
        total_bytes: Some(stats.total_space()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    /// A Status with something in every field, so a rendering test is exercising
    /// the shapes rather than a page of absences.
    fn busy() -> Status {
        Status {
            version: "9.9.9",
            started_at_ms: 1_700_000_000_000,
            uptime_seconds: 3_600,
            listeners: 4,
            ingest: [("stored", 12u64), ("duplicate", 3), ("invalid-api-key", 0)]
                .into_iter()
                .collect(),
            refused: [("duplicate", 3u64), ("call-not-found", 7)]
                .into_iter()
                .collect(),
            errors: [("dedup", 1u64)].into_iter().collect(),
            workers: vec![
                WorkerHealth {
                    name: "retention",
                    depth: 0,
                    done: 2,
                    running: true,
                },
                WorkerHealth {
                    name: "enhancement",
                    depth: 5,
                    done: 41,
                    running: false,
                },
            ],
            gauges_at_ms: 1_700_000_003_600,
            rate_window_ms: RATE_WINDOW_MS,
            archive: ArchiveHealth {
                calls: 512,
                audio_bytes: 90_000,
                frozen_audio_bytes: 1_024,
                oldest_call_at_ms: Some(1_699_000_000_500),
            },
            storage: StorageHealth {
                free_bytes: Some(1_000),
                total_bytes: Some(4_000),
            },
            retention: RetentionHealth {
                days: 30,
                max_size_bytes: Some(2_000),
            },
            systems: vec![SystemHealth {
                r#ref: 11,
                label: Some("Fulton County".into()),
                calls: 42,
                last_call_at_ms: Some(1_700_000_003_000),
            }],
            sinks: vec![SinkHealth {
                sink: "downstream",
                total: 2,
                disabled: 1,
                failing: 1,
                queued: 9,
                last_success_ms: Some(1_700_000_002_000),
            }],
        }
    }

    /// The name a sample line belongs to — everything before its labels or its
    /// value.
    fn family_of(line: &str) -> &str {
        let head = line.split(' ').next().unwrap_or(line);
        head.split('{').next().unwrap_or(head)
    }

    /// **Every sample belongs to a family that was declared first, once.** A
    /// scraper reads `# TYPE` to know whether a series is a counter it may take
    /// a rate of; an undeclared family is guessed at, and a family declared
    /// twice is a parse error that fails the *whole* scrape rather than one
    /// series. Neither is visible by eye in four hundred lines of output.
    #[test]
    fn every_family_is_declared_once_before_its_samples() {
        let text = render(&busy());
        let mut declared: Vec<&str> = Vec::new();

        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("# TYPE ") {
                let name = rest.split(' ').next().expect("a name after # TYPE");
                assert!(
                    !declared.contains(&name),
                    "{name} was declared twice:\n{text}"
                );
                declared.push(name);
            } else if !line.starts_with('#') && !line.is_empty() {
                let family = family_of(line);
                assert!(
                    declared.contains(&family),
                    "a sample of {family} came before its # TYPE:\n{text}"
                );
            }
        }
        assert!(
            declared.iter().all(|name| name.starts_with("radio_scout_")),
            "every family is ours: {declared:?}"
        );
        assert!(text.ends_with('\n'), "the exposition ends in a newline");
    }

    /// **Every family says what it is, in words.** `# HELP` is what a Grafana
    /// author reads in the metric browser; a family with a `# TYPE` and no
    /// `# HELP` is a number nobody can tell from its neighbour.
    #[test]
    fn every_family_says_what_it_is() {
        let text = render(&busy());
        let helped: Vec<&str> = text
            .lines()
            .filter_map(|line| line.strip_prefix("# HELP "))
            .filter_map(|rest| rest.split(' ').next())
            .collect();

        for line in text.lines().filter_map(|line| line.strip_prefix("# TYPE ")) {
            let name = line.split(' ').next().expect("a name");
            assert!(helped.contains(&name), "{name} has no # HELP:\n{text}");
        }
    }

    /// **A label value is a recorder's string, and it must not be able to break
    /// the format.** A System's label is curated, but it is also imported from a
    /// CSV and auto-populated from an upload — a quotation mark in one would end
    /// the label early and turn the rest of the line into a syntax error that
    /// fails the whole scrape.
    #[test]
    fn a_label_value_cannot_break_the_format() {
        let mut status = busy();
        status.systems[0].label = Some("Say \"hi\"\\ \n now".into());

        let text = render(&status);
        let line = text
            .lines()
            .find(|line| line.starts_with("radio_scout_system_info"))
            .expect("a system sample");

        assert!(
            line.contains(r#"label="Say \"hi\"\\ \n now""#),
            "the label was not escaped: {line}"
        );
        assert_eq!(text.lines().count(), render(&busy()).lines().count());
    }

    /// The counts an Operator asked for, under the labels a dashboard reads
    /// them by. The point of the assertion is the *shape* — a family per
    /// vocabulary, the closed slug as the label value — because that is the
    /// contract a saved dashboard is written against.
    #[test]
    fn the_exposition_carries_what_the_document_carries() {
        let text = render(&busy());

        for expected in [
            r#"radio_scout_build_info{version="9.9.9"} 1"#,
            "radio_scout_uptime_seconds 3600",
            r#"radio_scout_ingest_total{outcome="stored"} 12"#,
            r#"radio_scout_ingest_total{outcome="invalid-api-key"} 0"#,
            r#"radio_scout_refused_total{reason="call-not-found"} 7"#,
            r#"radio_scout_errors_total{stage="dedup"} 1"#,
            r#"radio_scout_worker_in_hand{worker="enhancement"} 5"#,
            r#"radio_scout_worker_settled_total{worker="enhancement"} 41"#,
            r#"radio_scout_worker_running{worker="enhancement"} 0"#,
            r#"radio_scout_worker_running{worker="retention"} 1"#,
            "radio_scout_listeners 4",
            "radio_scout_calls 512",
            "radio_scout_audio_bytes 90000",
            "radio_scout_frozen_audio_bytes 1024",
            "radio_scout_audio_bytes_limit 2000",
            "radio_scout_retention_days 30",
            "radio_scout_storage_free_bytes 1000",
            "radio_scout_storage_total_bytes 4000",
            "radio_scout_oldest_call_timestamp_seconds 1699000000.500",
            // The name rides on an info family; the numbers carry the Ref
            // alone, so a rename does not mint a new series under them.
            r#"radio_scout_system_info{system="11",label="Fulton County"} 1"#,
            r#"radio_scout_system_calls{system="11"} 42"#,
            r#"radio_scout_system_last_call_timestamp_seconds{system="11"} 1700000003.000"#,
            r#"radio_scout_sink_queue_depth{sink="downstream"} 9"#,
            r#"radio_scout_sink_last_success_timestamp_seconds{sink="downstream"} 1700000002.000"#,
            r#"radio_scout_sink_failing{sink="downstream"} 1"#,
            r#"radio_scout_sinks{sink="downstream"} 2"#,
        ] {
            assert!(
                text.lines().any(|line| line == expected),
                "missing `{expected}`:\n{text}"
            );
        }
    }

    /// **Whatever a label value holds, the line stays one line and its value
    /// stays one value.** Asserted as a round trip rather than as a table,
    /// because the failure is not "this string escapes wrongly" — it is "some
    /// string somewhere escapes wrongly and fails the whole scrape", and only
    /// the general statement catches that.
    #[test]
    fn an_escaped_label_is_one_value_on_one_line() {
        proptest!(|(value in r#"[\\PC"\n\\\\]{0,40}"#)| {
            let escaped = escape(&value);

            prop_assert!(!escaped.contains('\n'), "a newline survived: {escaped:?}");
            // Every quote and backslash left in it is one we put there: walking
            // the escape sequences must consume the whole string and give back
            // exactly what went in.
            prop_assert_eq!(unescape(&escaped), value);
        });
    }

    /// The inverse of [`escape`], for the property above — a scraper's own
    /// reading of the three sequences the format defines.
    fn unescape(escaped: &str) -> String {
        let mut out = String::new();
        let mut characters = escaped.chars();
        while let Some(character) = characters.next() {
            match character {
                '\\' => match characters.next() {
                    Some('n') => out.push('\n'),
                    // Every other escape stands for the character itself.
                    // `extend` rather than a third arm, because a lone trailing
                    // backslash is something [`escape`] cannot emit — so an arm
                    // for it would be one no property could ever reach.
                    next => out.extend(next),
                },
                _ => out.push(character),
            }
        }
        out
    }

    /// Prometheus states timestamps in **seconds**, and a `_seconds` family
    /// rounded to the second is a family that quietly lost its milliseconds.
    /// The negative case is unreachable from a real Call and is what keeps the
    /// arithmetic total: `-1` must not render as `0.-001`.
    #[rstest]
    #[case(0, "0.000")]
    #[case(1_700_000_003_000, "1700000003.000")]
    #[case(1_699_000_000_500, "1699000000.500")]
    #[case(1, "0.001")]
    #[case(-1, "-1.999")]
    fn an_instant_renders_as_seconds_with_its_milliseconds(
        #[case] at_ms: i64,
        #[case] shown: &str,
    ) {
        assert_eq!(seconds(at_ms), shown);
    }

    /// **Free space is the disk's answer, and "no disk" is not "no room".**
    ///
    /// Two ways there is nothing to report and they must both be an *absence*:
    /// an S3 instance has no volume of its own, and a path that cannot be stated
    /// — a store not created yet, a mount that has gone — is a failure rather
    /// than a full disk. A zero for either is the one reading an Operator would
    /// act on in the middle of the night.
    #[test]
    fn a_volume_nobody_can_measure_is_absent_rather_than_full() {
        assert_eq!(room_left(None), StorageHealth::default());
        assert_eq!(
            room_left(Some(std::path::Path::new("/no/such/place/on/this/machine"))),
            StorageHealth::default()
        );

        let here = room_left(Some(std::path::Path::new(".")));

        assert!(here.total_bytes.expect("a volume") > 0);
        assert!(here.free_bytes.is_some());
    }

    /// **A blank token is refused, not read as "off".** Absent means the
    /// endpoint does not exist; `token = ""` would publish it behind a
    /// credential every scrape in the world already presents, which is the one
    /// way this could be switched on by accident.
    #[rstest]
    #[case("a-long-random-string", true)]
    #[case("", false)]
    #[case("   ", false)]
    fn only_a_token_that_could_protect_something_is_accepted(
        #[case] written: &str,
        #[case] accepted: bool,
    ) {
        assert_eq!(super::checked_token(written).is_ok(), accepted);

        let parsed: Result<MetricsConfig, _> = toml::from_str(&format!("token = \"{written}\""));

        assert_eq!(parsed.is_ok(), accepted);
        // The refusal names the key an Operator has to edit. There is
        // deliberately no assertion here about the *value* being absent from the
        // message (ADR-0011 rule 2): the only values this refuses are blank, so
        // there is nothing to leak and any such assertion would be vacuously
        // true. What keeps rule 2 is that a usable token is never refused at
        // all.
        if let Err(error) = parsed {
            assert!(error.to_string().contains("metrics.token"), "{error}");
        }
        // ...and what is accepted is what is stored, spaces and all removed.
        assert_eq!(
            super::checked_token(written).ok(),
            accepted.then(|| written.trim().to_owned())
        );
    }

    /// **A reading that was never taken is absent, never zero.** Free space on
    /// the S3 backend is somebody else's machine's business and an Instance with
    /// no size cap has no limit to report — and a `0` for either is a dashboard
    /// alarming about a disk that is full and a cap that permits nothing.
    #[test]
    fn a_reading_nobody_took_is_absent_rather_than_zero() {
        let mut status = busy();
        status.storage = StorageHealth::default();
        status.retention.max_size_bytes = None;
        status.archive.oldest_call_at_ms = None;
        status.systems[0].last_call_at_ms = None;
        status.systems[0].label = None;
        status.sinks[0].last_success_ms = None;

        let text = render(&status);

        for absent in [
            "radio_scout_storage_free_bytes",
            "radio_scout_storage_total_bytes",
            "radio_scout_audio_bytes_limit",
            "radio_scout_oldest_call_timestamp_seconds",
            "radio_scout_system_last_call_timestamp_seconds",
            "radio_scout_sink_last_success_timestamp_seconds",
        ] {
            assert!(
                !text.contains(absent),
                "{absent} was reported anyway:\n{text}"
            );
        }
        // The System is still there, under its Ref alone — an unlabeled System
        // is the ordinary state of a fresh Instance, not a reason to hide it.
        assert!(
            text.lines()
                .any(|line| line == r#"radio_scout_system_calls{system="11"} 42"#),
            "{text}"
        );
    }
}
