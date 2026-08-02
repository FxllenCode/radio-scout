//! What a handler answers when it cannot answer with what was asked for
//! (ADR-0011 rules 3, 4 and 6; #29, reshaped by #92).
//!
//! On 2026-07-25 a missing column made every ingest return HTTP 500, and the raw
//! SQL error travelled to Trunk Recorder while the server recorded nothing. Both
//! halves of that are wrong, and they are the same bug: the one machine that
//! could have written the cause down handed it to the one that couldn't.
//!
//! **A handler returns a [`Failure`], never a response.** Two arms, because
//! there are exactly two kinds of not-answering and they are owed different
//! things:
//!
//! - **A break** ([`Failure::broke`]) names a [`Stage`] — where the code was —
//!   and carries the cause. The cause is logged at ERROR against the request id,
//!   in the span the failure happened in (so an ingest 500 still names its
//!   System and Talkgroup), and the client gets that id and nothing else. An
//!   operator greps one id and finds everything; internals stop travelling to
//!   clients and into pasted logs.
//! - **A refusal** (any [`Reason`], via `?`) says what the *caller* did wrong.
//!   One exhaustive match yields its slug, its level, its status and its body
//!   together, and [`Reason`]'s `IntoResponse` is the only way a refusal becomes
//!   a response — so a refusal that does not log is not expressible, and a new
//!   arm that decides none of those four things does not compile. That is rules
//!   3 and 6 made structural: before #92 they were a convention spread over four
//!   funnels in three modules, which had already drifted into two spellings of
//!   `reason=`.
//!
//! **Two vocabularies, deliberately not one.** A Stage is Operator diagnostics
//! ("an Operator must act"); a Reason is "a Recorder sent something we refused".
//! Merging them would make #70's rejections-by-reason metric ambiguous between
//! the two, which is precisely the split ADR-0011's levels exist for.
//!
//! **The redaction stays in the request middleware**, applied to **every** 5xx
//! and not only to the ones built here — a future handler that 500s some other
//! way is covered by construction rather than by remembering. Such a response
//! has no cause to log, which is itself worth an ERROR line: a 500 nobody wrote
//! down is the thing this module exists to prevent.
//!
//! The rdio-compatible strings are a wire contract (ADR-0001) and live on the
//! [`Reason`] arms that answer with them — including `Call imported
//! successfully.`, which two *rejections* answer with so a recorder never
//! retries a Call we deliberately dropped.

use std::net::IpAddr;

use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tracing::{Level, Span, error};

use crate::http_log::RequestId;

/// Where the server was when it broke — the **Stage** vocabulary.
///
/// Operator diagnostics, and deliberately fine-grained: `stage=dedup` answers
/// "which query" without a backtrace. Deliberately *not* the same vocabulary as
/// a [`Reason`], which says what the caller did wrong — conflating the two would
/// make #70's "rejections by reason" metric ambiguous between "an Operator must
/// act" and "a Recorder sent something we refused", which is the split
/// ADR-0011's levels exist for, and would put unvalidated cardinality behind a
/// metric label.
///
/// Written as one table so that [`Stage::ALL`] cannot go stale: a variant is
/// spelled once, with its slug, and both the enum and the list are generated
/// from it. Every slug is what the pre-#92 free-form string said, because those
/// strings are what an operator's saved greps and the suite's own assertions are
/// written against.
macro_rules! stages {
    ($($(#[$meta:meta])* $variant:ident => $slug:literal,)+) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Stage {
            $($(#[$meta])* $variant,)+
        }

        impl Stage {
            /// Every stage there is — the vocabulary itself, for a test to walk.
            pub const ALL: &'static [Stage] = &[$(Stage::$variant,)+];

            /// The slug an operator greps: `stage=dedup`.
            pub fn slug(self) -> &'static str {
                match self {
                    $(Stage::$variant => $slug,)+
                }
            }
        }
    };
}

stages! {
    // -- Ingest (`crate::ingest`) -------------------------------------------
    /// Checking the recorder's API key against the System it named.
    Auth => "auth",
    /// Giving a new System the lowest free Ref, for a recorder that sent none.
    AssignSystemRef => "assign-system-ref",
    /// Deciding whether an unknown System/Talkgroup may be created (#8).
    AutoPopulatePolicy => "auto-populate-policy",
    /// Looking for a Call already stored inside the dedup window.
    Dedup => "dedup",
    /// Writing the audio object, before the row (ADR-0001).
    StoreAudio => "store-audio",
    /// Inserting the Call row and its children, in one transaction.
    StoreCall => "store-call",
    /// Reading the stored Call back as the view the live feed carries.
    BuildCallView => "build-call-view",
    /// Resolving Trunk Recorder's `short_name` to a System Ref.
    ResolveSystem => "resolve-system",
    // -- Serving audio (`crate::serve`) -------------------------------------
    /// Finding the Call a listener asked for the audio of.
    LookUpCall => "look-up-call",
    /// Signing a presigned URL for an object-store backend (#31).
    SignAudioUrl => "sign-audio-url",
    /// Asking the store how big the object is.
    StatAudio => "stat-audio",
    /// Reading the object.
    ReadAudio => "read-audio",
    /// Reading a byte range of it.
    ReadAudioRange => "read-audio-range",
    // -- Reading the Archive (`crate::archive`, `crate::catalog`) -----------
    /// One page of archive search.
    SearchCalls => "search-calls",
    /// The cascading filter options behind that page.
    LoadFilterOptions => "load-filter-options",
    /// One Call, with everything the recorder said about it.
    LoadCallDetail => "load-call-detail",
    /// The Systems and Talkgroups a listener can select.
    LoadCatalog => "load-catalog",
    // -- The admin surface -------------------------------------------------
    /// One page of the operator log (#30).
    SearchLogs => "search-logs",
    /// Applying a Talkgroup CSV (#18).
    ImportTalkgroups => "import-talkgroups",
    // -- Web Push (#16) ----------------------------------------------------
    /// Storing (or re-storing) a device's subscription.
    StorePushSubscription => "store-push-subscription",
    /// Forgetting one.
    DeletePushSubscription => "delete-push-subscription",
}

impl Stage {
    /// This stage's failure, ready for `map_err` — `.map_err(Stage::Dedup.failed())`.
    ///
    /// A combinator rather than a closure at each of the twenty-odd call sites,
    /// so the only thing a handler writes is *which* stage: the span capture and
    /// the rendering are not its business and cannot be got wrong there.
    pub fn failed<E: std::fmt::Display>(self) -> impl FnOnce(E) -> Failure {
        move |cause| Failure::broke(self, cause)
    }
}

/// Declare that a type is answered as `200` with its JSON.
///
/// One decision spelled once rather than eight identical `impl`s, which is what
/// lets a handler name *what* it answers with and never *how* (#92). A macro
/// rather than a blanket `impl<T: Serialize>`, because that would claim every
/// serializable type in the crate — including the ones that are answered as
/// bytes, as a redirect, or as nothing at all.
#[macro_export]
macro_rules! answers_json {
    ($($ty:ty),+ $(,)?) => {
        $(impl axum::response::IntoResponse for $ty {
            fn into_response(self) -> axum::response::Response {
                axum::Json(self).into_response()
            }
        })+
    };
}

/// What the caller did wrong — the **Reason** vocabulary.
///
/// Closed, and every arm carries exactly what its wire form and its log line
/// need. One exhaustive match ([`Reason::refusal`]) turns an arm into all four
/// decisions at once — slug, level, status, body — so an arm added without them
/// does not compile, and a rejection path cannot skip the log line because
/// there is nowhere else to obtain a `Reason`.
///
/// Deliberately not the same vocabulary as a [`Stage`]: this is "a Recorder sent
/// something we refused", where a Stage is "an Operator must act".
///
/// Two arms describe *our* state rather than a mistake — [`Reason::AudioNotFound`]
/// and [`Reason::PushNotConfigured`] — and they still belong here, because what
/// they tell the caller is the same thing: stop asking for this, nothing an
/// Operator does will change the answer. What separates them from a [`Stage`] is
/// that nobody has to go and fix anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    // -- Ingest (ADR-0001's wire contract) ---------------------------------
    /// The key was not valid for the System the upload named.
    ///
    /// The Refs ride the body because rdio-scanner's string carries them, and a
    /// recorder branches on that string. They are *not* logged — the span the
    /// rejection happens in already names them — and neither, ever, is the key
    /// (rule 2): an unknown key has no row to name it by anyway.
    InvalidApiKey { system_ref: i64, talkgroup_ref: i64 },
    /// The same transmission, already stored inside the dedup window.
    Duplicate,
    /// The Talkgroup is blacklisted (#8). Answered `200` so the recorder never
    /// retries, which makes the WARN line the only record it happened.
    Blacklisted,
    /// An unknown System or Talkgroup, with auto-populate off (#8). Answered
    /// `200` for the same reason.
    NotPopulated,
    /// A body too incomplete to be a Call — the rdio `417` family.
    Incomplete(Incomplete),
    /// Trunk Recorder's own dialect, with a `meta` part that isn't JSON. Its
    /// wire string is not the `Incomplete call data: …` shape, so it is its own
    /// arm rather than a member of the family above.
    InvalidMeta,
    // -- Reading the Archive ------------------------------------------------
    /// No Call with that Id.
    CallNotFound,
    /// An **Encrypted Call**: a row with no audio object behind it at all (#42,
    /// spec US 9). The wire already omits `audioUrl` for these, so anything
    /// asking is a hand-built URL or a stale link — and both deserve the truth.
    CallHasNoAudio,
    /// The row points at an object the store does not have — pruned between the
    /// read and the fetch, which retention is entitled to do.
    AudioNotFound,
    /// A `Range` header naming bytes this object does not have.
    RangeNotSatisfiable { size: u64 },
    /// A filter that could not be read, naming the parameter at fault.
    ///
    /// rdio-scanner coerces whatever it can and silently ignores the rest, so a
    /// typo returns plausible wrong results; the detail is the whole difference.
    BadQuery(String),
    // -- Web Push (#16) -----------------------------------------------------
    /// This Instance has no VAPID identity, so there are no notifications to
    /// offer and nothing to subscribe to.
    PushNotConfigured,
    /// A subscription we could not deliver to, refused before it is stored.
    ///
    /// The endpoint is never named: it is a stable per-device identifier, which
    /// is exactly what rule 5 keeps out of an operator's log.
    BadSubscription(crate::webpush::InvalidSubscription),
    // -- The admin surface (#19) -------------------------------------------
    /// No session cookie at all.
    NoSession,
    /// A session cookie naming a session that is not live — never opened,
    /// logged out, or expired.
    UnknownSession,
    /// A state-changing admin request that did not echo its session's CSRF
    /// token.
    CsrfMismatch,
    /// A wrong admin password, and the running failure count for the address.
    InvalidPassword { client_addr: IpAddr, failures: u32 },
    /// An address that has spent its budget of failed logins. Refused *before*
    /// the password is checked, so it costs no Argon2 work — and refused even
    /// when it guesses right, because a lockout that let the winning guess
    /// through would not be one.
    AdminLockedOut {
        client_addr: IpAddr,
        retry_after_secs: u64,
    },
    // -- Talkgroup import (#18) --------------------------------------------
    /// A CSV that could not be read at all, in the JSON shape the client
    /// renders a successful import's report in.
    ///
    /// Carries the parse error itself rather than a slug and a sentence, for
    /// the same reason [`Reason::BadSubscription`] does: a `&'static str` here
    /// would be an *open* arm in a closed vocabulary, and #70 is about to put
    /// these slugs behind a metric label.
    BadImport(crate::import::ParseError),
}

/// The rdio `417` family: a body too incomplete to be a Call.
///
/// One type, because the wire detail **is** the slug with its dashes spelled as
/// spaces — `no-talkgroup` ⇄ `Incomplete call data: no talkgroup`. One string
/// rather than two that can drift, at the cost that renaming a slug rewrites a
/// recorder-facing string (which is why every body here is also pinned verbatim
/// in `tests/instrumentation.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Incomplete {
    /// A Talkgroup is mandatory — the load-bearing health-check string.
    NoTalkgroup,
    /// No audio part, or a present-and-empty one: a recorder that produced no
    /// samples. A zero-byte Call would play as silence forever.
    NoAudio,
    /// Trunk Recorder's dialect with no `meta` part.
    NoMeta,
    /// The stream ended inside a part's headers: nothing was readable at all.
    MalformedMultipartBody,
    /// Headers complete, body cut off — the part exists, its value doesn't.
    CouldNotReadField,
    /// The same, for the audio part.
    CouldNotReadAudio,
}

impl Incomplete {
    fn slug(self) -> &'static str {
        match self {
            Incomplete::NoTalkgroup => "no-talkgroup",
            Incomplete::NoAudio => "no-audio",
            Incomplete::NoMeta => "no-meta",
            Incomplete::MalformedMultipartBody => "malformed-multipart-body",
            Incomplete::CouldNotReadField => "could-not-read-field",
            Incomplete::CouldNotReadAudio => "could-not-read-audio",
        }
    }
}

/// Everything one [`Reason`] decides, produced by a single exhaustive match.
///
/// A struct rather than four `match`es, because four could disagree about which
/// arms they cover — and because the point of the type is that adding an arm
/// forces *all* of it. The four fields are required; the rest are attached by
/// the arms that have something extra to say.
struct Refusal {
    slug: &'static str,
    level: Level,
    status: StatusCode,
    body: Told,
    /// A header the wire form owes: `Retry-After`, `Content-Range`.
    header: Option<(HeaderName, HeaderValue)>,
    /// Rule 5's third class — an authentication attempt's source. Absent
    /// everywhere else, and absent rather than `None`, so an assertion about
    /// who was *not* recorded can look for the field and find nothing.
    client_addr: Option<IpAddr>,
    /// The running failure count on a wrong password.
    failures: Option<u32>,
    /// How long a locked-out address must wait — the same reading the
    /// `Retry-After` header carries, so the two cannot disagree.
    retry_after_secs: Option<u64>,
}

/// What the caller is handed: text for everything a recorder or a listener
/// reads, JSON for the one surface whose success is JSON too.
enum Told {
    Text(String),
    Json(serde_json::Value),
}

impl Refusal {
    /// The four decisions every arm owes.
    fn new(slug: &'static str, level: Level, status: StatusCode, body: Told) -> Self {
        Refusal {
            slug,
            level,
            status,
            body,
            header: None,
            client_addr: None,
            failures: None,
            retry_after_secs: None,
        }
    }

    /// Rule 5's exemption, taken deliberately by the two arms it covers.
    fn attempted_from(mut self, client_addr: IpAddr) -> Self {
        self.client_addr = Some(client_addr);
        self
    }

    fn failures(mut self, failures: u32) -> Self {
        self.failures = Some(failures);
        self
    }

    /// One reading, spent twice: the header the client obeys and the field the
    /// operator reads.
    fn retry_after(mut self, secs: u64) -> Self {
        self.retry_after_secs = Some(secs);
        self.header(header::RETRY_AFTER, secs.to_string())
    }

    fn header(mut self, name: HeaderName, value: String) -> Self {
        // Every value here is built from a number or a slug of our own, so it
        // is a valid header by construction; the fallback exists so a mistake
        // costs a missing header rather than a panic in a refusal path.
        self.header = HeaderValue::from_str(&value)
            .ok()
            .map(|value| (name, value));
        self
    }
}

/// `text/plain` bodies, all of which end in the newline a recorder's parser and
/// a `curl` both expect.
fn text(body: impl Into<String>) -> Told {
    Told::Text(body.into())
}

impl Reason {
    /// The slug an operator greps: `reason=duplicate`.
    pub fn slug(&self) -> &'static str {
        self.refusal().slug
    }

    /// What the caller is told, in words — the body, without its trailing
    /// newline.
    ///
    /// A window onto a refusal for whoever built it: the three read surfaces
    /// name the parameter at fault in here, and that naming is the whole
    /// difference from rdio-scanner, which coerces what it can and says nothing.
    /// **Never a log line** — a line carries [`Reason::slug`] and structured
    /// fields, never a formatted sentence (ADR-0011 rule 6) — which is why this
    /// exists only under `cfg(test)`: a caller who wants these words is a client,
    /// and a client gets them as a response body.
    #[cfg(test)]
    pub(crate) fn told(&self) -> String {
        match self.refusal().body {
            Told::Text(text) => text.trim_end().to_owned(),
            Told::Json(json) => json.to_string(),
        }
    }

    /// Everything this reason decides. **The exhaustive match** — no wildcard,
    /// deliberately, so a new arm cannot be added without saying what the caller
    /// is told and whether an Operator must act.
    fn refusal(&self) -> Refusal {
        match self {
            Reason::InvalidApiKey {
                system_ref,
                talkgroup_ref,
            } => Refusal::new(
                "invalid-api-key",
                Level::WARN,
                StatusCode::UNAUTHORIZED,
                text(format!(
                    "Invalid API key for system {system_ref} talkgroup {talkgroup_ref}.\n"
                )),
            ),
            Reason::Duplicate => Refusal::new(
                "duplicate",
                Level::WARN,
                StatusCode::OK,
                text("duplicate call rejected\n"),
            ),
            Reason::Blacklisted => Refusal::new(
                "blacklisted",
                Level::WARN,
                StatusCode::OK,
                text(CALL_IMPORTED),
            ),
            Reason::NotPopulated => Refusal::new(
                "not-populated",
                Level::WARN,
                StatusCode::OK,
                text(CALL_IMPORTED),
            ),
            Reason::Incomplete(incomplete) => Refusal::new(
                incomplete.slug(),
                Level::WARN,
                StatusCode::EXPECTATION_FAILED,
                text(format!(
                    "Incomplete call data: {}\n",
                    incomplete.slug().replace('-', " ")
                )),
            ),
            Reason::InvalidMeta => Refusal::new(
                "invalid-meta",
                Level::WARN,
                StatusCode::EXPECTATION_FAILED,
                text("Invalid call data\n"),
            ),
            Reason::CallNotFound => Refusal::new(
                "call-not-found",
                Level::DEBUG,
                StatusCode::NOT_FOUND,
                text("call not found\n"),
            ),
            Reason::CallHasNoAudio => Refusal::new(
                "call-has-no-audio",
                Level::DEBUG,
                StatusCode::NOT_FOUND,
                text("call has no audio\n"),
            ),
            Reason::AudioNotFound => Refusal::new(
                "audio-not-found",
                Level::DEBUG,
                StatusCode::NOT_FOUND,
                text("audio not found\n"),
            ),
            Reason::RangeNotSatisfiable { size } => Refusal::new(
                "range-not-satisfiable",
                Level::DEBUG,
                StatusCode::RANGE_NOT_SATISFIABLE,
                text("range not satisfiable\n"),
            )
            .header(header::CONTENT_RANGE, format!("bytes */{size}")),
            Reason::BadQuery(detail) => Refusal::new(
                "bad-query",
                Level::DEBUG,
                StatusCode::BAD_REQUEST,
                text(format!("{detail}\n")),
            ),
            // **DEBUG, and one body for both routes** (changed by #92). Push is
            // off in what ships, so a browser asking `/api/push/key` on an
            // ordinary instance is the *normal* answer, not news — at WARN it
            // would be a warning per page load on every instance nobody
            // configured push on (rule 8). The two routes used to disagree about
            // the body as well (`push is not configured` here, `not-configured`
            // there); one condition now has one answer.
            Reason::PushNotConfigured => Refusal::new(
                "not-configured",
                Level::DEBUG,
                StatusCode::NOT_FOUND,
                text("not-configured\n"),
            ),
            Reason::BadSubscription(invalid) => Refusal::new(
                invalid.reason(),
                Level::WARN,
                StatusCode::BAD_REQUEST,
                text(format!("{}\n", invalid.reason())),
            ),
            Reason::NoSession => Refusal::new(
                "no-session",
                Level::DEBUG,
                StatusCode::UNAUTHORIZED,
                text(ADMIN_SESSION_REQUIRED),
            ),
            Reason::UnknownSession => Refusal::new(
                "unknown-session",
                Level::DEBUG,
                StatusCode::UNAUTHORIZED,
                text(ADMIN_SESSION_REQUIRED),
            ),
            Reason::CsrfMismatch => Refusal::new(
                "csrf-mismatch",
                Level::DEBUG,
                StatusCode::FORBIDDEN,
                text("csrf token required\n"),
            ),
            Reason::InvalidPassword {
                client_addr,
                failures,
            } => Refusal::new(
                "invalid-password",
                Level::WARN,
                StatusCode::UNAUTHORIZED,
                text("invalid password\n"),
            )
            .attempted_from(*client_addr)
            .failures(*failures),
            Reason::AdminLockedOut {
                client_addr,
                retry_after_secs,
            } => Refusal::new(
                "admin-locked-out",
                Level::WARN,
                StatusCode::TOO_MANY_REQUESTS,
                text("too many failed logins; try again later\n"),
            )
            .attempted_from(*client_addr)
            .retry_after(*retry_after_secs),
            Reason::BadImport(error) => Refusal::new(
                error.reason(),
                Level::DEBUG,
                StatusCode::BAD_REQUEST,
                Told::Json(
                    serde_json::json!({ "error": error.reason(), "detail": error.to_string() }),
                ),
            ),
        }
    }
}

/// The rdio-compatible success string.
///
/// It lives here rather than beside [`crate::ingest::Imported`] because two
/// *rejections* answer with it too — a recorder must never retry a Call we
/// deliberately dropped — and one string is the only way the success and the
/// two silences cannot drift apart.
pub(crate) const CALL_IMPORTED: &str = "Call imported successfully.\n";

/// Which of the guard's refusals applied is for the operator's log, not for
/// whoever is knocking.
const ADMIN_SESSION_REQUIRED: &str = "admin session required\n";

impl IntoResponse for Reason {
    /// **The one rendering.** A refusal cannot reach a caller without leaving
    /// its line, because this is the only way a `Reason` becomes a response —
    /// which is what makes ADR-0011 rule 3 structural rather than a convention
    /// a new rejection path has to remember.
    fn into_response(self) -> Response {
        let refusal = self.refusal();
        // `tracing` bakes an event's level into a static callsite, so a level
        // chosen at runtime means one callsite per level — the same shape the
        // request line uses. The *fields* are written once, and an `Option`
        // that is `None` records nothing at all, so a line about a duplicate
        // carries no empty address.
        macro_rules! emit {
            ($emit:ident) => {
                tracing::$emit!(
                    reason = %refusal.slug,
                    client_addr = refusal.client_addr.as_ref().map(tracing::field::display),
                    failures = refusal.failures,
                    retry_after_secs = refusal.retry_after_secs,
                    "request refused"
                )
            };
        }
        match refusal.level {
            Level::WARN => emit!(warn),
            // Everything the request log's own WARN already covers, and which
            // an Operator does not have to act on. `refusal` never chooses
            // ERROR — a 5xx is a `Broke`, not a `Reason` — nor TRACE.
            _ => emit!(debug),
        }

        let mut response = match refusal.body {
            Told::Text(body) => (refusal.status, body).into_response(),
            Told::Json(body) => (refusal.status, axum::Json(body)).into_response(),
        };
        if let Some((name, value)) = refusal.header {
            response.headers_mut().insert(name, value);
        }
        response
    }
}

/// What a handler returns when it cannot answer with what was asked for — the
/// one error type every route shares.
///
/// Two arms, because there are exactly two kinds of not-answering and they are
/// owed different things. A **refusal** is the caller's fault: it has a
/// [`Reason`], a wire form the caller is entitled to, and a level that says
/// whether an Operator must act. A **break** is ours rather than the caller's:
/// they are told nothing but where to look, and the cause goes to the log
/// against the request id.
#[derive(Debug, Clone)]
pub struct Failure {
    kind: Kind,
    /// Where it happened, captured where the `Failure` was built and re-entered
    /// when the line is finally written.
    ///
    /// **Both arms need this, and for the same reason.** A handler returns its
    /// failure; axum renders it *afterwards*, outside every span the handler ran
    /// in. Without the capture, a rejected upload's line would carry the request
    /// id and nothing else — no System, no Talkgroup — and "one upload is one
    /// span" (ADR-0011) would hold for the lines a handler writes itself and
    /// quietly stop holding for the one line the *refusal* writes, which on a
    /// dropped Call is the only line there is.
    origin: Span,
}

#[derive(Debug, Clone)]
enum Kind {
    /// The caller asked for something we refuse to do.
    Refused(Reason),
    /// The server broke at a named [`Stage`], with the error's own words.
    Broke { stage: Stage, cause: String },
}

impl Failure {
    /// A failure at `stage`, carrying the underlying error's own words.
    pub fn broke(stage: Stage, cause: impl std::fmt::Display) -> Self {
        Failure::here(Kind::Broke {
            stage,
            cause: cause.to_string(),
        })
    }

    fn here(kind: Kind) -> Self {
        Failure {
            kind,
            origin: Span::current(),
        }
    }
}

impl From<Reason> for Failure {
    fn from(reason: Reason) -> Self {
        Failure::here(Kind::Refused(reason))
    }
}

impl From<Incomplete> for Failure {
    fn from(incomplete: Incomplete) -> Self {
        Reason::Incomplete(incomplete).into()
    }
}

/// An internal failure on its way to becoming a 500, as [`redact`] finds it in
/// the response's extensions.
#[derive(Debug, Clone)]
pub struct Broke {
    stage: Stage,
    /// The underlying error's own words, which go to the log and nowhere else.
    cause: String,
    origin: Span,
}

impl IntoResponse for Failure {
    /// A refusal is rendered and logged in one place ([`Reason`]); a break is a
    /// 500 carrying its cause in the response extensions, where [`redact`] picks
    /// it up. The break's body is written there too, since only the middleware
    /// knows the request id.
    fn into_response(self) -> Response {
        let Failure { kind, origin } = self;
        match kind {
            Kind::Refused(reason) => origin.in_scope(|| reason.into_response()),
            Kind::Broke { stage, cause } => {
                let mut response = StatusCode::INTERNAL_SERVER_ERROR.into_response();
                response.extensions_mut().insert(Broke {
                    stage,
                    cause,
                    origin,
                });
                response
            }
        }
    }
}

/// The body a client gets for any 5xx: the request id, and nothing else.
///
/// Called a **request id** rather than a ref, because `CONTEXT.md` spends *Ref*
/// on the external identifier a recorder sends (`systemRef`, `talkgroupRef`) —
/// and the header, the span field and the type have all called this a request id
/// since #28. One word, one meaning.
fn request_id_body(request_id: &RequestId) -> String {
    format!("internal error (request id: {request_id})\n")
}

/// Record a failing response's cause against `request_id` and replace its body
/// with the request id alone. Anything that isn't a 5xx passes through
/// untouched.
///
/// Call this inside the request's span, so the ERROR line carries the same
/// `request_id` the client is handed.
pub(crate) fn redact(response: Response, request_id: &RequestId) -> Response {
    let status = response.status();
    if !status.is_server_error() {
        return response;
    }
    match response.extensions().get::<Broke>() {
        // Written where it happened, not where it was noticed: re-entering the
        // handler's span puts the upload's System and Talkgroup back on the line
        // (and the request id with them, since that span is a child of this one).
        // `%stage`, not the default: `stage=dedup` greps and `stage="dedup"`
        // does not (as with the request log's path).
        Some(broke) => broke
            .origin
            .in_scope(|| error!(stage = %broke.stage.slug(), cause = %broke.cause, "server error")),
        // Not reachable from our own handlers, and deliberately loud if it ever
        // is: a 500 with no recorded cause is the 2026-07-25 failure mode.
        None => error!("server error with no recorded cause"),
    }
    (status, request_id_body(request_id)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::LogCapture;
    use crate::webpush::InvalidSubscription;
    use axum::body::to_bytes;
    use axum::http::{HeaderName, header};
    use rstest::rstest;
    use std::net::IpAddr;

    const LOCALHOST: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

    async fn body_text(response: Response) -> String {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        String::from_utf8(bytes.to_vec()).expect("utf-8 body")
    }

    fn header_of(response: &Response, name: HeaderName) -> String {
        response
            .headers()
            .get(&name)
            .unwrap_or_else(|| panic!("a {name} header"))
            .to_str()
            .expect("an ascii header")
            .to_owned()
    }

    /// Every arm of the caller-fault vocabulary decides all four things at once:
    /// the slug an operator greps, the level that says whether they must act,
    /// the status the caller gets and the bytes it reads. This is the table an
    /// arm added without those decisions fails.
    ///
    /// The rdio-compatible strings are a wire contract (ADR-0001) and are pinned
    /// here byte for byte — including the two rejections that answer `200 Call
    /// imported successfully.` so a recorder never retries, which is exactly why
    /// the WARN line beside them is the only record they happened.
    #[rstest]
    // -- Ingest: the recorder-facing contract -------------------------------
    #[case::invalid_api_key(
        Reason::InvalidApiKey { system_ref: 11, talkgroup_ref: 54241 },
        "invalid-api-key", 401, "Invalid API key for system 11 talkgroup 54241.\n", " WARN "
    )]
    #[case::duplicate(
        Reason::Duplicate,
        "duplicate",
        200,
        "duplicate call rejected\n",
        " WARN "
    )]
    #[case::blacklisted(
        Reason::Blacklisted,
        "blacklisted",
        200,
        "Call imported successfully.\n",
        " WARN "
    )]
    #[case::not_populated(
        Reason::NotPopulated,
        "not-populated",
        200,
        "Call imported successfully.\n",
        " WARN "
    )]
    #[case::invalid_meta(
        Reason::InvalidMeta,
        "invalid-meta",
        417,
        "Invalid call data\n",
        " WARN "
    )]
    // ...and the 417 family, whose wire detail *is* the slug with its dashes
    // spelled as spaces, so a renamed slug rewrites the recorder-facing string.
    #[case::no_talkgroup(
        Reason::Incomplete(Incomplete::NoTalkgroup),
        "no-talkgroup",
        417,
        "Incomplete call data: no talkgroup\n",
        " WARN "
    )]
    #[case::no_audio(
        Reason::Incomplete(Incomplete::NoAudio),
        "no-audio",
        417,
        "Incomplete call data: no audio\n",
        " WARN "
    )]
    #[case::no_meta(
        Reason::Incomplete(Incomplete::NoMeta),
        "no-meta",
        417,
        "Incomplete call data: no meta\n",
        " WARN "
    )]
    #[case::malformed_body(
        Reason::Incomplete(Incomplete::MalformedMultipartBody),
        "malformed-multipart-body",
        417,
        "Incomplete call data: malformed multipart body\n",
        " WARN "
    )]
    #[case::could_not_read_field(
        Reason::Incomplete(Incomplete::CouldNotReadField),
        "could-not-read-field",
        417,
        "Incomplete call data: could not read field\n",
        " WARN "
    )]
    #[case::could_not_read_audio(
        Reason::Incomplete(Incomplete::CouldNotReadAudio),
        "could-not-read-audio",
        417,
        "Incomplete call data: could not read audio\n",
        " WARN "
    )]
    // -- Reading: a listener asking for something that isn't there ----------
    //
    // DEBUG, not WARN: the request log already emits a WARN line for the 4xx
    // (#28), and a media element re-requesting a pruned Call's audio by range
    // would otherwise write a warning per range (rule 8).
    #[case::call_not_found(
        Reason::CallNotFound,
        "call-not-found",
        404,
        "call not found\n",
        " DEBUG "
    )]
    #[case::call_has_no_audio(
        Reason::CallHasNoAudio,
        "call-has-no-audio",
        404,
        "call has no audio\n",
        " DEBUG "
    )]
    #[case::audio_not_found(
        Reason::AudioNotFound,
        "audio-not-found",
        404,
        "audio not found\n",
        " DEBUG "
    )]
    #[case::unsatisfiable(
        Reason::RangeNotSatisfiable { size: 10 },
        "range-not-satisfiable", 416, "range not satisfiable\n", " DEBUG "
    )]
    #[case::bad_query(
        Reason::BadQuery("system must be an integer".to_string()),
        "bad-query", 400, "system must be an integer\n", " DEBUG "
    )]
    // -- Web Push ----------------------------------------------------------
    #[case::push_off(
        Reason::PushNotConfigured,
        "not-configured",
        404,
        "not-configured\n",
        " DEBUG "
    )]
    #[case::bad_subscription(
        Reason::BadSubscription(InvalidSubscription::PublicKey),
        "bad-key",
        400,
        "bad-key\n",
        " WARN "
    )]
    // -- The admin surface -------------------------------------------------
    //
    // The guard's own refusals are DEBUG: a tab left open overnight, or an
    // internet-facing instance being probed for `/admin`, is not news. A failed
    // *authentication* is, and is WARN below.
    #[case::no_session(
        Reason::NoSession,
        "no-session",
        401,
        "admin session required\n",
        " DEBUG "
    )]
    #[case::unknown_session(
        Reason::UnknownSession,
        "unknown-session",
        401,
        "admin session required\n",
        " DEBUG "
    )]
    #[case::csrf(
        Reason::CsrfMismatch,
        "csrf-mismatch",
        403,
        "csrf token required\n",
        " DEBUG "
    )]
    #[case::invalid_password(
        Reason::InvalidPassword { client_addr: LOCALHOST, failures: 2 },
        "invalid-password", 401, "invalid password\n", " WARN "
    )]
    #[case::locked_out(
        Reason::AdminLockedOut { client_addr: LOCALHOST, retry_after_secs: 900 },
        "admin-locked-out", 429, "too many failed logins; try again later\n", " WARN "
    )]
    #[tokio::test]
    async fn every_reason_decides_its_slug_status_body_and_level(
        #[case] reason: Reason,
        #[case] slug: &str,
        #[case] status: u16,
        #[case] body: &str,
        #[case] level: &str,
    ) {
        let capture = LogCapture::start();

        assert_eq!(reason.slug(), slug);
        let response = reason.into_response();

        assert_eq!(response.status().as_u16(), status);
        assert_eq!(body_text(response).await, body);
        let logged = capture.text();
        assert!(logged.contains(level), "{logged}");
        // Bare, never quoted: `reason=duplicate` greps and `reason="duplicate"`
        // does not. This is the drift #92 closed — `push::rejected` rendered it
        // quoted while three other funnels rendered it bare.
        assert!(logged.contains(&format!("reason={slug}")), "{logged}");
        assert!(!logged.contains("reason=\""), "quoted: {logged}");
        assert!(logged.contains("request refused"), "{logged}");
    }

    /// ADR-0011 rule 5's third class, through the one rendering. An
    /// authentication attempt may name its source — "someone is guessing my
    /// admin password" cannot be acted on without an address for a firewall
    /// rule — and the running count is what turns a stray typo into a visible
    /// attack. Nothing else may carry an address, so the field has to be absent
    /// rather than `None` on every other refusal: an assertion about who was
    /// *not* recorded has to be able to look for the field and find nothing.
    #[tokio::test]
    async fn only_an_authentication_attempt_names_its_source() {
        let capture = LogCapture::start();

        let _ = Reason::InvalidPassword {
            client_addr: LOCALHOST,
            failures: 2,
        }
        .into_response();
        let _ = Reason::AdminLockedOut {
            client_addr: LOCALHOST,
            retry_after_secs: 900,
        }
        .into_response();
        let _ = Reason::Duplicate.into_response();

        let logged = capture.text();
        let mut lines = logged.lines();
        let wrong = lines.next().expect("the wrong-password line");
        let locked = lines.next().expect("the locked-out line");
        let ingest = lines.next().expect("the ingest line");

        assert!(wrong.contains("client_addr=127.0.0.1"), "{wrong}");
        assert!(wrong.contains("failures=2"), "{wrong}");
        assert!(!wrong.contains("retry_after_secs"), "{wrong}");

        assert!(locked.contains("client_addr=127.0.0.1"), "{locked}");
        assert!(locked.contains("retry_after_secs=900"), "{locked}");
        assert!(!locked.contains("failures"), "{locked}");

        assert!(!ingest.contains("client_addr"), "{ingest}");
        assert!(!ingest.contains("failures"), "{ingest}");
        assert!(!ingest.contains("retry_after_secs"), "{ingest}");
    }

    /// A locked-out address is told how long to wait, and the header and the log
    /// line are the *same* reading — two of them could disagree, and an operator
    /// comparing their log against a client's `Retry-After` is exactly who would
    /// notice.
    #[tokio::test]
    async fn a_lockout_tells_the_client_the_same_wait_it_tells_the_log() {
        let capture = LogCapture::start();

        let response = Reason::AdminLockedOut {
            client_addr: LOCALHOST,
            retry_after_secs: 900,
        }
        .into_response();

        assert_eq!(header_of(&response, header::RETRY_AFTER), "900");
        assert!(capture.text().contains("retry_after_secs=900"));
    }

    /// A range nobody can satisfy has to say what the object's size *is*, or a
    /// media element cannot correct itself (RFC 9110 §14.4).
    #[tokio::test]
    async fn an_unsatisfiable_range_says_how_big_the_object_is() {
        let response = Reason::RangeNotSatisfiable { size: 10 }.into_response();

        assert_eq!(header_of(&response, header::CONTENT_RANGE), "bytes */10");
    }

    /// The one refusal answered as JSON: a CSV import reports its rejections in
    /// the same shape as the report a successful one returns, so the client
    /// renders one thing either way.
    #[tokio::test]
    async fn a_refused_import_answers_json_naming_what_was_wrong() {
        let capture = LogCapture::start();

        let response = Reason::BadImport(crate::import::ParseError::Empty).into_response();

        assert_eq!(response.status().as_u16(), 400);
        assert_eq!(
            header_of(&response, header::CONTENT_TYPE),
            "application/json"
        );
        let body: serde_json::Value =
            serde_json::from_str(&body_text(response).await).expect("a JSON body");
        assert_eq!(body["error"], "empty-csv");
        assert_eq!(body["detail"], "the CSV has no data rows");
        // ...and what the caller was told reads back as those same bytes, so a
        // JSON refusal is as inspectable from inside as a text one.
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &Reason::BadImport(crate::import::ParseError::Empty).told()
            )
            .expect("told() is the JSON body"),
            body
        );
        assert!(
            capture.text().contains("reason=empty-csv"),
            "{}",
            capture.text()
        );
    }

    /// **A failure is reported where it happened, not where it was rendered** —
    /// for a refusal exactly as much as for a break.
    ///
    /// A handler *returns* its failure and axum renders it afterwards, outside
    /// every span the handler ran in. Two of ingest's five rejections answer
    /// `200 Call imported successfully.` so the recorder never retries, which
    /// makes this line the only record the Call was dropped at all — a record
    /// naming neither the System nor the Talkgroup would be close to useless.
    #[tokio::test]
    async fn a_failure_is_reported_in_the_span_it_happened_in() {
        let capture = LogCapture::start();

        // Built inside the upload's span...
        let failure: Failure =
            tracing::error_span!("ingest", system_ref = 11, talkgroup_ref = 54241)
                .in_scope(|| Reason::Duplicate.into());
        // ...and rendered well outside it, the way axum does.
        let _ = failure.into_response();

        let logged = capture.text();
        assert!(logged.contains("system_ref=11"), "{logged}");
        assert!(logged.contains("talkgroup_ref=54241"), "{logged}");
        assert!(logged.contains("reason=duplicate"), "{logged}");
    }

    /// Every stage names itself as the slug an operator greps, and no two say
    /// the same thing — a vocabulary with a duplicate in it cannot tell an
    /// operator which query broke.
    #[test]
    fn every_stage_has_its_own_slug() {
        let mut seen = std::collections::HashSet::new();
        for stage in Stage::ALL {
            assert!(
                !stage.slug().is_empty(),
                "{stage:?} must name what it was doing"
            );
            assert!(
                seen.insert(stage.slug()),
                "two stages both call themselves {:?}",
                stage.slug()
            );
        }
    }

    /// The client is told where to look and nothing more; the operator is told
    /// everything, against the same request id.
    #[tokio::test]
    async fn a_failure_becomes_a_request_id_for_the_client_and_a_cause_for_the_log() {
        let capture = LogCapture::start();
        let id = RequestId::new();

        let response = Failure::broke(Stage::StoreCall, "no such column: systems.auto_populate")
            .into_response();
        let response = redact(response, &id);

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = body_text(response).await;
        assert_eq!(body, format!("internal error (request id: {id})\n"));
        assert!(
            !body.contains("auto_populate"),
            "the cause must not travel to the client: {body:?}"
        );

        let logged = capture.text();
        assert!(logged.contains(" ERROR "), "{logged}");
        assert!(logged.contains("stage=store-call"), "{logged}");
        assert!(
            logged.contains("cause=no such column: systems.auto_populate"),
            "{logged}"
        );
    }

    /// Only failures are rewritten. A 200 body is a contract elsewhere in the
    /// app — the rdio response strings above all — and must arrive untouched.
    #[rstest]
    #[case(StatusCode::OK)]
    #[case(StatusCode::UNAUTHORIZED)]
    #[case(StatusCode::EXPECTATION_FAILED)]
    #[case(StatusCode::NOT_FOUND)]
    #[tokio::test]
    async fn a_non_failure_passes_through_untouched(#[case] status: StatusCode) {
        let capture = LogCapture::start();

        let response = redact(
            (status, "Call imported successfully.\n").into_response(),
            &RequestId::new(),
        );

        assert_eq!(response.status(), status);
        assert_eq!(body_text(response).await, "Call imported successfully.\n");
        assert_eq!(
            capture.text(),
            "",
            "nothing to report: {:?}",
            capture.text()
        );
    }

    /// A 5xx from somewhere that didn't build a [`Failure`] still loses its
    /// body and still leaves a line — silence is the one outcome ruled out.
    #[tokio::test]
    async fn a_5xx_with_no_recorded_cause_is_still_redacted_and_still_logged() {
        let capture = LogCapture::start();
        let id = RequestId::new();

        let response = redact(
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "connection pool exhausted at db.rs:41\n",
            )
                .into_response(),
            &id,
        );

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body_text(response).await,
            format!("internal error (request id: {id})\n")
        );
        let logged = capture.text();
        assert!(logged.contains(" ERROR "), "{logged}");
        assert!(logged.contains("no recorded cause"), "{logged}");
        capture.assert_never_logged("db.rs:41");
    }

    /// The identifier in the body is the request id verbatim — that identity is
    /// what makes "grep the server for this string" work at all, and it is why
    /// the body stopped calling it a *ref* (which the glossary spends on a
    /// radio-network identifier).
    #[test]
    fn the_body_carries_the_request_id_verbatim() {
        let id = RequestId::new();
        assert_eq!(
            request_id_body(&id),
            format!("internal error (request id: {id})\n")
        );
        assert!(request_id_body(&id).contains(id.as_str()));
    }
}
