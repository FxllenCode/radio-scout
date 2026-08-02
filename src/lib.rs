//! Radio-Scout library crate.
//!
//! A real Call flows ingest -> blob store (ADR-0002) -> live-feed WebSocket ->
//! audio served back over HTTP with range support. `build_app` returns the Axum
//! router the binary serves and the integration harness drives in-process over
//! its real HTTP + WS boundary (ADR-0009).

pub mod admin;
pub mod archive;
pub mod audio_meta;
pub mod blob;
pub mod call;
pub mod catalog;
pub mod config;
pub mod db;
pub mod enhance;
pub mod failure;
pub mod http_log;
pub mod import;
pub mod ingest;
pub mod instance;
pub mod live;
pub mod logsink;
pub mod logview;
pub mod merge;
pub mod observability;
pub mod push;
pub mod query;
pub mod retention;
pub mod selection;
pub mod serve;
pub mod service;
pub mod startup;
pub mod web;
pub mod webpush;
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
use crate::push::Push;

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
    /// The Web Push surface: the server's VAPID identity, or nothing at all
    /// when push is unconfigured (#16).
    pub push: Push,
    /// The enhancement queue, or nothing at all when `[enhancement] mode` is
    /// `off` — which is what ships (#20).
    pub enhancer: Enhancer,
    /// What time it is, for everything a handler stamps or expires (#90).
    pub clock: Clock,
    /// What every background Worker owes right now (#93) — the reading half, so
    /// a status handler (#70) can serve depths it could never reach through the
    /// `Instance` that owns the handles.
    pub workers: crate::worker::Workers,
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
            push: Push::disabled(),
            enhancer: Enhancer::disabled(),
            clock: Clock::system(),
            workers: crate::worker::Workers::default(),
        }
    }

    /// Publish a stored Call to everything that follows the live-feed fanout.
    ///
    /// One method rather than a bare `live.publish`, because the fanout has two
    /// kinds of follower and only one of them can be counted from inside it. A
    /// socket is served and forgotten; the Web Push sender (#16) **owes** the
    /// Call until it has decided whether to notify, and that debt has to be
    /// taken on *here* — where there is still one owner — because the fanout
    /// hands every follower a clone and no clone can carry the ticket.
    ///
    /// Without it, "no notification was sent" could only ever be a sleep long
    /// enough to feel safe: the sender is idle both before it has seen a Call
    /// and after it has declined one, and nothing outside could tell which.
    pub fn publish(&self, call: Arc<crate::call::StoredCall>) {
        self.push.owes_a_call();
        self.live.publish(call);
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
        // Archive read surface (#13): search, its cascading filter options, and
        // per-Call download.
        .route("/api/calls", get(archive::search))
        .route("/api/calls/filters", get(archive::filters))
        // What a listener can select from (#12) — Systems + Talkgroups, whether
        // or not any of their Calls are still in the archive.
        .route("/api/catalog", get(catalog::catalog))
        // One Call, with the recorder detail a list deliberately doesn't carry
        // (#42). Declared before the `/audio` and `/download` children only for
        // readability — the router matches on the whole path, not on order.
        .route("/api/call/{id}", get(archive::detail))
        .route("/api/call/{id}/audio", get(serve::audio))
        .route("/api/call/{id}/download", get(archive::download))
        // Web Push (#16): the listener-facing half. Unauthenticated like the
        // rest of listening (ADR-0008) — what it grants is notifications to a
        // device that already holds the endpoint.
        .route("/api/push/key", get(push::key))
        .route("/api/push/subscribe", post(push::subscribe))
        .route("/api/push/unsubscribe", post(push::unsubscribe))
        // The way in to the admin surface, and the only route under
        // `/api/admin/` outside the session guard — there is no session yet.
        .route("/api/admin/login", post(admin::login))
        .merge(admin_routes(state.admin.clone()))
        .route("/healthz", get(healthz))
        // Everything else is the frontend: embedded SPA assets + client-side
        // routing (ADR-0007). The API/WS/health routes above take precedence.
        .fallback(web::spa_handler)
        // One line per request (#28), outermost so it sees every outcome —
        // including the 404s and 405s the router answers on its own. It carries
        // its own slice of state (the trust list, #17) rather than the whole of
        // it, because the layer is added before `with_state` and needs nothing
        // else.
        .layer(axum::middleware::from_fn_with_state(
            state.trusted_proxies.clone(),
            http_log::log_requests,
        ))
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
        .route(
            "/api/admin/talkgroups/import",
            post(import::import_talkgroups),
        )
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
/// it, Web Push coalesces on it. That is what makes those decisions testable —
/// "this Call is one hour old" is a fact a test can arrange, where "this Call
/// is one hour old *right now*" is a sleep.
///
/// A moment stopped at is all this offers. Moving a frozen clock forward is
/// what a test proving a *timeout* would want, and nothing needs one yet —
/// #93's idle signals and #94's reaping table are where that question actually
/// arises, so the knob belongs with whichever of them turns out to need it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock(Option<i64>);

impl Clock {
    /// The machine's clock.
    pub fn system() -> Self {
        Clock(None)
    }

    /// A clock stopped at `at_ms`.
    pub fn frozen(at_ms: i64) -> Self {
        Clock(Some(at_ms))
    }

    /// What time it is, in unix milliseconds.
    pub fn now_ms(&self) -> i64 {
        self.0.unwrap_or_else(now_ms)
    }
}

impl Default for Clock {
    fn default() -> Self {
        Clock::system()
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
}
