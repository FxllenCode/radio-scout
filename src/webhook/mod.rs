//! **Webhooks** (#54, spec US 21): the marked Calls this Instance posts to an
//! Operator's own URL.
//!
//! # What this is, and what it deliberately is not
//!
//! Radio-Scout does not wake a device ([ADR-0014]). There is no Web Push, no
//! device notification, and **"Alert" is not a word this project uses** — what a
//! Recorder proves about a transmission is a **mark on a Call**, shown, filtered
//! and searched on. A Webhook is the one *delivery* path, and it survives the
//! removal because it is **Operator**-facing: someone configuring a URL to
//! receive their own flagged Calls is arranging their own inbox, which is a
//! different act from this Instance deciding to wake a **Listener** it has never
//! met. It sits beside [`crate::downstream`], not beside anything Listener-facing.
//!
//! It follows that nothing here may become a way back to a notification. A
//! Webhook is a row an Operator wrote; it has no subscription, no device, no
//! permission prompt and no per-Listener anything.
//!
//! # The shape
//!
//! - [`Mark`] is what a Recorder proved — **Emergency** today (#42), a **Tone
//!   profile** match once #55 lands. [`Marks`] is a set of them, and the trigger
//!   is *any* overlap between what a Call carries and what a Webhook asked for.
//! - [`Webhook`] is a configured URL as the sender uses one, scoped by a
//!   [`Selection`] — the live feed's own type, so a **Patch** reaches a webhook
//!   subscribed to the channel it was patched onto, which is the bug rdio's
//!   forwarder has.
//! - [`routed_to`] is the routing decision, purely: marks **and** scope, because
//!   an Emergency on a channel this webhook was not given is not its business.
//! - [`payload`] renders the body — ours or Discord's — and is pure.
//! - [`sender`] is the errand; the queue around it is [`crate::delivery`],
//!   shared with the Downstream sender.
//!
//! # What it costs a Call that carries no mark: nothing
//!
//! [`crate::ingest`] reads the Downstream roster on **every** upload, because
//! any Call may reach a peer. It reads the Webhook roster only when the Call
//! carries a mark, because one that carries none cannot match any webhook —
//! [`Marks::is_empty`] is checked before a statement is issued. On a Pi taking a
//! Call a second, an Emergency happens a few times a day, so this feature's
//! steady-state cost at ingest is **zero statements** rather than one.
//!
//! # Improving on rdio-scanner
//!
//! rdio-scanner has no webhooks at all — there is nothing to be compatible with
//! and nothing to clone, so the whole design is ours. What it does have is
//! `server/downstream.go`, whose defects [`crate::downstream`] lists; the two
//! that apply here are inherited as fixes rather than re-made: delivery is a
//! durable queue rather than an inline POST that drops what it cannot send, and
//! the scope walks patches rather than comparing one Talkgroup Ref.
//!
//! [ADR-0014]: https://github.com/FxllenCode/radio-scout/blob/next/docs/adr/0014-no-notifications.md

pub mod payload;
pub mod sender;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::delivery::{Dispatcher, Retry};
use crate::selection::Selection;

/// What this Worker is called on a status surface (#93, #70).
pub const WORKER: &str = "webhook";

/// What a Recorder proved about a transmission — a **mark on a Call**.
///
/// A closed vocabulary, and the whole trigger surface: a Webhook fires because a
/// Call carries one of these and for no other reason. There is deliberately no
/// "every Call" member — a webhook is an inbox, and a county's routine traffic
/// posted into a chat room would exceed Discord's rate limit within a minute of
/// being switched on. An Operator who wants every Call has [`crate::downstream`],
/// which is built for exactly that.
///
/// **#55 adds one variant here, and the compiler names the rest.** The roster
/// read, the routing, the queue, the scoping and the admin form are all written
/// against the *set* rather than against Emergency, so none of them changes. The
/// three places that match a `Mark` exhaustively — [`Mark::slug`], the payload's
/// own sentence, and `markName` on the client — are each a compile error until
/// the new mark is given a word, which is the point: a mark nobody has named
/// would otherwise render as its slug.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mark {
    /// The radio set the emergency bit (#42, spec US 5).
    Emergency,
}

/// Every mark there is, so a table cannot go stale by omission.
pub const MARKS: [Mark; 1] = [Mark::Emergency];

impl Mark {
    /// The wire spelling — what the row stores, what the form sends, and what
    /// the payload names.
    pub fn slug(self) -> &'static str {
        match self {
            Mark::Emergency => "emergency",
        }
    }

    /// Read a stored or submitted spelling. `None` is one this release does not
    /// know.
    pub fn from_slug(slug: &str) -> Option<Mark> {
        MARKS.into_iter().find(|mark| mark.slug() == slug)
    }
}

/// A set of [`Mark`]s — what a Call carries, and what a Webhook asked for.
///
/// Ordered rather than a `HashSet` so the admin listing renders the same way
/// twice: a set that reshuffles between refreshes is a screen an Operator stops
/// trusting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marks(BTreeSet<Mark>);

impl FromIterator<Mark> for Marks {
    fn from_iter<I: IntoIterator<Item = Mark>>(marks: I) -> Self {
        Marks(marks.into_iter().collect())
    }
}

impl Marks {
    /// What a Call carries, from the facts a Recorder sent.
    ///
    /// One function rather than a bool threaded through routing, because #55's
    /// tone match is decided somewhere else entirely and has to arrive here
    /// looking like this one does.
    pub fn on_call(emergency: bool) -> Marks {
        let mut marks = BTreeSet::new();
        if emergency {
            marks.insert(Mark::Emergency);
        }
        Marks(marks)
    }

    /// Whether this Call is of interest to *anything* — the gate that keeps the
    /// webhook roster read off the ordinary ingest path.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Which of a Call's marks a webhook wanting `wanted` asked for.
    ///
    /// **What a webhook is told about**, and deliberately not the Call's whole
    /// set: one watching only for tone-outs should never have to work out why it
    /// was sent an Emergency, and once #55 lands that stops being a distinction
    /// without a difference.
    pub fn wanted_by(&self, wanted: &Marks) -> Marks {
        Marks(self.0.intersection(&wanted.0).copied().collect())
    }

    /// Whether a Call carrying `self` is one a webhook wanting `wanted` asked
    /// for at all. Any overlap: a webhook asking for two marks wants a Call
    /// carrying either.
    ///
    /// [`Marks::wanted_by`] said as a question, rather than a second walk of the
    /// same two sets — so "does it fire" and "what is it told" can never
    /// disagree.
    pub fn overlaps(&self, wanted: &Marks) -> bool {
        !self.wanted_by(wanted).is_empty()
    }

    /// The marks, in order.
    pub fn iter(&self) -> impl Iterator<Item = Mark> + '_ {
        self.0.iter().copied()
    }

    /// The slugs, for a payload or a listing.
    pub fn slugs(&self) -> Vec<&'static str> {
        self.iter().map(Mark::slug).collect()
    }
}

impl Serialize for Marks {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.slugs().serialize(serializer)
    }
}

/// **What a stored mark set means, written once.**
///
/// A set that will not parse selects **nothing**, the [`crate::downstream::scope_of`]
/// direction and for its reason: the alternative is a webhook whose stored
/// configuration is unreadable posting an Operator's whole Archive into a
/// channel.
///
/// A slug this release does not know is **dropped rather than fatal**, which is
/// the downgrade case: a row written by a version that has #55's `tone` mark,
/// read by one that does not, keeps firing on `emergency` instead of falling
/// silent altogether.
pub fn marks_of(stored: &str) -> Marks {
    serde_json::from_str::<Vec<String>>(stored)
        .map(|slugs| {
            slugs
                .iter()
                .filter_map(|slug| Mark::from_slug(slug))
                .collect()
        })
        .unwrap_or_default()
}

/// A mark set as the column stores it.
///
/// A list of static strings cannot fail to serialize; the fallback is an empty
/// document rather than an `unwrap`, and an empty document fires for nothing.
pub fn marks_json(marks: &Marks) -> String {
    serde_json::to_string(&marks.slugs()).unwrap_or_else(|_| String::from("[]"))
}

/// Which shape a webhook's body takes.
///
/// Two, and the second one is the whole reason this is not just "POST some
/// JSON": Discord will not render an arbitrary document, and an Operator whose
/// automation *is* a Discord channel is the common case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    /// Radio-Scout's own: the Call as the live feed and the search page describe
    /// it, plus the marks that fired. What an automation parses.
    #[default]
    RadioScout,
    /// A Discord webhook message — one embed, rendered by Discord itself.
    Discord,
}

/// Every format there is, so a table cannot go stale by omission.
pub const FORMATS: [Format; 2] = [Format::RadioScout, Format::Discord];

impl Format {
    /// The wire spelling.
    pub fn slug(self) -> &'static str {
        match self {
            Format::RadioScout => "radio-scout",
            Format::Discord => "discord",
        }
    }

    /// Read a stored or submitted spelling. `None` is one this release does not
    /// know.
    pub fn from_slug(slug: &str) -> Option<Format> {
        FORMATS.into_iter().find(|format| format.slug() == slug)
    }
}

/// **What a stored format means, written once.**
///
/// An unreadable one falls back to [`Format::RadioScout`] rather than to
/// nothing, because unlike a scope or a mark set there is no "safe" empty
/// format — a webhook has to be sent *somehow*, and our own shape is the one
/// that carries every fact rather than the one a third party has to accept.
pub fn format_of(stored: &str) -> Format {
    Format::from_slug(stored).unwrap_or_default()
}

/// The two schemes anything this Instance POSTs to — or builds a link on — may
/// use.
///
/// `http` as well as `https` because an Operator's automation is often a script
/// on their own LAN, and a scanner behind a VPN is the common deployment.
const POSTABLE_SCHEMES: [&str; 2] = ["http://", "https://"];

/// Whether `url` is one this Instance could actually POST to, or build a link
/// on — **written once**, because two callers ask it about two different
/// strings and a disagreement between them is invisible until a Call is lost.
///
/// The callers are [`crate::curate::webhooks`], validating the URL an Operator
/// pasted, and [`crate::config`], validating `[server] public_url`. The second
/// one is the reason this is strict about the *scheme* rather than merely about
/// having a host: `public_url` ends up as a Discord embed's `url`, Discord
/// answers `400` for one it cannot parse, and a `400` is
/// [`crate::delivery::Verdict::Abandon`] — so a `ftp://` typo in a TOML would
/// silently drop every Emergency it was configured to carry.
///
/// Whitespace is refused for the same reason: `https://scan example` has a
/// perfectly good "host" by the rule below and is not a URL anybody can fetch.
pub fn is_postable_url(url: &str) -> bool {
    POSTABLE_SCHEMES
        .iter()
        .any(|scheme| url.starts_with(scheme))
        && !url.contains(char::is_whitespace)
        && host_of(url).is_some()
}

/// The **host** of a URL, which is all of one that may ever be shown.
///
/// A Webhook's URL is its credential, so the listing cannot carry it — and a
/// screen of rows distinguished only by an optional label is a screen an
/// Operator cannot use. `discord.com` is not a secret and is exactly the
/// reminder they need.
///
/// Parsed by hand rather than through a URL crate: the answer wanted here is
/// "the bit between the scheme and the path, without any userinfo", and the one
/// thing that absolutely must not happen is a parser being lenient enough to
/// leak a path segment. `None` for anything that does not look like an absolute
/// URL at all.
pub fn host_of(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|authority| !authority.is_empty())?;
    // Discard any `user:password@` — it is a credential, and it is exactly the
    // thing a naive "everything before the first slash" would publish.
    let host = authority.rsplit('@').next()?;
    match host.is_empty() {
        true => None,
        false => Some(host.to_owned()),
    }
}

/// How webhook delivery behaves — and the `[webhook]` section itself (ADR-0012,
/// #87).
///
/// Policy only. The webhooks themselves are **Curation** (CONTEXT.md): rows an
/// Operator edits in the browser, never anything written in `radio-scout.toml` —
/// which for this feature is also a security property, since the URL is a
/// credential and a TOML is world-readable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebhookConfig {
    /// How long a webhook has to answer one delivery before it counts as
    /// failed.
    #[serde(rename = "timeout_secs", with = "crate::config::secs")]
    pub timeout: Duration,
    /// How long to wait before the first retry. Doubles per attempt up to
    /// [`WebhookConfig::retry_max`].
    #[serde(rename = "retry_initial_secs", with = "crate::config::secs")]
    pub retry_initial: Duration,
    /// The ceiling on that doubling.
    #[serde(rename = "retry_max_secs", with = "crate::config::secs")]
    pub retry_max: Duration,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        WebhookConfig {
            // **Deliberately not the Downstream's 30 seconds.** That number is
            // sized for a peer receiving a minute of audio over a domestic
            // uplink; this body is a few hundred bytes of JSON, and a sink that
            // has not answered in ten seconds is not slow, it is down. The
            // difference matters because the timeout is also how long an
            // Emergency waits before its first retry begins.
            timeout: Duration::from_secs(10),
            retry_initial: Duration::from_secs(5),
            // The ceiling is also the recovery latency (the Downstream note):
            // a sink down overnight is polled once a minute rather than
            // thousands of times, and its backlog starts moving within a minute
            // of it returning.
            retry_max: Duration::from_secs(60),
        }
    }
}

impl WebhookConfig {
    /// How hard a webhook is tried again — this section's two retry settings as
    /// the shared queue reads them.
    pub fn retry(&self) -> Retry {
        Retry {
            initial: self.retry_initial,
            max: self.retry_max,
        }
    }
}

/// A configured Webhook, as the sender uses one.
///
/// The scope, the marks and the format arrive **parsed**, because what an
/// unreadable one means is decided once ([`marks_of`], [`format_of`],
/// [`crate::downstream::scope_of`]) where it can be said out loud — and because
/// the curation listing has to show the same policy the sender is applying
/// rather than a shape it invented.
#[derive(Debug, Clone, PartialEq)]
pub struct Webhook {
    pub id: i64,
    /// Where the POST goes. **The credential** — never rendered, never logged.
    pub url: String,
    pub format: Format,
    pub marks: Marks,
    pub scope: Selection,
}

impl crate::delivery::Sink for Webhook {
    fn id(&self) -> i64 {
        self.id
    }
}

impl Webhook {
    /// Read a stored row.
    pub fn from_row(row: &crate::db::entities::webhook::Model) -> Self {
        Webhook {
            id: row.id,
            url: row.url.clone(),
            format: format_of(&row.format),
            marks: marks_of(&row.marks),
            scope: crate::downstream::scope_of(&row.scope),
        }
    }

    /// Whether a Call carrying `marks`, on this System, reaching these
    /// Talkgroups, is this Webhook's business.
    ///
    /// **Both halves, and the order is not an optimisation.** The marks are
    /// checked first because that is the cheap set intersection and the one that
    /// is usually false; the scope is checked because an Emergency on a channel
    /// this webhook was never given is somebody else's Emergency.
    pub fn admits(
        &self,
        marks: &Marks,
        system_ref: i64,
        talkgroups: impl Iterator<Item = i64>,
    ) -> bool {
        marks.overlaps(&self.marks)
            && self
                .scope
                .reaches_channels(system_ref, talkgroups, |_, _| true)
    }
}

/// Which of these Webhooks a Call carrying `marks` on `system_ref` reaching
/// `talkgroups` is owed to — **the routing decision, purely** (#96's shape).
///
/// It lives here rather than inside the write that uses it, because a write is
/// not where a policy belongs: [`crate::db::repo::queue_webhook_deliveries`] is
/// handed the ids and asks nothing about marks or Selections, which keeps a
/// domain module out of the data layer the way #98 left it.
pub fn routed_to(
    hooks: &[Webhook],
    marks: &Marks,
    system_ref: i64,
    talkgroups: &[i64],
) -> Vec<i64> {
    hooks
        .iter()
        .filter(|hook| hook.admits(marks, system_ref, talkgroups.iter().copied()))
        .map(|hook| hook.id)
        .collect()
}

/// The webhook subsystem, cloned into every handler.
///
/// The [`crate::downstream::Downstreams`] shape exactly: this section's
/// configuration wrapped around a [`Dispatcher`]. Like a Downstream and unlike
/// enhancement there is no disabled form — a Webhook is a **row**, so an
/// Instance with none has an empty roster rather than a feature switched off,
/// and one added from the browser five minutes from now must be delivered to
/// without a restart.
#[derive(Clone)]
pub struct Webhooks {
    config: WebhookConfig,
    dispatch: Arc<Dispatcher>,
    /// Where a payload's absolute URLs point — `[server] public_url`, resolved
    /// once at boot.
    ///
    /// It lives here rather than being read out of `[server]` at send time
    /// because this is the only subsystem that needs it, and because it is a
    /// *policy about the payload*: [`payload`] is pure and is handed the base
    /// rather than reaching for it.
    public_url: Option<Arc<str>>,
}

impl Default for Webhooks {
    fn default() -> Self {
        Webhooks::new(WebhookConfig::default(), None)
    }
}

impl Webhooks {
    pub fn new(config: WebhookConfig, public_url: Option<String>) -> Self {
        let dispatch = Dispatcher::new(config.timeout);
        Webhooks {
            config,
            dispatch,
            public_url: public_url.map(Arc::from),
        }
    }

    /// The queue's accounting, its wake-up and its client.
    pub fn dispatch(&self) -> Arc<Dispatcher> {
        self.dispatch.clone()
    }

    /// Note that `count` deliveries have been queued, and wake the sender
    /// ([`Dispatcher::owes`]).
    pub fn owes(&self, count: usize) {
        self.dispatch.owes(count);
    }

    /// Wait until `n` deliveries have left the queue since this Instance
    /// started — the wait a **retry** needs.
    pub async fn deliveries_settled(&self, n: u64) {
        self.dispatch.deliveries_settled(n).await;
    }

    /// The policy this Instance posts under.
    pub fn config(&self) -> &WebhookConfig {
        &self.config
    }

    /// Where this Instance can be reached from outside, if an Operator has said.
    pub fn public_url(&self) -> Option<&str> {
        self.public_url.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn hook(marks: serde_json::Value, scope: serde_json::Value) -> Webhook {
        Webhook::from_row(&crate::db::entities::webhook::Model {
            id: 1,
            label: None,
            url: String::from("https://discord.com/api/webhooks/1/s3cret"),
            format: String::from("discord"),
            marks: marks.to_string(),
            scope: scope.to_string(),
            disabled: false,
            last_success_ms: None,
            last_failure_ms: None,
            last_failure: None,
            consecutive_failures: 0,
            created_at_ms: 0,
        })
    }

    fn emergency_hook() -> Webhook {
        hook(
            serde_json::json!(["emergency"]),
            serde_json::json!({ "all": true }),
        )
    }

    /// **Both halves have to hold.** An ordinary Call on a scoped channel is not
    /// this webhook's business, and neither is an Emergency on a channel it was
    /// never given — which is the difference between an inbox and a firehose.
    #[rstest]
    #[case(true, 11, true, "an Emergency in scope")]
    #[case(false, 11, false, "an ordinary Call in scope")]
    #[case(true, 22, false, "an Emergency outside the scope")]
    #[case(false, 22, false, "neither")]
    fn a_call_reaches_a_webhook_only_on_a_mark_it_asked_for_and_a_channel_it_was_given(
        #[case] emergency: bool,
        #[case] system_ref: i64,
        #[case] expected: bool,
        #[case] what: &str,
    ) {
        let hook = hook(
            serde_json::json!(["emergency"]),
            serde_json::json!({ "sel": { "11": { "*": true } } }),
        );

        assert_eq!(
            hook.admits(&Marks::on_call(emergency), system_ref, std::iter::once(100)),
            expected,
            "{what}"
        );
    }

    /// The one rdio's forwarder gets wrong, inherited as a fix: a **Patch** puts
    /// a transmission on a channel that is genuinely not the one it was
    /// transmitted on, and a webhook scoped to *that* channel asked for it.
    #[test]
    fn a_patched_emergency_reaches_a_webhook_scoped_to_the_patched_channel() {
        let hook = hook(
            serde_json::json!(["emergency"]),
            serde_json::json!({ "sel": { "11": { "500": true } } }),
        );
        let marks = Marks::on_call(true);

        assert!(hook.admits(&marks, 11, [100, 500].into_iter()));
        assert!(!hook.admits(&marks, 11, [100].into_iter()));
    }

    /// A stored configuration that will not parse fires for **nothing**. The
    /// other direction would post an Operator's whole Archive into a channel,
    /// which is the failure you cannot take back — a message you did not mean to
    /// send is already sent.
    #[rstest]
    #[case(serde_json::json!("not a list"), serde_json::json!({ "all": true }), "unreadable marks")]
    #[case(serde_json::json!(["emergency"]), serde_json::json!("not a selection"), "unreadable scope")]
    #[case(serde_json::json!([]), serde_json::json!({ "all": true }), "no marks asked for")]
    fn an_unreadable_or_empty_configuration_fires_for_nothing(
        #[case] marks: serde_json::Value,
        #[case] scope: serde_json::Value,
        #[case] what: &str,
    ) {
        assert!(
            !hook(marks, scope).admits(&Marks::on_call(true), 11, std::iter::once(100)),
            "{what}"
        );
    }

    /// **A mark this release does not know is dropped, not fatal.** A row
    /// written by a version that has #55's tone mark and read by one that does
    /// not must keep firing on Emergency, rather than parsing to nothing and
    /// falling silent on the mark it *does* understand.
    #[test]
    fn an_unknown_mark_does_not_take_the_known_ones_with_it() {
        let marks = marks_of(r#"["emergency","tone"]"#);

        assert_eq!(marks.slugs(), vec!["emergency"]);
        assert!(Marks::on_call(true).overlaps(&marks));
    }

    /// The set round-trips through the column, so what an Operator ticked is
    /// what routing asks — and renders in a stable order, because a listing that
    /// reshuffles between refreshes is one nobody trusts.
    #[test]
    fn a_mark_set_round_trips_through_the_column() {
        let marks: Marks = MARKS.into_iter().collect();

        assert_eq!(marks_of(&marks_json(&marks)), marks);
        assert_eq!(
            marks.slugs(),
            MARKS.map(Mark::slug).to_vec(),
            "ordered by the vocabulary, not by insertion"
        );
    }

    /// Every mark and every format reads back as itself, and nothing else does.
    #[test]
    fn every_slug_is_its_own_and_round_trips() {
        for mark in MARKS {
            assert_eq!(Mark::from_slug(mark.slug()), Some(mark));
        }
        for format in FORMATS {
            assert_eq!(Format::from_slug(format.slug()), Some(format));
        }
        assert_eq!(Mark::from_slug("tone"), None, "not until #55");
        assert_eq!(Format::from_slug("slack"), None);
    }

    /// An unreadable format still sends: unlike a scope, there is no safe
    /// *empty* format, and our own shape carries every fact rather than the one
    /// a third party has to accept.
    #[rstest]
    #[case("discord", Format::Discord)]
    #[case("radio-scout", Format::RadioScout)]
    #[case("slack", Format::RadioScout)]
    #[case("", Format::RadioScout)]
    fn an_unreadable_stored_format_falls_back_to_our_own(
        #[case] stored: &str,
        #[case] expected: Format,
    ) {
        assert_eq!(format_of(stored), expected);
    }

    /// **The host, and never a path.** The listing shows this instead of the URL
    /// because the URL is the credential — so the one thing this must never do
    /// is be lenient enough to let a token through.
    #[rstest]
    #[case("https://discord.com/api/webhooks/123/t0ken", Some("discord.com"))]
    #[case("https://hooks.example:8443/x", Some("hooks.example:8443"))]
    #[case("http://example.test", Some("example.test"))]
    #[case("https://example.test?token=x", Some("example.test"))]
    #[case("https://example.test#frag", Some("example.test"))]
    #[case("https://user:pass@example.test/x", Some("example.test"))]
    #[case("https://user@/x", None)]
    #[case("https:///nohost", None)]
    #[case("not a url", None)]
    #[case("", None)]
    fn a_host_is_all_of_a_url_that_may_ever_be_shown(
        #[case] url: &str,
        #[case] expected: Option<&str>,
    ) {
        assert_eq!(host_of(url).as_deref(), expected, "{url}");
    }

    /// A Call carrying nothing is the ordinary case, and it is the one that must
    /// cost no statement at ingest.
    #[test]
    fn an_unmarked_call_carries_no_marks_at_all() {
        assert!(Marks::on_call(false).is_empty());
        assert!(!Marks::on_call(true).is_empty());
    }

    /// Routing answers with ids, and only the webhooks that asked.
    #[test]
    fn routing_names_the_webhooks_that_asked_for_this_call() {
        let wants_emergency = Webhook {
            id: 1,
            ..emergency_hook()
        };
        let wants_nothing = Webhook {
            id: 2,
            marks: Marks::default(),
            ..emergency_hook()
        };
        let elsewhere = Webhook {
            id: 3,
            scope: crate::downstream::scope_of(r#"{"sel":{"22":{"*":true}}}"#),
            ..emergency_hook()
        };
        let hooks = vec![wants_emergency, wants_nothing, elsewhere];

        assert_eq!(
            routed_to(&hooks, &Marks::on_call(true), 11, &[100]),
            vec![1]
        );
        assert!(routed_to(&hooks, &Marks::on_call(false), 11, &[100]).is_empty());
    }

    /// The section's own two settings are the ones the shared backoff applies.
    #[test]
    fn the_sections_retry_settings_are_the_ones_applied() {
        let config = WebhookConfig {
            retry_initial: Duration::from_secs(3),
            retry_max: Duration::from_secs(30),
            ..WebhookConfig::default()
        };

        assert_eq!(
            config.retry(),
            Retry {
                initial: Duration::from_secs(3),
                max: Duration::from_secs(30),
            }
        );
    }

    /// A webhook's timeout is **not** a Downstream's: this body is a few hundred
    /// bytes where that one is a minute of audio, and the timeout is also how
    /// long an Emergency waits before its first retry starts.
    #[test]
    fn a_webhook_is_given_less_time_to_answer_than_a_peer() {
        assert!(
            WebhookConfig::default().timeout
                < crate::downstream::DownstreamConfig::default().timeout
        );
    }
}
