//! Admin authentication (#19, spec US 38, ADR-0008): a server-side session
//! behind an httpOnly cookie, a CSRF token bound to it, and a brute-force
//! lockout in front of the password.
//!
//! Everything under `/api/admin/` mutates or reveals configuration, so the
//! guard is a **prefix layer** ([`require_session`], applied in
//! [`crate::admin_routes`]) rather than a check each handler remembers. A route
//! added beside the others is protected by construction; the one route that
//! must not be — `/api/admin/login`, where there is no session yet — has to be
//! written outside the layer on purpose.
//!
//! # The shape
//!
//! ```text
//! POST /api/admin/login    { "password": … }  -> 200 { csrf_token, expires_in_secs }
//!                                                + Set-Cookie: rs_session=…
//!                                             -> 401 wrong password
//!                                             -> 429 + Retry-After, address locked out
//! GET  /api/admin/session                     -> 200 { csrf_token, expires_in_secs }
//! POST /api/admin/logout                      -> 204 + Set-Cookie: rs_session=; Max-Age=0
//! ```
//!
//! The CSRF token rides in the **body**, not a second cookie. It is a
//! synchronizer token — held server-side against the session — which an
//! attacker who can write cookies for a sibling subdomain cannot forge, as they
//! can with the widespread double-submit pattern. A page that has been reloaded
//! holds the cookie but not the token, and asks `GET /api/admin/session` for it.
//!
//! # Improving on rdio-scanner
//!
//! `server/admin.go` is the reference for *what* an admin gate does and a
//! catalogue of what to do differently. Each of these is cited at the code that
//! fixes it:
//!
//! | rdio | Radio-Scout |
//! |---|---|
//! | JWT in `localStorage`, sent as `Authorization` | **httpOnly cookie** — unreachable from anything running on the page (ADR-0008) |
//! | a shipped default password (`rdio-scanner`) with a nag | **no guessable credential ever** — first run generates one into `.env` `0600` and logs only the path |
//! | bcrypt | **Argon2id** at OWASP's parameters, so a guess costs memory as well as time |
//! | tokens with no `exp`, alive until restart | an **idle window** *and* an **absolute lifetime** ([`Entry::has_expired`]) |
//! | five tokens, oldest evicted unconditionally | [`MAX_SESSIONS`] = 32, **expired reclaimed first**, eviction logged |
//! | no CSRF defence (it doesn't need one; we do) | **synchronizer token** + `SameSite=Strict` |
//! | lockout keyed on a spoofable `X-Forwarded-For` | the address [`TrustedProxies::client_ip`] resolved (#17) |
//! | cooldown that evaluates to **zero**, so failures never decay | a real cooldown from the last attempt ([`Lockout`]) |
//! | any successful login clears **everyone's** failures | only the address that authenticated |
//! | a locked address gets the same 401 as a wrong password | **429 + `Retry-After`**, so an operator can tell them apart |

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::Json;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use crate::AppState;
use crate::config::TrustedProxies;
use crate::failure::{Failure, Reason};
use crate::startup::AdminPassword;

/// The header a reverse proxy names the original client in — the same one the
/// request log reads (#28), and believed on the same terms.
const FORWARDED_FOR: &str = "x-forwarded-for";

/// The header a reverse proxy names the original scheme in — how a server that
/// only ever speaks plain HTTP learns that the client's half of the hop was TLS.
const FORWARDED_PROTO: &str = "x-forwarded-proto";

/// The cookie carrying the session id.
pub const SESSION_COOKIE: &str = "rs_session";

/// The header an unsafe admin request echoes its session's CSRF token in.
pub const CSRF_HEADER: &str = "x-csrf-token";

/// Tuning for the admin surface — and the `[admin]` section itself (#17, #87).
///
/// The admin *password* is deliberately not here: like the ingest key, first run
/// **writes** it, so it lives in the environment and `.env`
/// (`RADIO_SCOUT_ADMIN_PASSWORD`) rather than in a file `--write-config`
/// generates. Everything about it that is a knob rather than a secret is here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AdminConfig {
    /// How long a session survives without being used. Refreshed on every
    /// authenticated request.
    #[serde(rename = "session_idle_secs", with = "crate::config::secs")]
    pub session_idle: Duration,
    /// How long a session may live at all, however active. Never refreshed.
    #[serde(rename = "session_max_secs", with = "crate::config::secs")]
    pub session_max: Duration,
    /// Failed logins one address may spend before it is locked out.
    pub lockout_attempts: u32,
    /// How long a spent address stays locked out, measured from its **last**
    /// attempt — so hammering a locked address keeps it locked, and walking
    /// away clears it.
    #[serde(rename = "lockout_secs", with = "crate::config::secs")]
    pub lockout: Duration,
}

impl Default for AdminConfig {
    fn default() -> Self {
        AdminConfig {
            session_idle: Duration::from_secs(8 * 60 * 60),
            session_max: Duration::from_secs(7 * 24 * 60 * 60),
            lockout_attempts: 5,
            lockout: Duration::from_secs(15 * 60),
        }
    }
}

/// The admin surface's credential and the sessions it has opened.
///
/// Cheap to clone — one lives in [`crate::AppState`] for the life of the
/// process.
#[derive(Clone)]
pub struct AdminAuth(Arc<Inner>);

struct Inner {
    /// The PHC-format Argon2id hash of the admin password, or `None` when none
    /// was provisioned — in which case nothing can authenticate.
    password: Option<String>,
    config: AdminConfig,
    sessions: Mutex<Sessions>,
    lockout: Mutex<Lockout>,
}

impl AdminAuth {
    /// An admin surface gated by `password`.
    pub fn new(password: &str, config: AdminConfig) -> Self {
        AdminAuth(Arc::new(Inner {
            password: Some(hash_password(password)),
            config,
            sessions: Mutex::new(Sessions::default()),
            lockout: Mutex::new(Lockout::default()),
        }))
    }

    /// The admin surface a boot's provisioning earned it (#19).
    ///
    /// This is ADR-0008's safety property in one place rather than a branch in
    /// `main.rs`: a password that could not be written to `.env` yields
    /// [`AdminPassword::NotPersisted`], which has no password, which **locks the
    /// surface**. A credential only the server ever saw would leave the operator
    /// convinced they had one. It lives here because `main.rs` is excluded from
    /// coverage, and a decision nothing can test is a decision waiting to be
    /// inverted.
    pub fn provisioned(password: &AdminPassword, config: AdminConfig) -> Self {
        match password.password() {
            Some(password) => AdminAuth::new(password, config),
            None => AdminAuth::locked(),
        }
    }

    /// An admin surface with no password provisioned: **nothing** authenticates.
    ///
    /// The safe default for a [`crate::AppState`] assembled without one, so a
    /// wiring mistake closes the config surface rather than opening it.
    pub fn locked() -> Self {
        AdminAuth(Arc::new(Inner {
            password: None,
            config: AdminConfig::default(),
            sessions: Mutex::new(Sessions::default()),
            lockout: Mutex::new(Lockout::default()),
        }))
    }

    /// Whether `candidate` is the admin password.
    ///
    /// One expression rather than a chain of guards, because both `None` cases
    /// mean the same thing: an unprovisioned surface authenticates nothing, and
    /// a stored hash we cannot read is one we did not write.
    pub fn verify(&self, candidate: &str) -> bool {
        self.0
            .password
            .as_deref()
            .and_then(|stored| PasswordHash::new(stored).ok())
            .is_some_and(|hash| {
                Argon2::default()
                    .verify_password(candidate.as_bytes(), &hash)
                    .is_ok()
            })
    }

    /// Open a session, returning the id for its cookie and what the client is
    /// told about it.
    pub fn open_session(&self, now: Instant) -> (String, SessionResponse) {
        self.0
            .sessions
            .lock()
            .expect("sessions")
            .open(now, &self.0.config)
    }

    /// Look a session up and mark it used. `None` means there is no such live
    /// session — it was never opened, it was logged out, or it has expired.
    pub fn touch_session(&self, id: &str, now: Instant) -> Option<SessionResponse> {
        self.0
            .sessions
            .lock()
            .expect("sessions")
            .touch(id, now, &self.0.config)
    }

    /// Forget a session. Idempotent — logging out twice is not an error, and
    /// neither is logging out of a session that has already expired.
    pub fn revoke_session(&self, id: &str) {
        self.0.sessions.lock().expect("sessions").live.remove(id);
    }

    /// How long `addr` must wait before it may try a password again.
    pub fn locked_for(&self, addr: IpAddr, now: Instant) -> Option<Duration> {
        self.0
            .lockout
            .lock()
            .expect("lockout")
            .locked_for(addr, now, &self.0.config)
    }

    /// Charge `addr` for a wrong password, returning its running failure count.
    pub fn record_failure(&self, addr: IpAddr, now: Instant) -> u32 {
        self.0
            .lockout
            .lock()
            .expect("lockout")
            .record_failure(addr, now, &self.0.config)
    }

    /// Forget `addr`'s failures, because it just proved it knows the password.
    pub fn clear_failures(&self, addr: IpAddr) {
        self.0.lockout.lock().expect("lockout").clear(addr);
    }

    /// The configuration this surface runs on.
    pub fn config(&self) -> &AdminConfig {
        &self.0.config
    }
}

/// The live sessions, keyed by id.
#[derive(Default)]
struct Sessions {
    live: HashMap<String, Entry>,
}

/// What is remembered about one session.
struct Entry {
    csrf: String,
    opened_at: Instant,
    last_seen: Instant,
}

/// Failed logins, per address.
///
/// rdio-scanner has one of these and it does not work. Its cooldown is
/// `time.Duration(time.Duration.Minutes(10))` (`admin.go:64`) — a method
/// expression that evaluates to **zero**, not ten minutes — so failures never
/// decay and three wrong passwords lock an address until the process restarts;
/// and the only thing that ever clears the ledger is a successful login, which
/// clears it for *everybody*. Both are fixed here, and the decay is the fix
/// that matters: a lockout an operator cannot wait out is a lockout they have
/// to restart the scanner to escape.
#[derive(Default)]
struct Lockout {
    failures: HashMap<IpAddr, Failures>,
}

/// One address's record: how many failures, and when the last one was.
struct Failures {
    count: u32,
    last: Instant,
}

impl Lockout {
    /// How long `addr` must wait before it may try again, or `None` if it may
    /// try now.
    ///
    /// Checked *before* the password is verified, so a locked-out address costs
    /// no Argon2 work — the memory-hard hash that makes each guess expensive
    /// would otherwise make a lockout a way to exhaust a Pi's memory.
    fn locked_for(&self, addr: IpAddr, now: Instant, config: &AdminConfig) -> Option<Duration> {
        let record = self.failures.get(&addr)?;
        if record.count < config.lockout_attempts {
            return None;
        }
        match config.lockout.checked_sub(now.duration_since(record.last)) {
            // Rounded up, so a caller told to wait never gets 0 — and never
            // retries a fraction of a second too early.
            Some(left) if !left.is_zero() => Some(Duration::from_secs(left.as_secs() + 1)),
            _ => None,
        }
    }

    /// Charge `addr` for a wrong password. Returns the running count, so the
    /// line that reports the lockout can be written exactly once.
    fn record_failure(&mut self, addr: IpAddr, now: Instant, config: &AdminConfig) -> u32 {
        // This is also what makes failures *decay*: an address that walked away
        // for the full cooldown has its record dropped here, so the `or_insert`
        // below starts it over at zero rather than resuming. One mechanism, not
        // a sweep plus a reset that would each have to be right on their own.
        self.forget_spent(now, config);
        let record = self.failures.entry(addr).or_insert(Failures {
            count: 0,
            last: now,
        });
        record.count += 1;
        record.last = now;
        record.count
    }

    /// Forget `addr`'s failures — and only `addr`'s.
    fn clear(&mut self, addr: IpAddr) {
        self.failures.remove(&addr);
    }

    /// Drop what the ledger no longer needs, so guessing from a fresh address
    /// each time cannot grow it without bound — free for anyone holding an IPv6
    /// /64, and a Pi is the machine that would notice.
    ///
    /// A record whose cooldown has fully elapsed carries no information:
    /// [`Lockout::record_failure`] would reset its count anyway, so remembering
    /// it and forgetting it are the same behaviour. Only if that leaves the
    /// ledger still at its cap does a *live* record go, and then the least
    /// recently seen — which at worst hands the most idle attacker its budget
    /// back. Run on failure rather than on a timer: an attempt is already
    /// paying for an Argon2 hash, so a walk of a small map is free beside it,
    /// and nothing grows while nobody is guessing.
    fn forget_spent(&mut self, now: Instant, config: &AdminConfig) {
        self.failures
            .retain(|_, record| now.duration_since(record.last) < config.lockout);
        if self.failures.len() < MAX_TRACKED_ADDRESSES {
            return;
        }
        while self.failures.len() >= MAX_TRACKED_ADDRESSES {
            let stalest = self
                .failures
                .iter()
                .min_by_key(|(_, record)| record.last)
                .map(|(addr, _)| *addr)
                // The loop runs only while the map is at a non-zero cap.
                .expect("a full ledger has a least recently seen address");
            self.failures.remove(&stalest);
        }
        tracing::warn!(
            limit = MAX_TRACKED_ADDRESSES,
            "admin lockout ledger is full; the least recently seen addresses were forgotten"
        );
    }
}

/// How many addresses the lockout ledger tracks at once.
///
/// Far above what a real instance sees — an operator mistyping their password
/// is one address — and small enough that the whole map is a rounding error on
/// a Pi. It exists so that the ledger is bounded by *something* even inside a
/// single cooldown window, when [`Lockout::forget_spent`]'s expiry sweep has
/// nothing to reclaim.
const MAX_TRACKED_ADDRESSES: usize = 1_024;

/// How many sessions may be live at once.
///
/// A backstop on memory, not a policy: an operator with a laptop, a phone and a
/// tablet is nowhere near it, so in practice the expiry windows are what bound
/// the table. rdio-scanner's equivalent is **five** (`admin.go:441`), low enough
/// that ordinary use hits it — and hitting it silently signs out whoever logged
/// in first.
const MAX_SESSIONS: usize = 32;

impl Sessions {
    /// Record a new session, returning the id for its cookie and what the
    /// client is told about it.
    fn open(&mut self, now: Instant, config: &AdminConfig) -> (String, SessionResponse) {
        self.make_room(now, config);
        let id = random_token();
        let entry = Entry {
            csrf: random_token(),
            opened_at: now,
            last_seen: now,
        };
        let described = entry.describe(now, config);
        self.live.insert(id.clone(), entry);
        (id, described)
    }

    /// Make room for one more session, if the table is full.
    ///
    /// Expired entries go first — reclaiming those is free, and on a scanner
    /// that has been up for months they are what the table is made of. Only if
    /// that leaves no room is a live session evicted, and then the least
    /// recently used one, with a line saying so: silently signing an operator
    /// out is the rdio behaviour worth not repeating.
    fn make_room(&mut self, now: Instant, config: &AdminConfig) {
        if self.live.len() < MAX_SESSIONS {
            return;
        }
        self.live.retain(|_, entry| !entry.has_expired(now, config));
        if self.live.len() < MAX_SESSIONS {
            return;
        }
        let stalest = self
            .live
            .iter()
            .min_by_key(|(_, entry)| entry.last_seen)
            .map(|(id, _)| id.clone())
            // The table holds at least `MAX_SESSIONS` entries by the check
            // above, and `MAX_SESSIONS` is not zero.
            .expect("a full table has a least recently used entry");
        self.live.remove(&stalest);
        tracing::warn!(
            limit = MAX_SESSIONS,
            "admin session limit reached; the least recently used session was signed out"
        );
    }

    /// Mark a session used, and describe what is left of it — or `None`, having
    /// forgotten it, if it is no longer live.
    fn touch(&mut self, id: &str, now: Instant, config: &AdminConfig) -> Option<SessionResponse> {
        let entry = self.live.get_mut(id)?;
        if entry.has_expired(now, config) {
            self.live.remove(id);
            return None;
        }
        entry.last_seen = now;
        Some(entry.describe(now, config))
    }
}

impl Entry {
    /// Whether this session is past either bound: idle since it was last used,
    /// or simply old.
    ///
    /// Two clocks rather than one, because they answer different questions.
    /// The idle window is refreshed by use, so an operator working through a
    /// long evening is never logged out mid-edit; the absolute lifetime never
    /// is, so a cookie somebody walked off with cannot be kept alive forever by
    /// using it. rdio-scanner's tokens carry no `exp` claim at all
    /// (`admin.go:433`) and live until the process restarts.
    fn has_expired(&self, now: Instant, config: &AdminConfig) -> bool {
        now.duration_since(self.last_seen) >= config.session_idle
            || now.duration_since(self.opened_at) >= config.session_max
    }

    /// What the client is told about this session: the token it must echo, and
    /// how long it has left.
    fn describe(&self, now: Instant, config: &AdminConfig) -> SessionResponse {
        SessionResponse {
            csrf_token: self.csrf.clone(),
            expires_in_secs: self.expires_in(now, config).as_secs(),
        }
    }

    /// How long this session has left — the sooner of the two bounds, since
    /// whichever arrives first is the one that ends it.
    fn expires_in(&self, now: Instant, config: &AdminConfig) -> Duration {
        let idle = config
            .session_idle
            .saturating_sub(now.duration_since(self.last_seen));
        let absolute = config
            .session_max
            .saturating_sub(now.duration_since(self.opened_at));
        idle.min(absolute)
    }
}

/// What `POST /api/admin/login` takes.
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    password: String,
}

/// What an authenticated client is told about its session.
///
/// The CSRF token rides in the **body**, not a second cookie: a token the
/// server holds against the session is a synchronizer token, which an attacker
/// who can write cookies on a sibling subdomain cannot forge — the classic
/// double-submit pattern can be.
#[derive(Clone, Debug, Serialize)]
pub struct SessionResponse {
    csrf_token: String,
    expires_in_secs: u64,
}

crate::answers_json!(SessionResponse);

/// `POST /api/admin/login` — exchange the admin password for a session.
///
/// The order is the security design: the lockout is consulted **before** the
/// password, so a spent address costs no Argon2 work, and a locked address is
/// refused even when it guesses right — a lockout that let the winning guess
/// through would not be one.
///
/// ADR-0011's rule 5 keeps listener addresses out of the log, and this is the
/// third class the rule names (recorders on ingest being the second): an
/// authentication *attempt* may say where it came from at WARN, because
/// "someone is guessing my admin password" is unactionable without an address
/// to put in a firewall rule, and it is not a record of who listened.
pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Result<OpenedSession, Failure> {
    let now = Instant::now();
    // The address the network established, not the one the request claims —
    // unless the operator named that peer as a proxy (#17). Believing
    // `X-Forwarded-For` unconditionally, as rdio does, makes the lockout both
    // evadable and weaponisable.
    let client_addr = state.trusted_proxies.client_ip(
        peer.ip(),
        headers
            .get(FORWARDED_FOR)
            .and_then(|value| value.to_str().ok()),
    );

    if let Some(retry_after) = state.admin.locked_for(client_addr, now) {
        return Err(Reason::AdminLockedOut {
            client_addr,
            // One reading, spent twice: `Reason` puts it in the operator's log
            // *and* in the client's `Retry-After`, so the two cannot disagree.
            retry_after_secs: retry_after.as_secs(),
        }
        .into());
    }

    if !state.admin.verify(&body.password) {
        return Err(Reason::InvalidPassword {
            client_addr,
            failures: state.admin.record_failure(client_addr, now),
        }
        .into());
    }

    // Only this address's record, unlike rdio's whole-ledger sweep.
    state.admin.clear_failures(client_addr);
    let (id, session) = state.admin.open_session(now);
    Ok(OpenedSession {
        cookie: session_cookie(
            &id,
            state.admin.config(),
            arrived_over_tls(&headers, peer.ip(), &state.trusted_proxies),
        ),
        session,
    })
}

/// A session that has just been opened: the cookie that carries it, and what the
/// client is told about it.
pub struct OpenedSession {
    cookie: String,
    session: SessionResponse,
}

impl IntoResponse for OpenedSession {
    fn into_response(self) -> Response {
        ([(header::SET_COOKIE, self.cookie)], Json(self.session)).into_response()
    }
}

/// `GET /api/admin/session` — what the live session is, for a page that has
/// just been reloaded and holds the cookie but not the token.
///
/// Safe, so it needs no CSRF token of its own. It asks [`AdminAuth`] itself
/// rather than being handed the guard's answer through the request extensions
/// (#92): an extension is a hand-off nothing type-checks, so a route registered
/// outside the guard would have compiled and 500ed at the first request. Asking
/// costs a second `touch_session`, which is the same idempotent call the guard
/// just made — one more lock on a map, on the one admin route that exists to
/// answer this question.
pub async fn session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<SessionResponse, Failure> {
    live_session(&state.admin, &headers, Instant::now())
}

/// The session a request proves it holds, or the refusal it has earned.
///
/// Split out from the handler because its two refusals are unreachable *behind*
/// [`require_session`] — which has already turned both away — and are exactly
/// what makes the route safe to mount anywhere. That is not a hypothetical: the
/// hand-off this replaced was a request extension, and a route registered
/// outside the guard would have compiled fine and then 500ed on a missing
/// extension. Here it 401s, and this is where that is proven, since no request
/// through the real router can reach it.
fn live_session(
    admin: &AdminAuth,
    headers: &HeaderMap,
    now: Instant,
) -> Result<SessionResponse, Failure> {
    let id = session_id_of(headers).ok_or(Reason::NoSession)?;
    admin
        .touch_session(&id, now)
        .ok_or(Reason::UnknownSession.into())
}

/// `POST /api/admin/logout` — revoke this session.
///
/// Server-side, not merely by clearing the cookie: ADR-0008 chose session state
/// over a JWT precisely so that revocation is real. rdio-scanner's logout drops
/// the token from an in-memory slice, which is the same idea — but its slice
/// holds five, so logging in a sixth time revokes somebody else instead.
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> LoggedOut {
    if let Some(id) = session_id_of(&headers) {
        state.admin.revoke_session(&id);
    }
    LoggedOut
}

/// The session is revoked and the cookie removed — nothing left to say.
pub struct LoggedOut;

impl IntoResponse for LoggedOut {
    fn into_response(self) -> Response {
        (
            StatusCode::NO_CONTENT,
            [(header::SET_COOKIE, cleared_session_cookie())],
        )
            .into_response()
    }
}

/// Middleware over `/api/admin/` (bar the login route): every request must
/// carry a live session.
///
/// A **prefix layer**, not a per-handler check, so a route added to the admin
/// router is gated by construction rather than by remembering — which is the
/// mistake #18 had to be shipped around.
pub async fn require_session(
    State(admin): State<AdminAuth>,
    request: Request,
    next: Next,
) -> Result<Response, Failure> {
    let id = session_id_of(request.headers()).ok_or(Reason::NoSession)?;
    let session = admin
        .touch_session(&id, Instant::now())
        .ok_or(Reason::UnknownSession)?;
    if changes_state(request.method())
        && !csrf_token_matches(request.headers(), &session.csrf_token)
    {
        return Err(Reason::CsrfMismatch.into());
    }
    Ok(next.run(request).await)
}

/// Whether a method may change anything. The safe methods (RFC 9110 §9.2.1)
/// need no CSRF token; everything else does, including the ones no admin route
/// uses yet — a check that enumerated the *unsafe* methods would let a `PATCH`
/// added later through.
fn changes_state(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// Whether the request echoes the CSRF token its session is bound to.
///
/// A **synchronizer token**: the value is held server-side against the session,
/// so it cannot be forged by an attacker who can write cookies for the site —
/// which the widespread double-submit pattern (token in a readable cookie,
/// compared against the header) is vulnerable to from any sibling subdomain.
/// rdio-scanner needs none of this because it authenticates with an
/// `Authorization` header; a cookie session buys XSS resistance (ADR-0008) and
/// owes CSRF protection in exchange.
fn csrf_token_matches(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(CSRF_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|offered| constant_time_eq(offered.as_bytes(), expected.as_bytes()))
}

/// Compare two secrets without leaking where they first differ.
///
/// Through `subtle` rather than a hand-rolled fold: the lengths are not secret
/// — every token here is 64 hex characters — but the bytes are, and a plain
/// loop is one the optimiser is entitled to turn back into an early exit.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

/// The session id carried by the `Cookie` header, if there is one.
///
/// Hand-parsed rather than through a cookie crate: one header, one name, and
/// the parse is the security boundary — worth being able to read in full and
/// to property-test.
fn session_id_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies
                .split(';')
                .filter_map(|pair| pair.split_once('='))
                .find(|(name, _)| name.trim() == SESSION_COOKIE)
                .map(|(_, value)| value.trim().to_string())
        })
        .filter(|id| !id.is_empty())
}

/// The `Set-Cookie` value carrying a session id.
///
/// `HttpOnly` keeps it out of reach of anything running on the page (ADR-0008
/// rejects rdio's JWT-in-localStorage for exactly this); `SameSite=Strict` is
/// the first line of CSRF defence, and this app is single-origin so nothing is
/// lost by it. `Max-Age` is the session's absolute lifetime — the server
/// decides expiry, and the browser is merely told not to keep it longer.
///
/// `Secure` rides only when the request actually arrived over TLS. v1 serves
/// plain HTTP and recommends a reverse proxy for HTTPS (ADR-0008), and a
/// browser silently discards a `Secure` cookie sent over plain HTTP — so
/// setting it unconditionally would make admin login impossible on exactly the
/// zero-config LAN install this project exists to make easy.
fn session_cookie(id: &str, config: &AdminConfig, over_tls: bool) -> String {
    let secure = match over_tls {
        true => " Secure;",
        false => "",
    };
    format!(
        "{SESSION_COOKIE}={id}; HttpOnly;{secure} {COOKIE_ATTRIBUTES}; Max-Age={}",
        config.session_max.as_secs()
    )
}

/// The attributes a session cookie and its removal must agree on.
///
/// One string, because a browser matches a removal against the cookie's name,
/// path and flags: a `Set-Cookie` that omits `Path=/` clears a different cookie,
/// or none. Spelling them twice is exactly the drift this would otherwise need a
/// comment to warn about.
const COOKIE_ATTRIBUTES: &str = "SameSite=Strict; Path=/";

/// Whether this request reached us over TLS.
///
/// We only ever serve plain HTTP ourselves, so the answer is always somebody
/// else's claim — and a claim is believed on the same terms `X-Forwarded-For`
/// is (#17): from a peer the operator named in `[server] trusted_proxies`, and
/// otherwise not at all. An operator who terminates TLS without declaring the
/// proxy gets a working session without the flag, which is the safe way to be
/// wrong.
fn arrived_over_tls(headers: &HeaderMap, peer: IpAddr, trusted: &TrustedProxies) -> bool {
    trusted.trusts(peer)
        && headers
            .get(FORWARDED_PROTO)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|proto| proto.trim().eq_ignore_ascii_case("https"))
}

/// The `Set-Cookie` value that removes a session cookie.
///
/// `Secure` is deliberately absent: a removal is matched on name, path and
/// `SameSite`, and a session issued over plain HTTP has no `Secure` to match.
fn cleared_session_cookie() -> String {
    format!("{SESSION_COOKIE}=; HttpOnly; {COOKIE_ATTRIBUTES}; Max-Age=0")
}

/// Hash a password for storage, Argon2id at the crate's defaults (OWASP's
/// recommended 19 MiB / t=2 / p=1) with a fresh random salt.
fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 accepts any byte string")
        .to_string()
}

/// A 256-bit random token, hex-encoded — a session id or a CSRF token.
fn random_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    /// A `Cookie` header carrying `id` as the session cookie.
    fn cookie(id: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("{SESSION_COOKIE}={id}").parse().expect("a cookie"),
        );
        headers
    }

    /// `GET /api/admin/session` refuses rather than breaking when it is reached
    /// without a live session.
    ///
    /// Unreachable through the real router — [`require_session`] turns both of
    /// these away first — and that is the point. The hand-off this replaced was
    /// a request extension, so the same route registered *outside* the guard
    /// would have compiled and then 500ed on a missing extension, which is a
    /// server error for something the caller did (#92). Now it is a 401 either
    /// way, and this is the only place that can say so.
    #[rstest]
    #[case::no_cookie_at_all(None, "no-session")]
    #[case::a_session_nobody_opened(Some("00".repeat(32)), "unknown-session")]
    #[tokio::test]
    async fn the_session_route_refuses_without_one_wherever_it_is_mounted(
        #[case] id: Option<String>,
        #[case] reason: &str,
    ) {
        let capture = crate::testing::LogCapture::start();
        let admin = AdminAuth::new("hunter2", config());
        let headers = id.map(|id| cookie(&id)).unwrap_or_default();

        let refused = live_session(&admin, &headers, Instant::now()).expect_err("no session");

        let response = refused.into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let logged = capture.text();
        assert!(logged.contains(&format!("reason={reason}")), "{logged}");
    }

    /// ...and hands the session back when there is one, so the split from the
    /// handler did not lose the only thing it does.
    #[tokio::test]
    async fn the_session_route_reports_a_live_session() {
        let admin = AdminAuth::new("hunter2", config());
        let now = Instant::now();
        let (id, opened) = admin.open_session(now);

        let live = live_session(&admin, &cookie(&id), now).expect("a live session");

        assert_eq!(live.csrf_token, opened.csrf_token);
        assert!(live.expires_in_secs > 0);
    }

    /// A config whose windows are small enough to step over by hand.
    fn config() -> AdminConfig {
        AdminConfig {
            session_idle: Duration::from_secs(100),
            session_max: Duration::from_secs(1_000),
            lockout_attempts: 3,
            lockout: Duration::from_secs(60),
        }
    }

    fn addr(last: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, last])
    }

    /// A distinct address per `n`, past the 256 a single octet can spell — the
    /// ledger's cap is larger than that.
    fn addr_n(n: u64) -> IpAddr {
        IpAddr::from([10, 1, (n >> 8) as u8, n as u8])
    }

    // -- Sessions -----------------------------------------------------------

    /// The idle window is what an unused session dies of.
    #[test]
    fn a_session_left_alone_expires() {
        let (config, start) = (config(), Instant::now());
        let mut sessions = Sessions::default();
        let (session, _) = sessions.open(start, &config);

        assert!(
            sessions
                .touch(&session, start + Duration::from_secs(99), &config)
                .is_some(),
            "still inside the idle window"
        );
        assert!(
            sessions
                .touch(&session, start + Duration::from_secs(199), &config)
                .is_none(),
            "99 s after the last use plus 100 s idle is expired"
        );
    }

    /// ...and using it pushes that window along, so an operator working through
    /// a long evening is never logged out mid-edit.
    #[test]
    fn using_a_session_refreshes_the_idle_window() {
        let (config, start) = (config(), Instant::now());
        let mut sessions = Sessions::default();
        let (session, _) = sessions.open(start, &config);

        // A touch every 90 s keeps it alive well past a single idle window.
        for step in 1..=5 {
            assert!(
                sessions
                    .touch(&session, start + Duration::from_secs(90 * step), &config)
                    .is_some(),
                "step {step}"
            );
        }
    }

    /// The absolute lifetime is the one use cannot extend — the bound on a
    /// cookie somebody walked off with. rdio's tokens have no equivalent.
    #[test]
    fn the_absolute_lifetime_is_not_refreshed_by_use() {
        let (config, start) = (config(), Instant::now());
        let mut sessions = Sessions::default();
        let (session, _) = sessions.open(start, &config);

        // Touched every 90 s, so the idle window never lapses...
        for step in 1..11 {
            sessions.touch(&session, start + Duration::from_secs(90 * step), &config);
        }
        // ...but 990 s in it is alive and 1000 s in it is not.
        assert!(
            sessions
                .touch(&session, start + Duration::from_secs(990), &config)
                .is_some()
        );
        assert!(
            sessions
                .touch(&session, start + Duration::from_secs(1_000), &config)
                .is_none()
        );
    }

    /// An expired session is *forgotten*, not merely refused — otherwise a
    /// long-running scanner accumulates every session it ever issued.
    #[test]
    fn an_expired_session_is_dropped_from_the_table() {
        let (config, start) = (config(), Instant::now());
        let mut sessions = Sessions::default();
        let (session, _) = sessions.open(start, &config);

        sessions.touch(&session, start + Duration::from_secs(500), &config);

        assert!(sessions.live.is_empty(), "the entry should be gone");
    }

    /// Two sessions get two ids and two tokens: one operator's token must never
    /// authorize another's request.
    #[test]
    fn every_session_gets_its_own_id_and_token() {
        let (config, now) = (config(), Instant::now());
        let mut sessions = Sessions::default();

        let ids: std::collections::HashSet<String> =
            (0..100).map(|_| sessions.open(now, &config).0).collect();

        assert_eq!(ids.len(), 100);
    }

    /// The table is bounded. rdio caps its token list at five and evicts the
    /// oldest unconditionally (`admin.go:441`), so logging in from a sixth place
    /// silently signs the first one out. Here the cap is far above what an
    /// operator uses, and expired entries are reclaimed before any live one is.
    #[test]
    fn the_session_table_is_bounded_and_reclaims_expired_entries_first() {
        let (config, start) = (config(), Instant::now());
        let mut sessions = Sessions::default();

        // Fill the table with sessions that have since expired...
        let stale: Vec<String> = (0..MAX_SESSIONS)
            .map(|_| sessions.open(start, &config).0)
            .collect();
        assert_eq!(sessions.live.len(), MAX_SESSIONS);

        // ...then open one more, long after they died.
        let (fresh, _) = sessions.open(start + Duration::from_secs(500), &config);

        assert!(
            sessions.live.len() <= MAX_SESSIONS,
            "the table must stay bounded"
        );
        assert!(
            sessions.live.contains_key(&fresh),
            "the new session must survive"
        );
        assert!(
            stale.iter().all(|old| !sessions.live.contains_key(old)),
            "the expired ones should have been reclaimed"
        );
    }

    /// With nothing expired to reclaim, the cap still holds — and it is the
    /// least recently used that goes, not the newest arrival.
    #[test]
    fn a_full_table_of_live_sessions_evicts_the_least_recently_used() {
        let (config, start) = (config(), Instant::now());
        let mut sessions = Sessions::default();

        // Opened one second apart, so "least recently used" is unambiguous.
        let live: Vec<String> = (0..MAX_SESSIONS)
            .map(|step| {
                sessions
                    .open(start + Duration::from_secs(step as u64), &config)
                    .0
            })
            .collect();
        let (newest, _) = sessions.open(start + Duration::from_secs(MAX_SESSIONS as u64), &config);

        assert_eq!(sessions.live.len(), MAX_SESSIONS);
        assert!(sessions.live.contains_key(&newest));
        assert!(
            !sessions.live.contains_key(&live[0]),
            "the oldest should have gone"
        );
        assert!(
            sessions.live.contains_key(&live[MAX_SESSIONS - 1]),
            "the newest of the old should have stayed"
        );
    }

    /// What `GET /api/admin/session` reports: the sooner of the two bounds,
    /// because whichever arrives first is the one that ends the session.
    #[rstest]
    // Fresh: the idle window (100 s) is nearer than the lifetime (1000 s).
    #[case(0, 100)]
    #[case(50, 100)]
    // ...and a touch at 50 s resets it to a full 100 s
    // Touched at 950 s, the remaining lifetime (50 s) is what is left.
    #[case(950, 50)]
    fn a_session_reports_the_sooner_of_its_two_bounds(
        #[case] touched_at: u64,
        #[case] expected: u64,
    ) {
        let (config, start) = (config(), Instant::now());
        let mut sessions = Sessions::default();
        let (session, _) = sessions.open(start, &config);

        // Kept alive on the way there, since a session that sat idle for 950 s
        // would have died of the idle window long before the lifetime mattered.
        for step in (90..=touched_at).step_by(90) {
            sessions.touch(&session, start + Duration::from_secs(step), &config);
        }
        let now = start + Duration::from_secs(touched_at);
        let described = sessions.touch(&session, now, &config).expect("live");

        assert_eq!(described.expires_in_secs, expected);
    }

    /// A session nobody opened is not a session, however well-formed the id.
    #[rstest]
    #[case("")]
    #[case("not-a-session")]
    #[case("0000000000000000000000000000000000000000000000000000000000000000")]
    fn an_unknown_id_is_no_session(#[case] id: &str) {
        let (config, now) = (config(), Instant::now());
        let mut sessions = Sessions::default();
        sessions.open(now, &config);

        assert!(sessions.touch(id, now, &config).is_none(), "id={id:?}");
    }

    // -- Lockout ------------------------------------------------------------

    /// The budget, then the wall.
    #[test]
    fn an_address_is_locked_out_only_once_its_budget_is_spent() {
        let (config, now) = (config(), Instant::now());
        let mut lockout = Lockout::default();

        for attempt in 1..=2 {
            lockout.record_failure(addr(1), now, &config);
            assert_eq!(
                lockout.locked_for(addr(1), now, &config),
                None,
                "attempt {attempt} is still inside the budget"
            );
        }
        lockout.record_failure(addr(1), now, &config);

        assert!(lockout.locked_for(addr(1), now, &config).is_some());
    }

    /// The fix for rdio's zero-length cooldown: an operator who locks themselves
    /// out can wait it out instead of restarting the scanner.
    #[test]
    fn a_lockout_expires_on_its_own() {
        let (config, start) = (config(), Instant::now());
        let mut lockout = Lockout::default();
        for _ in 0..3 {
            lockout.record_failure(addr(1), start, &config);
        }

        assert!(
            lockout
                .locked_for(addr(1), start + Duration::from_secs(59), &config)
                .is_some(),
            "still inside the cooldown"
        );
        assert_eq!(
            lockout.locked_for(addr(1), start + Duration::from_secs(60), &config),
            None,
            "the cooldown is over"
        );
    }

    /// ...and waiting it out returns the whole budget, not one more try.
    #[test]
    fn waiting_out_a_lockout_restores_the_whole_budget() {
        let (config, start) = (config(), Instant::now());
        let mut lockout = Lockout::default();
        for _ in 0..3 {
            lockout.record_failure(addr(1), start, &config);
        }
        let later = start + Duration::from_secs(60);

        assert_eq!(lockout.record_failure(addr(1), later, &config), 1);
        assert_eq!(lockout.locked_for(addr(1), later, &config), None);
    }

    /// Hammering a locked address keeps it locked: the cooldown runs from the
    /// last attempt, not the one that spent the budget.
    #[test]
    fn attempts_during_a_lockout_extend_it() {
        let (config, start) = (config(), Instant::now());
        let mut lockout = Lockout::default();
        for _ in 0..3 {
            lockout.record_failure(addr(1), start, &config);
        }

        lockout.record_failure(addr(1), start + Duration::from_secs(59), &config);

        assert!(
            lockout
                .locked_for(addr(1), start + Duration::from_secs(90), &config)
                .is_some(),
            "the cooldown restarted from the last attempt"
        );
    }

    /// One address's failures are its own. rdio keys the ledger on a spoofable
    /// header *and* clears the whole thing on any success; both of those let one
    /// client spend or restore another's budget.
    #[test]
    fn addresses_do_not_share_a_budget() {
        let (config, now) = (config(), Instant::now());
        let mut lockout = Lockout::default();
        for _ in 0..3 {
            lockout.record_failure(addr(1), now, &config);
        }

        assert!(lockout.locked_for(addr(1), now, &config).is_some());
        assert_eq!(
            lockout.locked_for(addr(2), now, &config),
            None,
            "a second address starts with a full budget"
        );

        lockout.clear(addr(2));

        assert!(
            lockout.locked_for(addr(1), now, &config).is_some(),
            "clearing one address must not free another"
        );
    }

    /// The ledger is bounded, or it is a way to exhaust a Pi's memory by
    /// guessing from a fresh address each time — which an IPv6 /64 makes free.
    /// A record whose cooldown has fully elapsed carries no information (its
    /// budget is already whole again), so it is dropped rather than kept.
    #[test]
    fn a_ledger_forgets_addresses_whose_cooldown_has_passed() {
        let (config, start) = (config(), Instant::now());
        let mut lockout = Lockout::default();
        for host in 0..50 {
            lockout.record_failure(addr(host), start, &config);
        }
        assert_eq!(lockout.failures.len(), 50);

        // One more, a full cooldown later: the other 50 are spent history.
        lockout.record_failure(addr(200), start + Duration::from_secs(60), &config);

        assert_eq!(
            lockout.failures.len(),
            1,
            "only the address still inside its window should be remembered"
        );
    }

    /// ...and even inside one window the ledger cannot grow without limit: past
    /// the cap the least recently seen address is forgotten, which at worst
    /// hands a long-idle attacker its budget back.
    #[test]
    fn a_ledger_is_capped_even_when_nothing_has_expired() {
        // A cooldown long enough that nothing expires over the run, so this is
        // about the cap alone and not the sweep the test above covers.
        let config = AdminConfig {
            lockout: Duration::from_secs(24 * 60 * 60),
            ..config()
        };
        let start = Instant::now();
        let mut lockout = Lockout::default();

        // Every attempt a second apart, so "least recently seen" is unambiguous.
        for host in 0..=(MAX_TRACKED_ADDRESSES as u64) {
            lockout.record_failure(addr_n(host), start + Duration::from_secs(host), &config);
        }

        assert_eq!(lockout.failures.len(), MAX_TRACKED_ADDRESSES);
        assert!(
            !lockout.failures.contains_key(&addr_n(0)),
            "the least recently seen address should have been dropped"
        );
        assert!(
            lockout
                .failures
                .contains_key(&addr_n(MAX_TRACKED_ADDRESSES as u64)),
            "the newest attempt must be recorded"
        );
    }

    /// A `Retry-After` of 0 tells a client to try again immediately, which is
    /// the one thing a lockout must never say.
    #[test]
    fn a_lockout_never_reports_zero_seconds_left() {
        let (config, start) = (config(), Instant::now());
        let mut lockout = Lockout::default();
        for _ in 0..3 {
            lockout.record_failure(addr(1), start, &config);
        }

        for elapsed_ms in [0, 1, 999, 59_001, 59_999] {
            let left = lockout
                .locked_for(addr(1), start + Duration::from_millis(elapsed_ms), &config)
                .expect("locked");
            assert!(!left.is_zero(), "elapsed_ms={elapsed_ms} left={left:?}");
        }
    }

    // -- Cookies and tokens -------------------------------------------------

    /// The session id is read out of whatever else the browser is carrying.
    #[rstest]
    #[case("rs_session=abc", Some("abc"))]
    #[case("rs_session=abc; other=1", Some("abc"))]
    #[case("other=1; rs_session=abc", Some("abc"))]
    #[case("a=1; rs_session=abc; b=2", Some("abc"))]
    // Whitespace around the pair is how browsers actually send them.
    #[case("a=1;  rs_session=abc ", Some("abc"))]
    // Absent, empty, or a name that merely starts the same way.
    #[case("", None)]
    #[case("other=1", None)]
    #[case("rs_session=", None)]
    #[case("rs_session_old=abc", None)]
    #[case("xrs_session=abc", None)]
    // A value-less cookie has no `=` to split on.
    #[case("rs_session", None)]
    fn a_session_id_is_read_out_of_the_cookie_header(
        #[case] cookies: &str,
        #[case] expected: Option<&str>,
    ) {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, cookies.parse().expect("header value"));

        assert_eq!(
            session_id_of(&headers).as_deref(),
            expected,
            "cookies={cookies:?}"
        );
    }

    #[test]
    fn no_cookie_header_is_no_session() {
        assert_eq!(session_id_of(&HeaderMap::new()), None);
    }

    /// The safe methods need no CSRF token; everything else does, including the
    /// ones no admin route uses yet.
    #[rstest]
    #[case(Method::GET, false)]
    #[case(Method::HEAD, false)]
    #[case(Method::OPTIONS, false)]
    #[case(Method::POST, true)]
    #[case(Method::PUT, true)]
    #[case(Method::PATCH, true)]
    #[case(Method::DELETE, true)]
    #[case(Method::TRACE, true)]
    fn only_the_safe_methods_skip_the_csrf_check(#[case] method: Method, #[case] expected: bool) {
        assert_eq!(changes_state(&method), expected, "{method}");
    }

    /// The cookie's attributes are the security properties, so they are asserted
    /// rather than assumed.
    #[test]
    fn a_session_cookie_carries_the_attributes_that_make_it_one() {
        let cookie = session_cookie("abc", &config(), false);

        assert!(cookie.starts_with("rs_session=abc;"), "{cookie}");
        assert!(cookie.contains("HttpOnly"), "{cookie}");
        assert!(cookie.contains("SameSite=Strict"), "{cookie}");
        assert!(cookie.contains("Path=/"), "{cookie}");
        assert!(cookie.contains("Max-Age=1000"), "{cookie}");
    }

    /// `Secure` is the one attribute that is conditional, so it gets its own
    /// assertion at both settings.
    #[rstest]
    #[case(true, true)]
    #[case(false, false)]
    fn the_secure_attribute_follows_the_transport(#[case] over_tls: bool, #[case] present: bool) {
        let cookie = session_cookie("abc", &config(), over_tls);

        assert_eq!(cookie.contains("Secure"), present, "{cookie}");
        // ...and the rest of the attributes survive either way.
        assert!(cookie.contains("HttpOnly"), "{cookie}");
        assert!(cookie.contains("SameSite=Strict"), "{cookie}");
    }

    /// Only a proxy the operator named is believed about the protocol, and only
    /// when it says `https`.
    #[rstest]
    #[case(true, Some("https"), true)]
    // A scheme is case-insensitive, and proxies are not consistent about it.
    #[case(true, Some("HTTPS"), true)]
    #[case(true, Some(" https "), true)]
    #[case(true, Some("http"), false)]
    #[case(true, Some(""), false)]
    #[case(true, None, false)]
    // The same claim from a peer nobody vouched for buys nothing — this is the
    // shipped posture, where the trust list is empty.
    #[case(false, Some("https"), false)]
    #[case(false, None, false)]
    fn only_a_trusted_proxy_is_believed_about_the_protocol(
        #[case] trusted: bool,
        #[case] proto: Option<&str>,
        #[case] expected: bool,
    ) {
        let peer: IpAddr = "10.1.2.3".parse().expect("addr");
        let trusted: TrustedProxies = match trusted {
            true => std::iter::once("10.1.2.3".parse().expect("proxy")).collect(),
            false => TrustedProxies::default(),
        };
        let mut headers = HeaderMap::new();
        if let Some(proto) = proto {
            headers.insert(FORWARDED_PROTO, proto.parse().expect("header value"));
        }

        assert_eq!(
            arrived_over_tls(&headers, peer, &trusted),
            expected,
            "proto={proto:?}"
        );
    }

    /// A removal has to match the cookie it removes on name, path and flags, or
    /// the browser clears a different one — or none.
    #[test]
    fn a_cleared_cookie_matches_the_one_it_clears() {
        let cleared = cleared_session_cookie();

        assert!(cleared.starts_with("rs_session=;"), "{cleared}");
        assert!(cleared.contains("Path=/"), "{cleared}");
        assert!(cleared.contains("SameSite=Strict"), "{cleared}");
        assert!(cleared.contains("Max-Age=0"), "{cleared}");
    }

    #[test]
    fn a_token_is_64_hex_characters_and_never_repeats() {
        let tokens: std::collections::HashSet<String> =
            (0..1_000).map(|_| random_token()).collect();

        assert_eq!(tokens.len(), 1_000);
        for token in &tokens {
            assert_eq!(token.len(), 64, "{token}");
            assert!(token.chars().all(|c| c.is_ascii_hexdigit()), "{token}");
        }
    }

    /// The password is checked against a hash, and an unprovisioned surface
    /// refuses everything — including the empty string, which is what an unset
    /// environment variable would offer.
    #[rstest]
    #[case("hunter2", true)]
    #[case("hunter", false)]
    #[case("hunter22", false)]
    #[case("HUNTER2", false)]
    #[case("", false)]
    fn a_password_is_verified_against_its_hash(#[case] candidate: &str, #[case] expected: bool) {
        let auth = AdminAuth::new("hunter2", config());

        assert_eq!(auth.verify(candidate), expected, "{candidate:?}");
        assert!(
            !AdminAuth::locked().verify(candidate),
            "an unprovisioned surface authenticates nothing"
        );
    }

    /// ADR-0008's safety property, at the seam that decides it: a generated
    /// password that could not be saved leaves the admin surface **shut**, not
    /// open on a credential only the server ever saw. The other two outcomes
    /// gate on the password they carry.
    #[rstest]
    #[case(AdminPassword::Configured(String::from("hunter2")), true)]
    #[case(
        AdminPassword::Generated {
            password: String::from("hunter2"),
            env_file: std::path::PathBuf::from("/srv/.env"),
        },
        true
    )]
    #[case(
        AdminPassword::NotPersisted {
            env_file: std::path::PathBuf::from("/srv/.env"),
            error: std::io::Error::other("read-only"),
        },
        false
    )]
    fn a_password_that_could_not_be_saved_leaves_the_surface_shut(
        #[case] provisioned: AdminPassword,
        #[case] opens: bool,
    ) {
        let auth = AdminAuth::provisioned(&provisioned, config());

        assert_eq!(auth.verify("hunter2"), opens, "{provisioned:?}");
        // ...and a locked surface is locked against everything, not just the
        // password that would have worked.
        assert!(!auth.verify(""), "{provisioned:?}");
    }

    /// A stored hash is never the password, and two hashes of the same password
    /// differ — a shared salt would let one leaked hash be tested against every
    /// instance at once.
    #[test]
    fn hashing_is_salted_and_never_stores_the_password() {
        let (first, second) = (hash_password("hunter2"), hash_password("hunter2"));

        assert_ne!(first, second, "each hash gets its own salt");
        assert!(!first.contains("hunter2"), "{first}");
        assert!(first.starts_with("$argon2id$"), "{first}");
    }

    proptest! {
        /// Equality, decided the same way `==` would but without an early exit.
        #[test]
        fn constant_time_equality_agrees_with_plain_equality(
            a in "[ -~]{0,64}",
            b in "[ -~]{0,64}",
        ) {
            prop_assert_eq!(constant_time_eq(a.as_bytes(), b.as_bytes()), a == b);
            prop_assert!(constant_time_eq(a.as_bytes(), a.as_bytes()));
        }

        /// Whatever a client sends as a `Cookie`, reading it is total, never
        /// panics, and never yields the empty string as an id.
        #[test]
        fn any_cookie_header_parses_without_yielding_an_empty_id(
            cookies in "[ -~]{0,80}",
        ) {
            let mut headers = HeaderMap::new();
            if let Ok(value) = cookies.parse() {
                headers.insert(header::COOKIE, value);
            }
            if let Some(id) = session_id_of(&headers) {
                prop_assert!(!id.is_empty());
                prop_assert!(!id.contains(';'));
            }
        }

        /// However the attempts fall, an address inside its budget is never
        /// locked out and an address that has spent it always is.
        #[test]
        fn the_budget_alone_decides_whether_an_address_is_locked(attempts in 0u32..10) {
            let (config, now) = (config(), Instant::now());
            let mut lockout = Lockout::default();
            for _ in 0..attempts {
                lockout.record_failure(addr(7), now, &config);
            }
            prop_assert_eq!(
                lockout.locked_for(addr(7), now, &config).is_some(),
                attempts >= config.lockout_attempts,
                "after {} attempts", attempts
            );
        }
    }
}
