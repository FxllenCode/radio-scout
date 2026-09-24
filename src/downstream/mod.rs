//! **Downstream** forwarding (#52, spec US 1–2): the Calls this Instance sends
//! on to its rdio-compatible peers.
//!
//! Forwarding only. *Receiving* a peer's downstream is [`crate::ingest`] with an
//! **API key** and needs nothing built, which is why this module has no inbound
//! half.
//!
//! # The shape
//!
//! - [`Downstream`] is a configured peer as the sender uses it. Its scope is a
//!   [`Selection`] — **the same type and the same rule** the live feed is scoped
//!   by ([`Selection::reaches_channels`]) — and [`routed_to`] is the routing
//!   decision, purely.
//! - A delivery is **enqueued inside the transaction that stores the Call**
//!   ([`crate::db::repo::queue_deliveries`]), so "the Call exists" and "the
//!   Call is owed to this peer" are one fact a crash cannot separate.
//! - [`sender`] drains that queue: per peer, head first, one attempt in flight,
//!   with a backoff between tries.
//! - [`dialect`] builds the rdio multipart body, and is pure.
//!
//! **The queue itself is [`crate::delivery`]**, shared with [`crate::webhook`]
//! (#54): the retry policy, the ordering rule, the accounting and the loop are
//! one implementation, and what belongs to this module is the errand — the rdio
//! dialect, the audio object, and what a peer's scope means.
//!
//! # Improving on rdio-scanner
//!
//! rdio has this feature (`server/downstream.go`) and every difference below is
//! a defect it has, found by reading it:
//!
//! | rdio | Radio-Scout |
//! |---|---|
//! | POSTs inline on the ingest goroutine; an unreachable peer is logged and the Call **dropped** (`downstream.go:416`) | a durable queue, retried with backoff — an outage costs delay, not Calls |
//! | scope tests `call.Talkgroup.TalkgroupRef` only (`downstream.go:99`), so a **Patch** never reaches a peer subscribed to the patched channel | [`Selection::reaches_channels`], which walks the patches — the live feed's own rule |
//! | serializes `units`/`frequencies` with Go's **default struct-field names** (`CallUnit` has no JSON tags), which its own ingest parser does not read | the field names its parser actually reads, pinned by a fixture *and* by a real forward between two Instances |
//! | sends 14 fields, omitting `site` and `frequency` that its parser accepts | every field [`crate::ingest::call_upload`] reads, in an order that survives its parser |
//! | stores the peer's key in plaintext **and returns it** from the admin API | stored of necessity, never returned and never logged |
//!
//! The `units`/`frequencies` one is worth stating plainly because it is
//! invisible from either end: an rdio peer receives `[{"Id":0,"CallId":0,
//! "Offset":1.5,"UnitRef":4424000}]` and parses `id`/`label`/`offset` out of it,
//! finding none of them. Every radio and every frequency sample is silently lost
//! on every forwarded Call, and the receiving instance looks perfectly healthy.
//!
//! # What the dialect cannot carry
//!
//! The rdio upload dialect has no field for the **Emergency** flag or for
//! encryption — only Trunk Recorder's native meta does ([`crate::ingest`]) — so
//! neither survives a forward, to us or to rdio. An **Encrypted Call** is
//! therefore never queued at all: it has no audio object, and the dialect
//! requires one.
//!
//! That gap is also why a **Webhook** (#54) is not a Downstream with a different
//! body: the one fact a Webhook exists to carry is the very fact this dialect
//! has no field for.
//!
//! # Design notes (moved verbatim from CLAUDE.md, #110)
//!
//! **A peer's outage costs delay, not Calls, and the queue is written where the Call is (#52, spec US 1–2).** `src/downstream/` is forwarding to rdio-compatible peers: `dialect` writes the upload contract `crate::ingest` has read since #5, `sender` is the Worker that drains a durable queue, and `Downstream`/`routed_to`/`DownstreamConfig` are the policy. rdio has this feature (`server/downstream.go`) and **every difference below is a defect it has**, found by reading it. Six things follow. **A delivery row is written inside the transaction that stores the Call** (`repo::queue_deliveries`, handed the ids `downstream::routed_to` decided — the Selection is never compared inside a write, which is what keeps a domain module out of the data layer), so "the Call exists" and "the Call is owed to this peer" are one fact a crash cannot separate — where rdio POSTs inline on the ingest goroutine and, when the peer is unreachable, logs and **drops the Call** (`downstream.go:416`). It costs ingest exactly **one statement**, the roster read, which is why `tests/ingest.rs`'s pinned count went 18 → 19 rather than sliding under a ceiling; a cached roster would have saved it and bought an invalidation problem with five call sites. **The scope is a `Selection`** — the same type and `Selection::reaches_channels`, which is the live feed's own rule, so a **Patch** reaches a peer subscribed to the channel it was patched onto. rdio's `HasAccess` compares `call.Talkgroup.TalkgroupRef` and stops (`downstream.go:99`), so a patched transmission reaches nobody. **The dialect is a superset of rdio's own forwarder, and its arrays are the ones the receiving parser reads**: `CallUnit` and `CallFrequency` carry no JSON tags, so rdio marshals `{"Id":…,"CallId":…,"Offset":…,"UnitRef":…}` and its own `case "units"` looks for `id`/`label`/`offset` — **every radio and every frequency sample is silently lost on every forwarded Call**, at both ends. Ours adds `site` and `frequency` (which its parser accepts and its forwarder never sends) — and **`frequency` is sent *before* `frequencies`, because rdio's parser assigns rather than appends there (`parsers.go:318`), so the other order makes an rdio peer discard every sample it just read**. That one is invisible to the two-Instance forward, since our own ingest keeps the two in separate columns, and is pinned by the snapshot; the rest is pinned by both, because a committed fixture stays green when the side that *reads* it moves. **Only the head of a peer's queue is ever attempted, and a head that is not due is waited for rather than skipped** — that is what makes in-order draining a property rather than a sort — with one attempt in flight per peer, so a peer taking the full timeout to fail delays nothing but its own backlog. **The retry policy turns on one question: will these same bytes ever be accepted?** A body-level refusal (400/413/415/417/422 — rdio's own `Incomplete call data` among them) abandons, because retrying it blocks every Call behind it forever; everything else retries, including `401`, because a mistyped key is a thing an Operator fixes and the backlog is what makes fixing it worth doing. And **the peer's key is stored recoverably and never leaves**: a hash cannot be POSTed, so it is kept — but `DownstreamRow` has no field for it, a `PATCH` omitting it keeps the stored one, and it is never logged. rdio stores it plaintext *and returns it* from the admin API, and puts it in the exported configuration document.

pub mod dialect;
pub mod sender;

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::delivery::{Dispatcher, Retry};
use crate::selection::Selection;

/// What this Worker is called on a status surface (#93, #70).
pub const WORKER: &str = "downstream";

/// The path a Downstream's base URL is joined with — rdio's own forwarder
/// appends the same one (`downstream.go:316`), so an Operator configures the
/// address they would give a recorder.
pub const UPLOAD_PATH: &str = "/api/call-upload";

/// How forwarding behaves — and the `[downstream]` section itself (ADR-0012,
/// #87).
///
/// Policy only. The peers themselves are **Curation** (CONTEXT.md): rows an
/// Operator edits in the browser, never anything written in `radio-scout.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DownstreamConfig {
    /// How long a peer has to answer one delivery before it counts as failed.
    #[serde(rename = "timeout_secs", with = "crate::config::secs")]
    pub timeout: Duration,
    /// How long to wait before the first retry. Doubles per attempt up to
    /// [`DownstreamConfig::retry_max`].
    #[serde(rename = "retry_initial_secs", with = "crate::config::secs")]
    pub retry_initial: Duration,
    /// The ceiling on that doubling — how often a peer that has been down for
    /// hours is poked.
    #[serde(rename = "retry_max_secs", with = "crate::config::secs")]
    pub retry_max: Duration,
}

impl Default for DownstreamConfig {
    fn default() -> Self {
        DownstreamConfig {
            // rdio's own downstream client timeout, and a reasonable one: a peer
            // on the other side of a domestic uplink receiving a minute of audio
            // needs more than a browser's few seconds.
            timeout: Duration::from_secs(30),
            // Long enough that a peer restarting is not hammered, short enough
            // that a blip costs one Call's worth of delay.
            retry_initial: Duration::from_secs(5),
            // **The ceiling is also the recovery latency**, which is the number
            // that matters: a peer down overnight is polled once a minute rather
            // than thousands of times, *and* its backlog starts moving within a
            // minute of it returning. A longer cap saves a poll an hour and
            // costs every one of those Calls the same delay again.
            retry_max: Duration::from_secs(60),
        }
    }
}

impl DownstreamConfig {
    /// How hard a peer is tried again — this section's two retry settings as
    /// the shared queue reads them ([`Retry`], where the doubling itself lives
    /// and is tested).
    pub fn retry(&self) -> Retry {
        Retry {
            initial: self.retry_initial,
            max: self.retry_max,
        }
    }
}

/// A configured Downstream, as the sender uses one.
///
/// The scope arrives parsed rather than as the JSON the row stores, because
/// [`crate::selection::stored`] decides what an unreadable one means once, where it can be said
/// out loud.
///
/// **`Downstream` is CONTEXT.md's word, and this is the type for it.** The
/// harness's `common::Peer` is deliberately something else: this is *our
/// configuration of* the far end, and that is the far end itself.
#[derive(Debug, Clone, PartialEq)]
pub struct Downstream {
    pub id: i64,
    pub url: String,
    pub api_key: String,
    pub scope: Selection,
}

impl crate::delivery::Sink for Downstream {
    fn id(&self) -> i64 {
        self.id
    }
}

impl Downstream {
    /// Read a stored row.
    pub fn from_row(row: &crate::db::entities::downstream::Model) -> Self {
        Downstream {
            id: row.id,
            url: row.url.clone(),
            api_key: row.api_key.clone(),
            scope: crate::selection::stored(&row.scope),
        }
    }

    /// Where a delivery is POSTed: the configured base URL with
    /// [`UPLOAD_PATH`] appended, joined so that a trailing slash on either side
    /// cannot produce `//api/call-upload`.
    pub fn upload_url(&self) -> String {
        format!("{}{UPLOAD_PATH}", self.url.trim_end_matches('/'))
    }

    /// Whether a Call on this System reaching these Talkgroups goes to this
    /// Downstream.
    ///
    /// The Talkgroups are the Call's own **plus its patches**, both canonical —
    /// the same set the live feed asks over, which is the half rdio gets wrong.
    pub fn admits(&self, system_ref: i64, talkgroups: impl Iterator<Item = i64>) -> bool {
        self.scope
            .reaches_channels(system_ref, talkgroups, |_, _| true)
    }
}

/// Which of these Downstreams a Call on `system_ref` reaching `talkgroups` is
/// owed to — **the routing decision, purely** (#96's shape).
///
/// It lives here rather than inside the write that uses it, because a write is
/// not where a policy belongs: [`crate::db::repo::queue_deliveries`] is handed
/// the ids and asks nothing about Selections, which keeps the data layer clear
/// of a domain module the way #98 left it.
pub fn routed_to(peers: &[Downstream], system_ref: i64, talkgroups: &[i64]) -> Vec<i64> {
    peers
        .iter()
        .filter(|peer| peer.admits(system_ref, talkgroups.iter().copied()))
        .map(|peer| peer.id)
        .collect()
}

/// The forwarding subsystem, cloned into every handler.
///
/// Unlike [`crate::enhance::Enhancer`] there is no disabled form: a Downstream
/// is a **row**, so an Instance with no peers is one whose roster is empty
/// rather than one whose feature is off. The Worker it spawns sleeps until
/// something is enqueued and costs nothing until then.
///
/// It is this section's configuration wrapped around a [`Dispatcher`] — the
/// accounting, the wake-up, the shared HTTP client and the double-spawn guard,
/// all of which [`crate::webhook::Webhooks`] is the same thing wrapped around
/// (#54). Those are the parts with the most ways to be subtly wrong, so they are
/// wrong or right in exactly one place.
#[derive(Clone)]
pub struct Downstreams {
    config: DownstreamConfig,
    dispatch: Arc<Dispatcher>,
}

impl Default for Downstreams {
    fn default() -> Self {
        Downstreams::new(DownstreamConfig::default())
    }
}

impl Downstreams {
    pub fn new(config: DownstreamConfig) -> Self {
        let dispatch = Dispatcher::new(config.timeout);
        Downstreams { config, dispatch }
    }

    /// The queue's accounting, its wake-up and its client.
    pub fn dispatch(&self) -> Arc<Dispatcher> {
        self.dispatch.clone()
    }

    /// Note that `count` deliveries have been queued, and wake the sender.
    ///
    /// [`Dispatcher::owes`] is where *what one unit of this Worker's work is*
    /// is written down, along with the two obvious readings that both hang the
    /// suite.
    pub fn owes(&self, count: usize) {
        self.dispatch.owes(count);
    }

    /// Wait until `n` deliveries have left the queue since this Instance
    /// started — the wait a **retry** needs, as opposed to an attempt count
    /// ([`Dispatcher::deliveries_settled`]).
    pub async fn deliveries_settled(&self, n: u64) {
        self.dispatch.deliveries_settled(n).await;
    }

    /// The policy this Instance forwards under.
    pub fn config(&self) -> &DownstreamConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn peer(scope: serde_json::Value) -> Downstream {
        Downstream::from_row(&crate::db::entities::downstream::Model {
            id: 1,
            label: None,
            url: String::from("https://peer.example"),
            api_key: String::from("s3cret"),
            scope: scope.to_string(),
            disabled: false,
            last_success_ms: None,
            last_failure_ms: None,
            last_failure: None,
            consecutive_failures: 0,
            created_at_ms: 0,
        })
    }

    /// The one that matters, and the one rdio gets wrong: a **Patch** puts a
    /// transmission on a channel that is genuinely not the one it was
    /// transmitted on, and a peer subscribed to *that* channel is entitled to
    /// it. rdio's `HasAccess` compares `call.Talkgroup.TalkgroupRef` and stops
    /// there, so this Call reaches nobody.
    #[test]
    fn a_patched_call_reaches_a_peer_scoped_to_the_patched_channel() {
        let peer = peer(serde_json::json!({ "sel": { "11": { "500": true } } }));

        assert!(
            peer.admits(11, [100, 500].into_iter()),
            "patched onto a scoped channel"
        );
        assert!(
            !peer.admits(11, [100].into_iter()),
            "the same Call unpatched"
        );
    }

    /// Scope is the live feed's algebra whole, not a subset of it — including
    /// the exception, which is what lets an Operator forward a System *except*
    /// one sensitive channel.
    #[rstest]
    #[case(serde_json::json!({ "all": true }), 11, 100, true, "everything")]
    #[case(serde_json::json!({}), 11, 100, false, "a peer nobody has scoped")]
    #[case(serde_json::json!({ "sel": { "11": { "*": true } } }), 11, 999, true, "a whole System")]
    #[case(serde_json::json!({ "sel": { "11": { "*": true } } }), 22, 999, false, "another System")]
    #[case(
        serde_json::json!({ "all": true, "sel": { "11": { "100": false } } }),
        11, 100, false, "an exception to everything"
    )]
    fn scope_is_the_live_feeds_own_rule(
        #[case] scope: serde_json::Value,
        #[case] system_ref: i64,
        #[case] talkgroup_ref: i64,
        #[case] expected: bool,
        #[case] what: &str,
    ) {
        assert_eq!(
            peer(scope).admits(system_ref, std::iter::once(talkgroup_ref)),
            expected,
            "{what}"
        );
    }

    /// A scope that will not parse selects nothing. The other direction —
    /// treating it as "everything" — would forward an Operator's whole County to
    /// a peer they had scoped to one channel, which is the failure you cannot
    /// take back.
    #[test]
    fn an_unparseable_scope_forwards_nothing() {
        assert!(!peer(serde_json::json!("not a selection")).admits(11, std::iter::once(100)));
    }

    /// An Operator pastes the address they would give a recorder, with or
    /// without the slash they happened to copy.
    #[rstest]
    #[case("https://peer.example")]
    #[case("https://peer.example/")]
    #[case("https://peer.example//")]
    fn the_upload_path_is_appended_once(#[case] url: &str) {
        let mut peer = peer(serde_json::json!({}));
        peer.url = url.to_string();

        assert_eq!(peer.upload_url(), "https://peer.example/api/call-upload");
    }

    /// The section's own two settings are the ones the shared backoff applies —
    /// the only thing about a retry this configuration still decides.
    #[test]
    fn the_sections_retry_settings_are_the_ones_applied() {
        let config = DownstreamConfig {
            retry_initial: Duration::from_secs(7),
            retry_max: Duration::from_secs(70),
            ..DownstreamConfig::default()
        };

        assert_eq!(
            config.retry(),
            Retry {
                initial: Duration::from_secs(7),
                max: Duration::from_secs(70),
            }
        );
    }
}
