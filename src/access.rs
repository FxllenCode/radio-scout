//! **Access codes** (#68, spec US 52): gating the sensitive, keeping the open.
//!
//! CONTEXT.md: an **Access code** is a Listener-facing secret that grants scoped
//! access to specific **Systems** and **Talkgroups**. ADR-0008 deferred it to v2
//! and left [`AccessScope`] behind as the seam; this is what resolves one.
//!
//! # Two independent questions, and the separation is the feature
//!
//! *What is sensitive* is a property of the channel — `systems.restricted`, and
//! `talkgroups.restricted` inheriting it. *Who may hear it* is a property of a
//! code. Deriving either from the other — a channel being gated because some
//! code happens to name it, which is the obvious short cut — makes the state an
//! Operator is in halfway through setting this up **inexpressible**: sensitive,
//! and no code written yet, which must be silent rather than open. It would also
//! mean a code scoped to every System gating the whole Instance by accident,
//! which is rdio-scanner's all-or-nothing PIN wall arrived at sideways.
//!
//! So an Instance with nothing restricted hands every Listener [`AccessScope::All`]
//! and costs **nothing at all**: no statement, no join, no predicate. That is
//! what makes "an instance with no codes behaves exactly as today" true by
//! construction rather than by inspection, and it is why [`Access::is_gating`]
//! is a cached bit on [`crate::tone::Tones::is_armed`]'s terms — refreshed at
//! boot and on the same request that writes a `restricted` column, because the
//! two ways of being stale cost very different things here: stale-*true* buys a
//! join, stale-*false* is a leak.
//!
//! # The scope is a Selection, and that is not a coincidence
//!
//! A code's scope is [`crate::selection::Selection`] — the same matrix the live
//! feed subscribes with, a **Downstream** peer is scoped by and a **DVR** filters
//! on. Three things come free and one of them is load-bearing:
//! [`Selection::selects`] is the per-channel rule, [`crate::archive`]'s `within`
//! is already the SQL for it (exhaustively tested against the predicate since
//! #63), and an unreadable stored scope already has one meaning, written once
//! ([`crate::selection::stored`]): it reaches **nothing**, which here is the safe
//! direction.
//!
//! # What a Call's gate is, and what it deliberately is not
//!
//! A **Patch** puts one transmission on several channels, and a Call's own
//! channel is the only one whose `restricted` column is in hand when a live frame
//! is decided. So the gate is the Call's **own** channel's, always — a Call
//! addressed to a restricted channel stays gated however it was patched, and a
//! Call addressed to an open one stays open. The second half is not a hole: that
//! audio is reachable on the open channel by anybody, so refusing it on the
//! patched name would hide nothing and cost a Listener a Call they can already
//! hear. The first half is the conservative reading and is the one that matters.
//!
//! # Improving on rdio-scanner
//!
//! Every row below is something `access.go` / `controller.go` does, and the
//! right-hand column is what this does instead.
//!
//! | rdio-scanner | Radio-Scout |
//! |---|---|
//! | the code is stored **plaintext**, returned by the admin API, and exported in the configuration document | Argon2id at rest; the row's minted **grant** is what travels, never returned except to the browser that just unlocked, and access codes are **absent from the document entirely** |
//! | a failed attempt **logs the code it was given** (`controller.go:462`) | nothing ever logs a code or a grant, at any level (ADR-0011 rule 2) |
//! | the guess counter is per-socket and resets on reconnect, so there is no brute-force bound at all | a shared per-address [`crate::lockout::Lockout`], the admin login's own |
//! | `client.Access = access` is assigned **before** `HasExpired()` and before the connection limit, so a client told `expired` or `max` still passes every later scope check — **an expired code grants full scoped access** | an [`Unlocked`] is the only thing that carries a scope, and it is produced by the one path that has already checked both |
//! | the limit is compared by **pointer identity** and the roster is rebuilt on every config write, so editing anything resets every code's count to zero | counted by code **Id** |
//! | nothing ever re-checks an open connection, so a code that expires tonight keeps working until the listener closes the tab | expiry is re-read on the live feed's own heartbeat tick (#94's table) |
//! | all-or-nothing: any code closes the whole instance | the gate is per channel, and everything else stays open |

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::extract::{FromRequestParts, Query};
use axum::http::request::Parts;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::db::Db;
use crate::db::entities::access_code;
use crate::failure::{Failure, Reason, Stage};
use crate::lockout::{Budget, Lockout};
use crate::selection::Selection;

/// The query parameter every gated read carries the **grant** in.
///
/// A query parameter rather than a header or a path segment, which ADR-0008
/// settled before this ticket existed and for a reason worth repeating:
/// `http_log` writes a request's *path* and deliberately never its query, so "a
/// grant is never logged" is true by construction rather than by a redaction
/// pass that can be wrong. It is also the only thing an `<audio src>` and a
/// `WebSocket` URL can both carry.
pub const GRANT_PARAM: &str = "grant";

/// How many bytes of randomness a minted grant carries. 128 bits, the
/// **Share link** token's own size and for its reason: it is a bearer credential
/// that reaches a URL, so it has to be unguessable rather than memorable.
const GRANT_BYTES: usize = 16;

/// The prefix a grant is minted with, so one found in a paste is recognisable as
/// this Instance's and not as an API key or a share token.
const GRANT_PREFIX: &str = "rsg_";

// ---------------------------------------------------------------------------
// What a viewer may hear
// ---------------------------------------------------------------------------

/// What one viewer of this Instance may hear (ADR-0008).
///
/// An **input** everywhere it is asked — [`crate::live::Connection::new`] takes
/// one, [`crate::archive::CallSearch`] carries one, [`crate::serve`] is handed
/// one — so a restricted read is something a test constructs rather than
/// something it has to arrange a credential for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessScope {
    /// Everything this Instance holds.
    ///
    /// Two situations, and they mean the same thing: an Instance with no
    /// restricted channel at all, and a code scoped to every System. Collapsing
    /// them is what keeps the cost of this feature at zero for the Instances not
    /// using it — there is no predicate to add and no join to make.
    All,
    /// Every **unrestricted** channel, plus the restricted ones this Selection
    /// reaches. [`Selection::default`] — which is what a Listener holding no
    /// code gets — reaches none of them, and is therefore exactly open listening.
    Granted(Selection),
}

impl Default for AccessScope {
    /// Open listening: every unrestricted channel and no gated one.
    ///
    /// Deliberately **not** [`AccessScope::All`], so a value assembled by
    /// mistake grants the least rather than the most.
    fn default() -> Self {
        AccessScope::Granted(Selection::default())
    }
}

impl AccessScope {
    /// What a Listener holding no **Access code** may hear.
    pub fn open() -> Self {
        AccessScope::default()
    }

    /// What a stored scope grants, normalized.
    ///
    /// A Selection that reaches every Call there is *is* [`AccessScope::All`],
    /// and saying so here rather than at each call site is what lets every
    /// consumer treat "no gate to apply" as one case — including the SQL, where
    /// it is the difference between an unfiltered search and two joins.
    pub fn granting(scope: Selection) -> Self {
        match scope.reaches_everything() {
            true => AccessScope::All,
            false => AccessScope::Granted(scope),
        }
    }

    /// May this scope hear `(system_ref, talkgroup_ref)`, given whether that
    /// channel is **restricted**?
    ///
    /// The restriction is a parameter rather than something this looks up,
    /// because the two callers already have it in hand for nothing: a live frame
    /// carries it on the [`crate::call::StoredCall`], and a stored read joins the
    /// tables it comes from anyway.
    pub fn permits(&self, system_ref: i64, talkgroup_ref: i64, restricted: bool) -> bool {
        match self {
            AccessScope::All => true,
            AccessScope::Granted(granted) => {
                !restricted || granted.selects(system_ref, talkgroup_ref)
            }
        }
    }

    /// Whether this scope reaches every Call there is, and so has no filtering
    /// left to do.
    pub fn reaches_everything(&self) -> bool {
        matches!(self, AccessScope::All)
    }
}

// ---------------------------------------------------------------------------
// The credential
// ---------------------------------------------------------------------------

/// What is known about the **Access code** a request presented, if it presented
/// one.
///
/// Three arms rather than an `Option`, because "you sent nothing" and "you sent
/// something that has stopped working" are different things to a Listener: the
/// first is how everybody starts, the second is a browser holding a grant an
/// Operator has revoked or a code that expired overnight, and only the second is
/// worth saying out loud.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Held {
    /// No grant in the query string.
    Nothing,
    /// A live code.
    Code(CodeHeld),
    /// A grant that named no code, or named one that is disabled or expired.
    ///
    /// **Degraded rather than refused**, on every HTTP read: a browser whose
    /// stored grant has gone stale must fall back to open listening and be told
    /// so, not find the whole app answering `401`. Where it *is* refused is the
    /// two places a Listener is asking for something specific — the unlock
    /// itself, and the live feed, which says so in a frame.
    Stale(Stale),
}

/// Why a presented grant is not a live code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Stale {
    /// Nothing answers to it: never minted, re-minted since, or revoked.
    ///
    /// A **disabled** code lands here too, deliberately — the durable off is
    /// meant to be indistinguishable from a revoke to whoever is holding the
    /// grant, and distinguishable in the admin listing, which is where the
    /// Operator who switched it off is looking.
    Unknown,
    /// The code existed and its window has closed.
    Expired,
}

/// The live **Access code** a request is holding.
///
/// Carries no secret: the grant that arrived is not kept, because nothing
/// downstream has any use for it and a field for it is how one ends up in a log
/// line (the [`crate::curate::webhooks::WebhookRow`] shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeHeld {
    pub id: i64,
    /// What the Operator calls it — the only thing about a code that may be
    /// said out loud, and therefore what every line about one names.
    pub label: Option<String>,
    /// When it stops working, if it does. Carried because a live-feed connection
    /// re-reads it on its own heartbeat (#94's table, one row longer): a code
    /// that runs out at midnight must take its sockets with it rather than serve
    /// until somebody closes a tab, which is what rdio does.
    pub expires_at_ms: Option<i64>,
    /// How many live-feed connections may hold it at once; `None` is unlimited.
    pub max_connections: Option<i64>,
}

/// Who is asking, and what they may hear.
///
/// **The extractor every gated surface takes.** One type rather than each
/// handler reading the query string and resolving it, because a handler that
/// forgot would answer with the whole Archive and look perfectly ordinary doing
/// it — the failure this whole module is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    pub scope: AccessScope,
    pub held: Held,
}

impl Viewer {
    /// A viewer of an Instance that gates nothing.
    pub fn unrestricted() -> Self {
        Viewer {
            scope: AccessScope::All,
            held: Held::Nothing,
        }
    }

    /// The code this viewer is holding, if it is holding a live one.
    pub fn code(&self) -> Option<&CodeHeld> {
        match &self.held {
            Held::Code(code) => Some(code),
            _ => None,
        }
    }
}

impl FromRequestParts<AppState> for Viewer {
    type Rejection = Failure;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        state
            .access
            .viewer(&state.db, presented(parts).as_deref(), state.clock.now_ms())
            .await
    }
}

/// The grant a request is carrying, read off its query string.
///
/// Deliberately tolerant: a request with no query, an unparseable one, or one
/// naming no grant are all simply *no grant*. A refusal here would mean an
/// Instance that gates nothing could reject a request over a malformed query
/// string it was never going to read.
fn presented(parts: &Parts) -> Option<String> {
    #[derive(Deserialize)]
    struct Presented {
        grant: Option<String>,
    }
    Query::<Presented>::try_from_uri(&parts.uri)
        .ok()
        .and_then(|Query(presented)| presented.grant)
        .map(|grant| grant.trim().to_string())
        .filter(|grant| !grant.is_empty())
}

// ---------------------------------------------------------------------------
// The policy
// ---------------------------------------------------------------------------

/// What an unlock attempt is allowed to cost (ADR-0012).
///
/// **Policy only, and no master switch** — the [`crate::tone::ToneConfig`] shape:
/// an Access code is a **row** and a gate is a **column**, so an Instance with
/// neither has nothing switched off, it simply has nothing gated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AccessConfig {
    /// Failed unlock attempts one address may spend before it is refused
    /// outright.
    ///
    /// More generous than `[admin] lockout_attempts`, because the two guard
    /// different people: an Operator typing a password they chose, and a
    /// Listener typing a code somebody read out to them on a phone in a fire
    /// hall.
    pub lockout_attempts: u32,
    /// How long a spent address waits, measured from its **last** attempt.
    #[serde(rename = "lockout_secs", with = "crate::config::secs")]
    pub lockout: Duration,
}

impl Default for AccessConfig {
    fn default() -> Self {
        AccessConfig {
            lockout_attempts: 10,
            lockout: Duration::from_secs(5 * 60),
        }
    }
}

/// The access surface, cloned into every handler.
///
/// Holds no roster. Resolving a grant is **one indexed lookup**, not a cached
/// map, and that is deliberate: a cache would need invalidating from five write
/// paths, and the two facts most worth being immediate — an Operator revoking a
/// code, and a code reaching its expiry — are exactly the ones a cache gets
/// wrong. What *is* cached is the single bit that decides whether a lookup
/// happens at all.
#[derive(Clone)]
pub struct Access(Arc<Inner>);

struct Inner {
    config: AccessConfig,
    /// Whether any channel on this Instance is **restricted**.
    ///
    /// [`crate::tone::Tones::is_armed`]'s bit, with one difference that decides
    /// how it is maintained: being stale-*true* costs a join on a search, and
    /// being stale-*false* means a gated channel is not gated. So it is
    /// refreshed on the same request that could have changed it, never on a
    /// timer, and `tests/access.rs` asserts end to end that restricting a
    /// channel gates the very next read.
    gating: AtomicBool,
    /// Failed unlock attempts, per address — the admin login's own ledger type,
    /// because both guard an Argon2id verification and an unbounded guesser is
    /// also a way to exhaust a Pi.
    lockout: Mutex<Lockout>,
    /// How many live-feed connections each code is holding right now.
    ///
    /// By code **Id**. rdio compares `Access` *pointers* and rebuilds its roster
    /// on every configuration write, so editing an unrelated setting silently
    /// resets every code's count to zero.
    connections: Mutex<HashMap<i64, u32>>,
}

impl Default for Access {
    fn default() -> Self {
        Access::new(AccessConfig::default())
    }
}

impl Access {
    pub fn new(config: AccessConfig) -> Self {
        Access(Arc::new(Inner {
            config,
            gating: AtomicBool::new(false),
            lockout: Mutex::new(Lockout::default()),
            connections: Mutex::new(HashMap::new()),
        }))
    }

    /// Whether any channel on this Instance is restricted at all.
    ///
    /// The whole of what this feature costs an Instance not using it: `false`
    /// here and every read below returns [`AccessScope::All`] without touching
    /// the database.
    pub fn is_gating(&self) -> bool {
        self.0.gating.load(Ordering::Relaxed)
    }

    /// Re-read whether anything is gated.
    ///
    /// Called at boot and by every surface that writes a `restricted` column —
    /// which is what makes a channel restricted now apply to the very next
    /// request rather than to the next restart.
    pub async fn rearm(&self, db: &Db) {
        self.arm(crate::db::repo::anything_restricted(db).await);
    }

    /// Take the answer, or keep the last one and say so.
    ///
    /// [`crate::tone::Tones::arm`]'s shape with the opposite bias: a roster we
    /// could not re-read leaves the bit **where it was**, which for tone-out
    /// means missing a page and here would mean either a pointless join or an
    /// open gate. Keeping the last answer is right for both — what must never
    /// happen is defaulting to `false` on an error, which would turn a database
    /// hiccup into an unlocked Instance.
    pub fn arm(&self, answer: Result<bool, sea_orm::DbErr>) {
        match answer {
            Ok(gating) => self.set_gating(gating),
            Err(error) => tracing::warn!(
                reason = %"roster-unreadable",
                %error,
                "could not re-read which channels are restricted; keeping the last answer"
            ),
        }
    }

    /// Set the bit directly. The boot path's own, and a test seam.
    pub fn set_gating(&self, gating: bool) {
        self.0.gating.store(gating, Ordering::Relaxed);
    }

    /// Resolve a presented grant into what its holder may hear.
    ///
    /// Costs **no statement** when nothing is gated, and none when nothing was
    /// presented — so the only request that pays for this feature is one from a
    /// browser that has actually unlocked something.
    pub async fn viewer(
        &self,
        db: &Db,
        grant: Option<&str>,
        now_ms: i64,
    ) -> Result<Viewer, Failure> {
        if !self.is_gating() {
            return Ok(Viewer::unrestricted());
        }
        let Some(grant) = grant else {
            return Ok(Viewer {
                scope: AccessScope::open(),
                held: Held::Nothing,
            });
        };
        let found = crate::db::repo::code_by_grant(db, grant)
            .await
            .map_err(Stage::Access.failed())?;
        Ok(match found {
            None => Viewer {
                scope: AccessScope::open(),
                held: Held::Stale(Stale::Unknown),
            },
            Some(row) if expired(&row, now_ms) => Viewer {
                scope: AccessScope::open(),
                held: Held::Stale(Stale::Expired),
            },
            Some(row) => Viewer {
                scope: AccessScope::granting(scope_of(&row)),
                held: Held::Code(CodeHeld {
                    id: row.id,
                    label: row.label,
                    expires_at_ms: row.expires_at_ms,
                    max_connections: row.max_connections,
                }),
            },
        })
    }

    /// Prove knowledge of an **Access code** and be handed its grant.
    ///
    /// The order is the security design, and it is rdio's bug written the right
    /// way round: the lockout is consulted **before** any Argon2 work, and an
    /// [`Unlocked`] — the only thing that carries a grant — is produced only
    /// after expiry has been checked. rdio assigns the scope to the client
    /// *first* and checks expiry afterwards, so a client told `expired` there
    /// keeps the access it was told it did not have.
    pub async fn unlock(
        &self,
        db: &Db,
        code: &str,
        from: std::net::IpAddr,
        now_ms: i64,
    ) -> Result<Unlocked, Failure> {
        let now = Instant::now();
        if let Some(left) = self.lockout().locked_for(from, now, self.budget()) {
            return Err(Reason::UnlockLockedOut {
                client_addr: from,
                retry_after_secs: left.as_secs(),
            }
            .into());
        }

        // Disabled codes are not candidates at all: the durable off has to be
        // indistinguishable from a revoke to whoever is holding the code, and
        // distinguishable in the admin listing, which is where the Operator who
        // switched it off is looking.
        let candidates = crate::db::repo::unlockable_codes(db)
            .await
            .map_err(Stage::Access.failed())?;

        // Argon2id is memory-hard on purpose, which makes this the one place in
        // the process where work scales with the size of a roster: there is no
        // way to *look up* a salted hash, so every enabled code is a candidate.
        // An Operator's roster is a handful, the lockout above bounds how often
        // anyone may spend it, and it runs on a blocking thread because the
        // multiplier is real and a Pi has four of them — `admin::login` pays
        // this cost exactly once and so pays it inline.
        let candidate = code.to_string();
        let matched = tokio::task::spawn_blocking(move || {
            candidates
                .into_iter()
                .find(|row| verify(&row.code_hash, &candidate))
        })
        .await
        .map_err(Stage::Access.failed())?;

        let Some(row) = matched else {
            let failures = self.lockout().record_failure(from, now, self.budget());
            return Err(Reason::InvalidAccessCode {
                client_addr: from,
                failures,
            }
            .into());
        };

        // Deliberately **not** charged to the lockout: whoever typed this knows
        // the code, and there is nothing left for them to guess at. What it is
        // charged to is the log, at WARN, because "the code you handed your
        // firefighters stopped working last night" is precisely what an Operator
        // needs told.
        if expired(&row, now_ms) {
            return Err(Reason::AccessCodeExpired { code_id: row.id }.into());
        }

        self.lockout().clear(from);
        Ok(Unlocked {
            grant: row.grant,
            label: row.label,
            scope: scope_of_stored(row.scope.as_deref()),
            expires_at_ms: row.expires_at_ms,
        })
    }

    /// Take a live-feed connection slot for a code, or find the limit spent.
    ///
    /// The slot is returned by dropping what this hands back, which is the
    /// [`crate::listeners::Listeners`] shape and for its reason: a connection
    /// ends in several ways — a close frame, a socket error, a task cancelled
    /// out from under it — and a decrement that some arm has to remember is a
    /// decrement some arm will not.
    pub fn hold(&self, code: &CodeHeld) -> Option<Holding> {
        let mut held = self.0.connections.lock().expect("connections");
        let taken = held.entry(code.id).or_insert(0);
        if let Some(limit) = code.max_connections
            && i64::from(*taken) >= limit
        {
            return None;
        }
        *taken += 1;
        Some(Holding {
            access: self.clone(),
            code_id: code.id,
        })
    }

    /// How many live-feed connections a code is holding right now. The admin
    /// listing's column, and what `tests/access.rs` asserts a drop releases.
    pub fn connections_held(&self, code_id: i64) -> u32 {
        self.0
            .connections
            .lock()
            .expect("connections")
            .get(&code_id)
            .copied()
            .unwrap_or(0)
    }

    fn budget(&self) -> Budget {
        Budget {
            attempts: self.0.config.lockout_attempts,
            cooldown: self.0.config.lockout,
        }
    }

    fn lockout(&self) -> MutexGuard<'_, Lockout> {
        self.0.lockout.lock().expect("lockout")
    }
}

/// One live-feed connection's claim on an **Access code**'s connection limit.
///
/// Releases on drop, so there is no arm of the connection loop that has to
/// remember to give it back.
pub struct Holding {
    access: Access,
    code_id: i64,
}

impl Drop for Holding {
    fn drop(&mut self) {
        let mut held = self.access.0.connections.lock().expect("connections");
        match held.get_mut(&self.code_id) {
            // The entry is removed at zero rather than left at it, so a roster
            // an Operator churns through does not leave a map growing by one
            // per code that was ever connected to.
            Some(1) | None => {
                held.remove(&self.code_id);
            }
            Some(taken) => *taken -= 1,
        }
    }
}

/// What a successful unlock hands back.
///
/// The **one** place a grant appears in a response, which is what makes it
/// findable: [`crate::curate::codes::CodeRow`] has no field for it, so no later
/// edit to the admin listing can start carrying one by accident.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Unlocked {
    /// What the browser keeps and presents on every gated read.
    pub grant: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Which restricted channels this opens — the same matrix the panel is
    /// selected with, so a client can say what it just gained without a second
    /// vocabulary for it.
    pub scope: Selection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
}

crate::answers_json!(Unlocked);

// ---------------------------------------------------------------------------
// The handler
// ---------------------------------------------------------------------------

/// What an unlock asks with. One field, and it is the secret, which is why this
/// is a `POST` body and not a query string.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnlockRequest {
    pub code: String,
}

/// `POST /api/unlock` — prove you know an **Access code** and be handed its
/// grant (#68, spec US 52).
///
/// **Listener-facing and unauthenticated**, like minting a **Share link** — a
/// Listener holds no credential, which is the whole point of this endpoint. What
/// bounds it is the [`crate::lockout::Lockout`] the admin login already uses, on
/// the address the *network* established rather than the one the request claims:
/// rdio keys its equivalent ledger on `X-Forwarded-For` unconditionally, which
/// makes a lockout both evadable and weaponisable, and its counter is per-socket
/// anyway so reconnecting resets it.
pub async fn unlock(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    axum::Json(body): axum::Json<UnlockRequest>,
) -> Result<Unlocked, Failure> {
    let client_addr = state.trusted_proxies.client_ip(
        peer.ip(),
        headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok()),
    );

    // **Trimmed, here and at creation**: a code is something somebody reads out
    // and somebody else pastes, and the whitespace either side of it was never
    // part of what was said. `curate::codes` trims what it stores for the same
    // reason, so the two cannot disagree about what the code *is*.
    let unlocked = state
        .access
        .unlock(
            &state.db,
            body.code.trim(),
            client_addr,
            state.clock.now_ms(),
        )
        .await?;

    // **INFO, and with no address on it.** A refused attempt names its source,
    // because rule 5 exempts an authentication attempt and an Operator cannot
    // firewall what they cannot see; a *successful* one must not, because
    // "10.0.0.7 listens to PD Tac" is exactly the record of who listened that
    // rule exists to keep an Instance from accumulating. The **label** is what
    // is named, never the code and never the grant.
    // Built before the macro rather than inside it, for the reason
    // `MergeChange::record` gives: a `tracing` field expression runs only when
    // a subscriber is interested, which makes it look unreachable to coverage.
    let code = unlocked.label.as_deref().unwrap_or("(unlabelled)");
    tracing::info!(code, "access code unlocked");
    Ok(unlocked)
}

// ---------------------------------------------------------------------------
// Reading a stored code
// ---------------------------------------------------------------------------

/// Whether a code's window has closed. One place, so nothing can disagree about
/// the edge — [`crate::share::live_at`]'s rule, one feature along.
pub fn expired(row: &access_code::Model, now_ms: i64) -> bool {
    row.expires_at_ms.is_some_and(|at| at <= now_ms)
}

/// What a stored scope opens.
///
/// A scope that will not parse — or that is absent — opens **nothing**, which is
/// [`crate::selection::stored`]'s rule and is the safe direction here for a
/// sharper reason than it is for a **Downstream**: the alternative is a row
/// nobody can read granting a stranger the Operator's whole County.
///
/// The wildcard is therefore spelled `{"all": true}` — a Selection that reaches
/// every System, exactly as a Downstream scoped to everything spells it — rather
/// than by leaving the column empty.
fn scope_of_stored(stored: Option<&str>) -> Selection {
    crate::selection::stored(stored.unwrap_or_default())
}

fn scope_of(row: &access_code::Model) -> Selection {
    scope_of_stored(row.scope.as_deref())
}

/// Whether `scope` may have this Call, asked of the database.
///
/// **The gate for the surfaces that hold a Call id and nothing else** — its
/// audio, its download, a Share link being minted for it, a **Star** being left
/// on it. [`crate::archive::gate`] is the same question as a `WHERE` clause,
/// which is what the reads answering with *many* Calls use; this is the one-Call
/// form, and it costs no statement at all when nothing is gated.
///
/// A Call that is not there answers `false`, so a caller has exactly one refusal
/// to write: [`crate::failure::Reason::CallNotFound`], which is also the right
/// answer for one that is there and out of scope — see
/// [`crate::archive::reachable`].
pub async fn reaches_call<C: sea_orm::ConnectionTrait>(
    db: &C,
    scope: &AccessScope,
    id: crate::call::CallId,
) -> Result<bool, sea_orm::DbErr> {
    if scope.reaches_everything() {
        return Ok(true);
    }
    Ok(crate::db::repo::call_channel(db, id)
        .await?
        .is_some_and(|on| scope.permits(on.system_ref, on.talkgroup_ref, on.restricted)))
}

// ---------------------------------------------------------------------------
// Minting and proving
// ---------------------------------------------------------------------------

/// A fresh grant: [`GRANT_PREFIX`] and 128 random bits, hex.
pub fn mint_grant() -> String {
    let mut bytes = [0u8; GRANT_BYTES];
    OsRng.fill_bytes(&mut bytes);
    let mut grant = String::from(GRANT_PREFIX);
    for byte in bytes {
        grant.push_str(&format!("{byte:02x}"));
    }
    grant
}

/// Store an Operator-chosen code: Argon2id at the crate's defaults, which are
/// OWASP's (19 MiB, t=2, p=1) — the admin password's own.
pub fn hash_code(code: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(code.as_bytes(), &salt)
        .expect("argon2 accepts any byte string")
        .to_string()
}

/// Whether `candidate` is the code behind `stored`.
///
/// A stored hash we cannot read verifies nothing — the same reading
/// [`crate::admin::AdminAuth::verify`] takes, and for its reason: a hash we
/// cannot parse is one we did not write.
fn verify(stored: &str, candidate: &str) -> bool {
    PasswordHash::new(stored).is_ok_and(|hash| {
        Argon2::default()
            .verify_password(candidate.as_bytes(), &hash)
            .is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn scope(json: serde_json::Value) -> AccessScope {
        AccessScope::granting(scope_of_stored(Some(&json.to_string())))
    }

    fn code(max_connections: Option<i64>) -> CodeHeld {
        CodeHeld {
            id: 7,
            label: Some(String::from("Fire Ops")),
            expires_at_ms: None,
            max_connections,
        }
    }

    // --- What a scope permits ------------------------------------------------

    /// **The whole of "an instance with no codes behaves exactly as today".**
    /// An open channel is reachable by a Listener holding nothing at all, and
    /// every arm of every scope agrees about that — which is what keeps a
    /// feature nobody has switched on from changing what anybody hears.
    #[rstest]
    #[case::nothing_held(AccessScope::open())]
    #[case::a_code_for_somewhere_else(scope(serde_json::json!({"sel": {"22": {"*": true}}})))]
    #[case::a_master_code(AccessScope::All)]
    fn an_unrestricted_channel_is_reachable_by_anybody(#[case] scope: AccessScope) {
        assert!(scope.permits(11, 100, false));
    }

    /// ...and the mirror: holding nothing reaches no gated channel, which is the
    /// sentence the whole ticket is.
    #[test]
    fn a_restricted_channel_is_reachable_by_nobody_holding_nothing() {
        assert!(!AccessScope::open().permits(11, 500, true));
    }

    #[rstest]
    #[case::the_named_channel(11, 500, true, "what the code names")]
    #[case::a_sibling_channel(11, 501, false, "a gated channel the code does not name")]
    #[case::another_system(22, 500, false, "the same Ref on a System the code says nothing about")]
    #[case::an_open_channel_here(11, 100, true, "open channels are still open")]
    fn a_scoped_code_opens_what_it_names_and_nothing_else(
        #[case] system_ref: i64,
        #[case] talkgroup_ref: i64,
        #[case] permitted: bool,
        #[case] why: &str,
    ) {
        let scope = scope(serde_json::json!({"sel": {"11": {"500": true}}}));
        // Every channel here is gated except 100, so the case table reads as
        // "what does this code open" rather than as two tables interleaved.
        let restricted = talkgroup_ref != 100;

        assert_eq!(
            scope.permits(system_ref, talkgroup_ref, restricted),
            permitted,
            "{why}"
        );
    }

    /// A System-wide grant reaches a channel minted after the code was written —
    /// which is the case that matters, because auto-populate (#8) is how most
    /// channels on a real Instance come to exist.
    #[test]
    fn a_system_wide_code_reaches_a_channel_that_did_not_exist_yet() {
        let scope = scope(serde_json::json!({"sel": {"11": {"*": true}}}));

        assert!(scope.permits(11, 9_999, true));
        assert!(
            !scope.permits(22, 9_999, true),
            "and only on its own System"
        );
    }

    /// A code reaching every Call there is **is** [`AccessScope::All`], so the
    /// Archive has no predicate to apply and no joins to make for it. Normalizing
    /// here rather than at each call site is what makes that true everywhere at
    /// once.
    #[test]
    fn a_code_that_reaches_everything_normalizes_to_no_gate_at_all() {
        assert_eq!(scope(serde_json::json!({"all": true})), AccessScope::All);
        assert_eq!(
            scope(serde_json::json!({"all": true, "sel": {"11": {"500": true}}})),
            AccessScope::All,
            "entries that agree with the default say nothing"
        );
        assert!(
            !scope(serde_json::json!({"all": true, "sel": {"11": {"500": false}}}))
                .reaches_everything(),
            "...and one that disagrees is a real exception"
        );
    }

    /// A scope nobody can read opens **nothing** — the safe direction, and the
    /// one [`crate::selection::stored`] takes for a **Downstream** too. The
    /// alternative is a corrupt row handing a stranger the Operator's County.
    #[rstest]
    #[case::unreadable("not json at all")]
    #[case::empty("")]
    #[case::the_wrong_shape(r#"["11"]"#)]
    fn a_scope_that_will_not_parse_opens_nothing(#[case] stored: &str) {
        let scope = AccessScope::granting(scope_of_stored(Some(stored)));

        assert!(!scope.permits(11, 500, true), "no gated channel");
        assert!(
            scope.permits(11, 100, false),
            "and open listening is untouched"
        );
    }

    /// An absent scope is read the same way, which is why the wildcard has to be
    /// spelled `{"all": true}`: a code created with the column left empty must
    /// not be a master key.
    #[test]
    fn an_absent_scope_is_not_a_wildcard() {
        assert!(!AccessScope::granting(scope_of_stored(None)).permits(11, 500, true));
    }

    /// The default is open listening rather than [`AccessScope::All`], so a value
    /// assembled by mistake grants the least rather than the most.
    #[test]
    fn the_default_scope_grants_the_least() {
        assert_eq!(AccessScope::default(), AccessScope::open());
        assert!(!AccessScope::default().permits(11, 500, true));
    }

    // --- Expiry --------------------------------------------------------------

    #[rstest]
    #[case::never(None, false)]
    #[case::tomorrow(Some(2_000), false)]
    #[case::this_instant(Some(1_000), true)]
    #[case::yesterday(Some(500), true)]
    fn a_window_closes_on_its_edge(#[case] expires_at_ms: Option<i64>, #[case] closed: bool) {
        let row = access_code::Model {
            id: 1,
            label: None,
            code_hash: String::new(),
            grant: String::new(),
            scope: None,
            expires_at_ms,
            max_connections: None,
            disabled: false,
            created_at_ms: 0,
        };

        assert_eq!(expired(&row, 1_000), closed);
    }

    // --- Connection limits ---------------------------------------------------

    /// The limit counts **connections held right now**, and a dropped connection
    /// gives its slot back. rdio compares `Access` pointers and rebuilds its
    /// roster on every configuration write, so editing an unrelated setting
    /// silently resets every code's count to zero.
    #[test]
    fn a_limit_is_spent_by_connections_and_returned_by_dropping_them() {
        let access = Access::default();
        let code = code(Some(2));

        let first = access.hold(&code).expect("the first connection");
        let second = access.hold(&code).expect("the second");
        assert_eq!(access.connections_held(code.id), 2);
        assert!(access.hold(&code).is_none(), "the third is over the limit");

        drop(second);
        assert_eq!(access.connections_held(code.id), 1);
        let third = access.hold(&code).expect("the slot came back");

        drop(first);
        drop(third);
        assert_eq!(
            access.connections_held(code.id),
            0,
            "a code nobody is holding counts nothing"
        );
    }

    /// A code with no limit is not counted *down*, but it is still counted — the
    /// admin listing shows the number, and there is no second bookkeeping for it.
    #[test]
    fn a_code_with_no_limit_still_says_how_many_are_connected() {
        let access = Access::default();
        let code = code(None);

        let held: Vec<Holding> = (0..5)
            .map(|_| access.hold(&code).expect("no limit"))
            .collect();

        assert_eq!(access.connections_held(code.id), 5);
        drop(held);
        assert_eq!(access.connections_held(code.id), 0);
    }

    /// A limit of zero admits nobody. It is a strange thing to configure and a
    /// perfectly ordinary thing to type, and "zero means unlimited" is the
    /// reading that would hand out the Operator's most sensitive channel.
    #[test]
    fn a_limit_of_zero_admits_nobody() {
        let access = Access::default();

        assert!(access.hold(&code(Some(0))).is_none());
    }

    /// One code's connections are not another's.
    #[test]
    fn two_codes_do_not_share_a_limit() {
        let access = Access::default();
        let fire = code(Some(1));
        let police = CodeHeld {
            id: 8,
            ..code(Some(1))
        };

        let _held = access.hold(&fire).expect("fire");

        assert!(
            access.hold(&police).is_some(),
            "a different code, its own budget"
        );
        assert!(access.hold(&fire).is_none(), "and fire is still spent");
    }

    // --- The credential ------------------------------------------------------

    /// A grant is unguessable and recognisable — 128 bits, and a prefix so one
    /// found in a paste is not mistaken for an API key or a share token.
    #[test]
    fn a_minted_grant_is_unguessable_and_recognisable() {
        let (first, second) = (mint_grant(), mint_grant());

        assert_ne!(first, second);
        assert!(first.starts_with(GRANT_PREFIX), "{first}");
        assert_eq!(first.len(), GRANT_PREFIX.len() + GRANT_BYTES * 2, "{first}");
        assert!(
            first[GRANT_PREFIX.len()..]
                .chars()
                .all(|c| c.is_ascii_hexdigit()),
            "{first}"
        );
    }

    /// A roster we could not re-read leaves the bit **where it was**, which for
    /// a gate is the only safe answer: defaulting to `false` on a database
    /// hiccup would turn it into an unlocked Instance.
    #[test]
    fn a_roster_that_will_not_read_keeps_the_last_answer() {
        let access = Access::default();
        access.arm(Ok(true));

        access.arm(Err(sea_orm::DbErr::Custom(String::from("no such table"))));

        assert!(access.is_gating(), "kept, rather than quietly opened");
    }

    /// A code is stored Argon2id and salted, so two Instances with the same code
    /// hold different bytes and a stolen database is not a stolen roster. This is
    /// the whole difference from `api_keys.key_hash`, which is unsalted SHA-256
    /// and defensible only for 128 minted bits (#51).
    #[test]
    fn a_code_is_salted_argon2id_and_verifies() {
        let (first, second) = (hash_code("FIRE2024"), hash_code("FIRE2024"));

        assert_ne!(first, second, "salted");
        assert!(first.starts_with("$argon2id$"), "{first}");
        assert!(verify(&first, "FIRE2024"));
        assert!(verify(&second, "FIRE2024"));
        assert!(!verify(&first, "fire2024"), "and it is case-sensitive");
    }

    /// A stored hash we cannot read verifies nothing, rather than verifying
    /// everything — which is what a naive string compare against a corrupt row
    /// would do.
    #[test]
    fn a_hash_we_did_not_write_verifies_nothing() {
        assert!(!verify("", ""));
        assert!(!verify("FIRE2024", "FIRE2024"));
    }
}
