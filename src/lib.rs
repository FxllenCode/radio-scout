//! Radio-Scout library crate.
//!
//! A real Call flows ingest -> blob store (ADR-0002) -> live-feed WebSocket ->
//! audio served back over HTTP with range support. `build_app` returns the Axum
//! router the binary serves and the integration harness drives in-process over
//! its real HTTP + WS boundary (ADR-0009).

pub mod access;
pub mod activity;
pub mod admin;
pub mod archive;
pub mod audio_meta;
pub mod blob;
pub mod call;
pub mod catalog;
pub mod config;
pub mod curate;
pub mod db;
pub mod delay;
pub mod delivery;
pub mod dirwatch;
pub mod downstream;
pub mod embed;
pub mod enhance;
pub mod event;
pub mod export;
pub mod failure;
pub mod framing;
pub mod http_log;
pub mod import;
pub mod ingest;
pub mod instance;
pub mod listeners;
pub mod live;
pub mod logsink;
pub mod logview;
pub mod merge;
pub mod metrics;
pub mod mining;
pub mod observability;
pub mod query;
pub mod quiet;
pub mod recorder;
pub mod retention;
pub mod rf;
pub mod secret;
pub mod selection;
pub mod serve;
pub mod service;
pub mod share;
pub mod star;
pub mod startup;
pub mod station;
pub mod tone;
pub mod web;
pub mod webhook;
pub mod worker;

#[cfg(test)]
mod testing;

use std::sync::Arc;

use crate::db::Db;
use axum::Router;
use axum::routing::{any, get, post};

use crate::admin::AdminAuth;
use crate::blob::AudioStore;
use crate::config::TrustedProxies;
use crate::enhance::Enhancer;
use crate::live::LiveFeed;

// Re-exported so the binary and the integration harness can wire the app up
// without reaching into module paths.
pub use crate::blob::{BlobStore, S3Config, StorageConfig};
pub use crate::ingest::IngestConfig;

/// Shared application state, cloned into every handler. All fields are cheap to
/// clone (Arc / channel / DB pool handle).
#[derive(Clone)]
pub struct AppState {
    pub audio: Arc<dyn AudioStore>,
    pub db: Db,
    pub live: LiveFeed,
    pub ingest: IngestConfig,
    /// Whose `X-Forwarded-For` the request log may believe (#17). Empty — the
    /// shipped default — means nobody's.
    pub trusted_proxies: TrustedProxies,
    /// The admin surface's credential and its live sessions (#19).
    pub admin: AdminAuth,
    /// The enhancement queue, or nothing at all when `[enhancement] mode` is
    /// `off` — which is what ships (#20).
    pub enhancer: Enhancer,
    /// How the **Mining** sweep walks the Archive that was already there (#48).
    pub mining: crate::mining::MiningConfig,
    /// Forwarding to **Downstream** peers (#52) — the policy and the sender's
    /// wake-up. Unlike enhancement there is no disabled form: a peer is a row,
    /// so an Instance with none has an empty roster rather than a feature
    /// switched off.
    pub downstreams: crate::downstream::Downstreams,
    /// Posting marked Calls to an Operator's own URLs (#54) — the policy, the
    /// sender's wake-up, and where this Instance can be reached from outside.
    /// A Webhook is a **row**, so an Instance with none has an empty roster
    /// rather than a feature switched off.
    pub webhooks: crate::webhook::Webhooks,
    /// Looking at a Call's audio for a **Tone profile** page (#55) — the
    /// detection queue and whether there is anything to look for. A profile is
    /// a **row**, so like a Webhook and unlike enhancement there is no disabled
    /// form: an Instance with none has an empty roster.
    pub tones: crate::tone::Tones,
    /// Looking at a Call's audio for the gaps **Catch-up** skips (#59) — the
    /// scan queue and whether this Instance scans at all. Unlike the four above
    /// it has no roster and no per-Call gate: whether a Call holds a gap can
    /// only be answered by looking, so the switch is `[quiet] enabled` and
    /// nothing else.
    pub quiet: crate::quiet::Quiet,
    /// Expiring public links to a single Call (#64, spec US 32) — the policy,
    /// and where this Instance can be reached from outside, which is what a
    /// preview card's absolute URLs are built on. A link is a **row**, so like
    /// a Webhook there is no disabled form beyond the switch itself.
    pub shares: crate::share::Shares,
    /// What a **Star** is worth here (#66, spec US 37) — one field off
    /// `[retention] starred_days`, so the catalog can say whether a Star
    /// outlives the retention window without the whole policy in hand.
    pub stars: crate::star::Stars,
    /// Taking a range of the Archive away with you (#65, spec US 33) — the
    /// policy, and the right to be the one export that is running. Not a
    /// roster and not a queue: the only state an export has is whether one is
    /// already in flight.
    pub exports: crate::export::Exports,
    /// The scanner as a radio station (#74, spec US 60) — the policy, and the
    /// slots a stream holds for as long as somebody is listening to it.
    pub stations: crate::station::Stations,
    /// How many people are listening (#62, spec US 41) — the count a live-feed
    /// connection joins, and the one a status page (#70) reads. Counts only:
    /// there is nothing in it that could name anybody.
    pub listeners: crate::listeners::Listeners,
    /// Who may hear what (#68, spec US 52) — whether this Instance gates any
    /// channel at all, and what a presented **grant** resolves to. Like a
    /// Webhook and unlike enhancement there is no disabled form: an **Access
    /// code** is a row and a gate is a column, so an Instance with neither has
    /// nothing switched off and pays nothing.
    pub access: crate::access::Access,
    /// What time it is, for everything a handler stamps or expires (#90).
    pub clock: Clock,
    /// What every background Worker owes right now (#93) — the reading half, so
    /// a status handler (#70) can serve depths it could never reach through the
    /// `Instance` that owns the handles.
    pub workers: crate::worker::Workers,
    /// What the **Recorder**s dialled into this Instance are doing right now
    /// (#71, spec US 50) — a live view, held in memory and never written down.
    /// Like a Webhook and unlike enhancement there is no disabled form: a
    /// Recorder is a *connection*, so an Instance with none has an empty roster.
    pub recorders: crate::recorder::Recorders,
    /// What this Instance has been doing (#70, spec US 48–49) — the counters
    /// behind the status page and the Prometheus text, and `[metrics]`' own
    /// token, which is the switch that decides whether the second one is served
    /// at all.
    pub metrics: crate::metrics::Metrics,
    /// Ingesting what Recorders drop into folders (#72) — the Worker's inbox,
    /// the roots every watch is bounded by, and what each watch is doing. A
    /// watch is a **row**, so an Instance with none has an empty roster.
    pub dirwatch: crate::dirwatch::Dirwatch,
    /// Publishing a Call late, on purpose (#73, spec US 62) — whether anything
    /// is delayed at all, and the release Worker's wake-up. A Delay is a
    /// **column**, so an Instance with none has nothing switched off and pays
    /// nothing.
    pub delays: crate::delay::Delays,
}

impl AppState {
    /// Assemble state from a blob store, a database connection, and ingest
    /// config, with a fresh live-feed hub, no trusted proxies, and an admin
    /// surface nothing can authenticate to until a password is provisioned.
    pub fn new(audio: Arc<dyn AudioStore>, db: Db, ingest: IngestConfig) -> Self {
        AppState {
            audio,
            db,
            live: LiveFeed::new(),
            ingest,
            trusted_proxies: TrustedProxies::default(),
            admin: AdminAuth::locked(),
            enhancer: Enhancer::disabled(),
            mining: crate::mining::MiningConfig::default(),
            downstreams: crate::downstream::Downstreams::default(),
            webhooks: crate::webhook::Webhooks::default(),
            tones: crate::tone::Tones::default(),
            quiet: crate::quiet::Quiet::default(),
            shares: crate::share::Shares::default(),
            stars: crate::star::Stars::default(),
            exports: crate::export::Exports::default(),
            stations: crate::station::Stations::default(),
            listeners: crate::listeners::Listeners::default(),
            access: crate::access::Access::default(),
            clock: Clock::system(),
            workers: crate::worker::Workers::default(),
            recorders: crate::recorder::Recorders::default(),
            metrics: crate::metrics::Metrics::default(),
            dirwatch: crate::dirwatch::Dirwatch::default(),
            delays: crate::delay::Delays::default(),
        }
    }

    /// **Something about a channel changed**: re-read whether anything is
    /// gated (#68) and whether anything is delayed (#73), on the request that
    /// changed it, and move the Calls already waiting onto the Delay now in
    /// force.
    ///
    /// One call for every curation write that can touch either — a System or a
    /// Talkgroup created, edited or removed, a configuration document restored,
    /// a channel folded — rather than two calls repeated beside each of them,
    /// because a site that remembered one and not the other would be a gate or
    /// a Delay that applies from the next restart instead of the next request.
    /// Both bits are cached on the same terms: stale-`true` costs a clause,
    /// stale-`false` is a leak, so neither may wait for a timer.
    pub async fn channels_changed(&self) {
        self.access.rearm(&self.db).await;
        self.delays.reconsider(&self.db).await;
    }

    /// **Emit** a stored Call: give it its place in the emission sequence,
    /// record that on the row, and hand it to everything that follows the
    /// live-feed fanout.
    ///
    /// One method rather than a bare `live.publish`, because this is where a
    /// Call stops being merely *stored* and becomes
    /// *emitted* (#94). Ingest reaches here a breath after the insert. A
    /// **Delay**ed Call (#73) does not: its emission *is* its release, so it is
    /// written in the transaction that owes it to every sink
    /// (`crate::delay::worker`), where a failed stamp must keep the Call back
    /// rather than let it out unrecorded. Either way the emission is allocated
    /// and written down at the moment the Call goes out, which is what makes a
    /// **Backfill** replayable in the order Listeners actually heard things.
    ///
    /// A failed stamp **degrades rather than fails**: everyone connected still
    /// hears the Call, and the row keeps `emitted_seq = NULL`, which reads as
    /// "not emitted" and so is left out of Backfills. Refusing to deliver a Call
    /// that is already stored, over a bookkeeping write, would cost the Listener
    /// far more than the missed replay does.
    pub async fn publish(&self, call: Arc<crate::call::StoredCall>) {
        let emitted = crate::live::Emitted {
            seq: self.live.next_emission(),
            call,
        };
        if let Err(error) = db::repo::emit_call(&self.db, emitted.call.id, emitted.seq).await {
            // Never silent: a Call missing from one Listener's Backfill and
            // present in everyone else's is a bug that can only ever be
            // reported as "some calls go missing sometimes" (ADR-0011 rule 3).
            tracing::warn!(
                %error,
                call_id = emitted.call.id,
                seq = emitted.seq,
                "emission could not be recorded"
            );
        }
        self.live.publish(emitted);
    }
}

/// Build the Axum application: the ingest endpoint, the live-feed WebSocket, and
/// audio serving. This is the single seam the binary and tests share.
pub fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/api/call-upload", post(ingest::call_upload))
        .route(
            "/api/trunk-recorder-call-upload",
            post(ingest::trunk_recorder_call_upload),
        )
        .route("/api/live", any(live::ws_handler))
        // **Where a Recorder dials in** (#71, spec US 50). Outside
        // `/api/admin/` because a recorder holds no admin session — it presents
        // the same **API key** it uploads with, in the query string, which is
        // the only credential Trunk Recorder's bare `statusServer` URL can
        // carry and the one place `http_log` never writes down (ADR-0011 rule
        // 2). The dashboard it feeds is admin-gated; this is the recorder's
        // door.
        .route("/api/recorder-status", any(recorder::ws::ws_handler))
        // Archive read surface (#13): search, its cascading filter options, and
        // per-Call download.
        .route("/api/calls", get(archive::search))
        .route("/api/calls/filters", get(archive::filters))
        .route("/api/calls/quiet", get(archive::quiet))
        // A range of the Archive as a file (#65, spec US 33) — the same
        // filters, streamed as a zip of Calls or as one stitched WAV.
        .route("/api/calls/export", get(export::export))
        // How busy the Archive was under a search (#62, spec US 34–35) —
        // the density ribbon over its results, and the hour-by-day heatmap.
        .route("/api/calls/activity", get(archive::activity))
        // **The scanner as a radio station** (#74, spec US 60): a Selection as
        // one endless MP3, for a speaker, a car or VLC. Under `/api` so the
        // service worker never answers it from a cache, and named `.mp3`
        // because some players decide what a URL is by how it ends.
        .route(crate::station::STATION_PATH, get(crate::station::stream))
        // What a listener can select from (#12) — Systems + Talkgroups, whether
        // or not any of their Calls are still in the archive.
        .route("/api/catalog", get(catalog::catalog))
        // One Call, with the recorder detail a list deliberately doesn't carry
        // (#42). Declared before the `/audio` and `/download` children only for
        // readability — the router matches on the whole path, not on order.
        .route("/api/call/{id}", get(archive::detail))
        // One radio's history (#47, spec US 44) — where it talks and since
        // when. A Ref is unique only within its System, so both ride the path.
        .route("/api/unit/{system}/{ref}", get(archive::unit))
        .route("/api/call/{id}/audio", get(serve::audio))
        // Minting a Call's expiring public link (#64, spec US 32). Listener-
        // facing and unauthenticated, because a Listener holds no credential —
        // the abuse bound is one live link per Call, not a gate.
        .route("/api/call/{id}/share", post(share::create))
        // Two verbs on one path, and no credential on either: a **Listener**
        // holds none, and the mark they leave is the Instance's (#66).
        .route("/api/call/{id}/star", post(star::star).delete(star::unstar))
        .route("/api/call/{id}/download", get(archive::download))
        // Proving you know an **Access code** (#68, spec US 52). Listener-facing
        // and unauthenticated for the reason minting a share link is: a Listener
        // holds no credential. What bounds it is a per-address **Lockout**, the
        // admin login's own.
        .route("/api/unlock", post(access::unlock))
        // The way in to the admin surface, and the only route under
        // `/api/admin/` outside the session guard — there is no session yet.
        .route("/api/admin/login", post(admin::login))
        .merge(admin_routes(state.admin.clone()))
        .route("/healthz", get(healthz))
        // **The Prometheus surface** (#70, spec US 49), outside `/api` because
        // it is not this app's API — it is the one URL a third party is
        // configured with, and `/metrics` is what every Prometheus example in
        // the world already says. Always routed, never conditionally: a route
        // registered only when a token is set would let the SPA fallback answer
        // `/metrics` with the app's own HTML and a `200`, which is worse than
        // any refusal. Its gate is the token, checked in the handler.
        .route(metrics::METRICS_PATH, get(metrics::expose))
        // **The share surface, outside `/api` on purpose** (#64, spec US 32): it
        // is a page a stranger opens, and a short URL is the thing being pasted
        // into a message. The token rides the *query string* rather than the
        // path because `http_log` logs paths and never queries — so "a share
        // token is never logged" is true by construction (ADR-0011 rule 2).
        .route(share::SHARE_PATH, get(share::open))
        .route(share::SHARE_AUDIO_PATH, get(share::audio))
        // **An Event's share surface** (#67, spec US 38), a sibling of `/s` and
        // outside `/api` for its reason: a page a stranger opens, with the token
        // in the query string so `http_log` never writes it down. Three routes,
        // because an incident is a list — the page, one member's bytes, and the
        // whole thing as a file.
        .route(crate::event::EVENT_PATH, get(crate::event::open))
        .route(crate::event::EVENT_AUDIO_PATH, get(crate::event::audio))
        .route(crate::event::EVENT_EXPORT_PATH, get(crate::event::export))
        // **The embeddable player** (#75, spec US 59): the one page another
        // site may frame, and what it reads. Outside `/api` for the share page's
        // reason — it is the URL in a snippet — with the token in the query
        // string, where `http_log` never looks. The feed is under `/api` so the
        // service worker never answers it from a cache.
        .route(crate::embed::EMBED_PATH, get(crate::embed::page))
        .route(crate::embed::FEED_PATH, get(crate::embed::feed))
        // Everything else is the frontend: embedded SPA assets + client-side
        // routing (ADR-0007). The API/WS/health routes above take precedence.
        .fallback(web::spa_handler)
        // One line per request (#28), outermost so it sees every outcome —
        // including the 404s and 405s the router answers on its own. It carries
        // its own slice of state (the trust list, #17) rather than the whole of
        // it, because the layer is added before `with_state` and needs nothing
        // else.
        .layer(axum::middleware::from_fn_with_state(
            http_log::Watching {
                trusted_proxies: state.trusted_proxies.clone(),
                metrics: state.metrics.clone(),
            },
            http_log::log_requests,
        ))
        // **Who may frame what** (#75): every response says, and only the
        // embed page says "anybody" (`crate::framing`). Outermost of all — over
        // the router's own 404s and 405s, and over the request log too, because
        // that is where a 5xx is rebuilt with nothing in it but its request id,
        // and a header written inside it would be thrown away with the body.
        .layer(axum::middleware::map_response(framing::apply))
        .with_state(state)
}

/// Everything under `/api/admin/` that mutates or reveals configuration.
///
/// One router, so the session guard #19 puts over it is a **prefix layer** and
/// not a decoration each handler has to remember: a route added here is gated
/// by default, and a route that must not be — `/api/admin/login` — has to be
/// written outside on purpose.
fn admin_routes(admin: AdminAuth) -> Router<AppState> {
    Router::new()
        .route("/api/admin/session", get(admin::session))
        .route("/api/admin/logout", post(admin::logout))
        // The operator log surface (#30): what the server has been saying, for
        // an operator who has no shell to read `journalctl` from.
        .route("/api/admin/logs", get(logview::search))
        // Peak Listeners over time (#62, spec US 41). The one chart that is not
        // a Listener's — how many people take an open archive up is the
        // Operator's business, and it is counts alone either way.
        .route("/api/admin/listeners", get(listeners::history))
        // Is this Instance healthy (#70, spec US 48) — one document, refreshed
        // live. Behind the session for `listeners::history`'s reason: how an
        // Operator's Instance is doing is the Operator's own business.
        .route("/api/admin/status", get(metrics::status))
        // What the SDRs are doing right now (#71, spec US 50), and how each
        // frequency has been receiving (US 51). Behind the session for
        // `listeners::history`'s reason: an Operator's own receivers are the
        // Operator's business.
        .route("/api/admin/recorders", get(recorder::ws::dashboard))
        .route("/api/admin/recorders/health", get(rf::health))
        .route(
            "/api/admin/talkgroups/import",
            post(import::import_talkgroups),
        )
        // A fleet's numbering scheme in one paste (#47, spec US 43).
        .route("/api/admin/units/import", post(import::import_unit_csv))
        // Running the Instance from a browser (#49, spec US 45–46): Systems,
        // Talkgroups, Groups, Tags, Units and API keys. Merged rather than
        // written out here, so the whole surface is gated by the one
        // `route_layer` below — which is the property a route added to
        // `curate::routes` inherits without knowing it exists.
        .merge(curate::routes())
        // `route_layer`, not `layer`: it runs only for paths this router
        // matched, so an unrouted URL still 404s rather than being told to log
        // in first — which would turn the guard into a map of what exists.
        .route_layer(axum::middleware::from_fn_with_state(
            admin,
            admin::require_session,
        ))
}

/// `GET /healthz` — liveness probe.
async fn healthz() -> &'static str {
    "ok"
}

/// Wall-clock time in unix milliseconds — the crate's one clock reading.
///
/// Milliseconds since the epoch is how every timestamp is stored and compared
/// (dialect-agnostic, see `db::entities::call`). A clock before 1970 reads as 0
/// rather than panicking; nothing here is worth killing a scanner over.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since_epoch| since_epoch.as_millis() as i64)
        .unwrap_or(0)
}

/// What time it is, as an **input** rather than a global (#90).
///
/// An Instance is wired with one and everything it assembles reads it: ingest
/// stamps a Call with it, the Retention sweeper decides what has aged out by
/// it, sessions expire on it. That is what makes those decisions testable —
/// "this Call is one hour old" is a fact a test can arrange, where "this Call
/// is one hour old *right now*" is a sleep.
///
/// # A clock that stands still until it is moved (#73)
///
/// [`Clock::frozen`] stops time; [`Clock::advance`] passes it. The **Delay**
/// policy is the first thing whose subject *is* time passing — a Call that must
/// not be published now and must be published ten minutes from now — so it is
/// the first thing that needed both halves, and #90's note that the knob
/// "belongs with whichever of them turns out to need it" is answered here.
///
/// Moving a clock is only half of what a Worker waiting on one needs: it also
/// has to *wake up*. [`Clock::sleep_until`] is that half, and it is the reason
/// the frozen arm holds a `watch` rather than a number — a Worker asleep on a
/// frozen clock is woken by the advance itself, the same instant every reader
/// of [`Clock::now_ms`] sees the new time. Every clone shares the one instant,
/// which is what lets the Instance, its Workers and a test all agree on it
/// across a restart.
#[derive(Clone, Debug, Default)]
pub struct Clock(Option<Arc<tokio::sync::watch::Sender<i64>>>);

/// The longest [`Clock::sleep_until`] sleeps on the machine's clock before
/// returning to let its caller look again.
///
/// The sleep is monotonic and the instant it is aiming for is wall-clock, and
/// the two part company whenever NTP steps the wall clock. Waking at least this
/// often bounds how wrong a long sleep can be to one nap — a **Delay** released
/// a minute late at worst, never early.
const LONGEST_NAP: std::time::Duration = std::time::Duration::from_secs(60);

impl Clock {
    /// The machine's clock.
    pub fn system() -> Self {
        Clock(None)
    }

    /// A clock stopped at `at_ms`, until [`Clock::advance`] moves it.
    pub fn frozen(at_ms: i64) -> Self {
        Clock(Some(Arc::new(tokio::sync::watch::Sender::new(at_ms))))
    }

    /// What time it is, in unix milliseconds.
    pub fn now_ms(&self) -> i64 {
        match &self.0 {
            Some(stopped) => *stopped.borrow(),
            None => now_ms(),
        }
    }

    /// Move a frozen clock forward by `by`, waking anything asleep on it.
    ///
    /// The machine's clock is not this process's to move, so on that one this
    /// does nothing — and a test that called it would see nothing happen, which
    /// is the honest failure.
    pub fn advance(&self, by: std::time::Duration) {
        if let Some(stopped) = &self.0 {
            let by_ms = i64::try_from(by.as_millis()).unwrap_or(i64::MAX);
            stopped.send_modify(|now| *now = now.saturating_add(by_ms));
        }
    }

    /// Resolve no later than `at_ms` on this clock — and possibly earlier, so a
    /// caller looks at the time again rather than trusting it has come.
    ///
    /// On the machine's clock that is a monotonic sleep of at most
    /// [`LONGEST_NAP`]; on a frozen one it is the moment [`Clock::advance`]
    /// reaches `at_ms`, and never otherwise.
    pub async fn sleep_until(&self, at_ms: i64) {
        match &self.0 {
            Some(stopped) => {
                // `Err` is a dropped sender, which cannot happen: it lives in
                // the `Arc` this borrow is holding.
                let _ = stopped.subscribe().wait_for(|now| *now >= at_ms).await;
            }
            None => {
                let wait = u64::try_from(at_ms.saturating_sub(now_ms())).unwrap_or(0);
                tokio::time::sleep(std::time::Duration::from_millis(wait).min(LONGEST_NAP)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frozen clock stays where it was put — which is what lets a test say
    /// "this Call is an hour old" instead of sleeping for an hour — and the
    /// default is the machine's, so a scanner is unaffected by this existing.
    #[test]
    fn a_frozen_clock_stays_put_and_the_default_does_not() {
        assert_eq!(Clock::frozen(1_000).now_ms(), 1_000);

        let real = Clock::default().now_ms();

        assert!(real > 1_700_000_000_000, "that is not a real wall clock");
    }

    /// Advancing passes time for **every** clone at once — the Instance, its
    /// Workers and the test that moved it all read one instant (#73).
    #[test]
    fn advancing_a_frozen_clock_moves_every_clone_of_it() {
        let clock = Clock::frozen(1_000);
        let held_elsewhere = clock.clone();

        clock.advance(std::time::Duration::from_millis(250));

        assert_eq!(clock.now_ms(), 1_250);
        assert_eq!(held_elsewhere.now_ms(), 1_250);
    }

    /// ...and the machine's clock is not this process's to move: advancing it
    /// does nothing, which is the honest failure for a test that tried.
    #[test]
    fn the_machines_clock_does_not_advance() {
        let clock = Clock::system();

        clock.advance(std::time::Duration::from_secs(24 * 60 * 60));

        assert!(
            clock.now_ms() < now_ms() + 60 * 60 * 1000,
            "a day did not pass"
        );
    }

    /// A sleep on a frozen clock ends when an advance reaches its instant — not
    /// before, and not only on an exact hit.
    #[tokio::test]
    async fn a_sleep_on_a_frozen_clock_ends_when_time_reaches_it() {
        let clock = Clock::frozen(1_000);
        let mut sleeping = Box::pin(clock.sleep_until(2_000));

        clock.advance(std::time::Duration::from_millis(999));
        assert!(
            futures_util::poll!(sleeping.as_mut()).is_pending(),
            "one millisecond short is still asleep"
        );

        clock.advance(std::time::Duration::from_millis(5));
        tokio::time::timeout(std::time::Duration::from_secs(5), sleeping)
            .await
            .expect("an advance past the instant ends the sleep");
    }

    /// The machine's clock sleeps for what is left, capped at one nap — an
    /// instant already past ends the sleep at once.
    #[tokio::test(start_paused = true)]
    async fn a_sleep_on_the_machines_clock_is_at_most_one_nap() {
        let started = tokio::time::Instant::now();
        Clock::system().sleep_until(now_ms() - 1_000).await;
        assert_eq!(started.elapsed(), std::time::Duration::ZERO, "already due");

        let started = tokio::time::Instant::now();
        Clock::system().sleep_until(i64::MAX).await;
        assert_eq!(started.elapsed(), LONGEST_NAP, "never longer than a nap");
    }
}
