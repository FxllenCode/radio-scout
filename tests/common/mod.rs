//! The integration-test harness (ticket #21, ADR-0009) — the project's primary
//! test seam.
//!
//! [`TestApp`] brings up the **real** Axum router in-process on an ephemeral
//! port, against a fresh database and a filesystem blob store in a temp
//! directory it owns, and drives it over its actual HTTP + WebSocket boundary.
//! Nothing here reaches past a handler: an assertion is made about what an
//! operator could observe — a response body, a database row, a stored object, a
//! frame on the wire.
//!
//! ```ignore
//! let app = TestApp::with_key("k").await;
//! let mut ws = app.connect_ws().await;
//! subscribe(&mut ws, r#"{"t":"sub","all":true}"#).await;
//!
//! app.upload_ok(CallUpload::new()).await;
//!
//! assert_eq!(next_json(&mut ws).await["t"], "call");
//! assert_eq!(app.count::<call::Entity>().await, 1);
//! assert!(app.stored(&app.the_call().await.object_key).await);
//! ```
//!
//! The database is **SQLite by default and Postgres when the run was given
//! one** — set `TEST_POSTGRES_URL` and every app spawned here lands on a
//! `rs_test_<uuid>` database of its own, which is how the whole suite runs a
//! second time on the other dialect (#22, ADR-0003). No test says which; see
//! [`postgres_server`] and `docs/agents/dual-dialect.md`.
//!
//! The client carries a **cookie jar**, so the admin session (#19) behaves as it
//! does in a browser: [`TestApp::login`] once and every later `post_admin_*` on
//! that handle is authenticated, with the CSRF token remembered alongside the
//! cookie the way a page keeps it in memory. Every spawned app is gated by
//! [`ADMIN_PASSWORD`]; [`TestAppBuilder::admin`] takes an [`AdminAuth`] for the
//! tests that need a different policy — or none.
//!
//! **The handle owns its temp directory**, so a test never has to keep a `_tmp`
//! binding alive by hand — dropping the app deletes the database and the audio.
//! (It does *not* own the `axum::serve` task: that runs until the test binary
//! exits, exactly as it did before #21. Harmless — the process is about to go —
//! but "owns everything" would be a lie.) Non-default wiring goes through
//! [`TestApp::builder`]: an [`IngestConfig`], a
//! caller-supplied blob store (the S3 serving mode), or a caller-supplied
//! database URL.
//!
//! **Failure is wired in by default** (#97). Every spawned app issues its
//! statements through a handle this one can refuse — [`TestApp::refuse_statements_on`]
//! and [`TestApp::refuse_updates_to`], which is how a 5xx path or a worker's
//! error arm is reached without damaging a schema — and
//! [`faulty_store`] hands over an audio store that can be told to fail a write,
//! refuse a read, or answer one with "no such object" ([`faults_over_store`]
//! does the same over a store of your own, which is what failing a *presign*
//! needs). Both substitute at an interface Radio-Scout owns; see `faults.rs` for
//! why that is not the same as decorating what is underneath.
//!
//! Included via `mod common;` from each `tests/*.rs` binary. Every binary is its
//! own crate and recompiles this module whole while using a subset of it, so an
//! unused helper is the normal state here — hence the blanket `dead_code` allow
//! rather than an attribute on every item.
//!
//! The harness's own tests are `tests/harness.rs`.
#![allow(dead_code)]

mod audio;
mod faults;
pub mod logs;
mod push;
pub mod s3;
mod upload;
mod ws;

// Each binary uses a subset of these, so in most of them most are unused. The
// allow is scoped to the re-exports alone — the module's own `use` statements
// below stay checked.
#[allow(unused_imports)]
pub use audio::{silence_ms, wav};
#[allow(unused_imports)]
pub use faults::{Faults, INJECTED_IO, REFUSED, Statements, faults_over_store, faulty_store};
#[allow(unused_imports)]
pub use push::{PushService, Pushed, SUBSCRIBER_AUTH, SUBSCRIBER_PRIVATE, SUBSCRIBER_PUBLIC};
#[allow(unused_imports)]
pub use upload::CallUpload;
#[allow(unused_imports)]
pub use ws::{
    Drained, FILTER_BUDGET, Ws, drain_until, expect_server_closed, frame_within, next_json,
    next_text, no_frame_within, subscribe,
};

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use clap::Parser;
use radio_scout::admin::CSRF_HEADER;
use radio_scout::blob::{AudioStore, StoredAudio};
use radio_scout::config::{Cli, Config};
use radio_scout::db::Db;
use radio_scout::db::entities::{call, call_patch, system, tag, talkgroup, talkgroup_ref, unit};
use radio_scout::db::repo::{self, NewCall, NewLogEvent};
use radio_scout::enhance::EnhancementConfig;
use radio_scout::instance::{self, Credentials, Instance, Wiring};
use radio_scout::merge;
use radio_scout::{Clock, IngestConfig};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, Set,
};

/// A seeded Call's row pointing at `key` — what ingest would have left behind
/// after writing the object there.
///
/// The byte length is zero because nothing was written; a test that cares about
/// the size says so with [`TestApp::put_object`] and its own value.
pub fn audio_at(key: impl Into<String>) -> Option<StoredAudio> {
    Some(StoredAudio::written(key.into(), 0))
}

/// A running Radio-Scout, and everything needed to observe it.
///
/// Field order is drop order: `tmp` is declared last, so the temp directory
/// holding the database and the audio outlives every handle onto it.
pub struct TestApp {
    /// `host:port` the app is listening on.
    pub addr: String,
    /// The same handle the handlers use — for seeding rows and asserting on
    /// them, and for refusing a statement they are about to issue.
    pub db: Db,
    /// The same audio store the app writes to.
    pub store: Arc<dyn AudioStore>,
    /// Which statements this Instance's database is refusing (#97).
    statements: Statements,
    /// The running Instance itself (#90) — the same assembly the binary boots,
    /// and what [`TestApp::restart`] stops and starts.
    instance: Instance,
    /// The feeding half of this Instance's operator log sink, until a test asks
    /// for it with [`TestApp::store_logs`]. The Instance is already draining
    /// it; what a test adds is *this thread's* logging as a source.
    log_sink: std::sync::Mutex<Option<radio_scout::logsink::LogSink>>,
    client: reqwest::Client,
    /// The admin session [`TestApp::login`] opened, if any: the raw session
    /// cookie and the CSRF token bound to it. The client's jar carries the
    /// cookie too — this is the copy a test can still present after logout.
    session: std::sync::Mutex<Option<(String, String)>>,
    tmp: tempfile::TempDir,
}

impl TestApp {
    /// Bring up an app with everything defaulted: a fresh SQLite database and a
    /// filesystem blob store under a temp directory, production ingest config,
    /// production heartbeat.
    pub async fn spawn() -> Self {
        Self::builder().spawn().await
    }

    /// [`TestApp::spawn`] with a global (any-System) API key already registered —
    /// the precondition of nearly every ingest test.
    pub async fn with_key(key: &str) -> Self {
        let app = Self::spawn().await;
        app.create_api_key(key).await;
        app
    }

    /// Start describing a non-default app.
    pub fn builder() -> TestAppBuilder {
        TestAppBuilder::default()
    }

    /// The temp directory the app's database and (default) audio live under.
    pub fn path(&self) -> &Path {
        self.tmp.path()
    }

    /// This Instance's background Workers, by name (#93) — the sweeper, the
    /// push sender, the enhancement worker and the log sink.
    ///
    /// What a test waits on instead of sleeping. Most callers want
    /// [`TestApp::settle`]; reach for a named one when the wait is for a count
    /// rather than for quiet.
    pub fn workers(&self) -> &radio_scout::worker::Workers {
        self.instance.workers()
    }

    /// Wait until every Worker owes nothing.
    ///
    /// The single most useful line in the harness, and the one that replaced
    /// four bespoke pollers and two fixed sleeps. **This is what "and nothing
    /// happened" is made of**: a sleep can only ever say "not yet", and gets
    /// longer every time a loaded CI runner disagrees with it. Settling says
    /// the work was picked up, considered, and finished — so an assertion after
    /// it is about a decision the Instance actually made.
    pub async fn settle(&self) {
        self.workers().idle().await;
    }

    /// Stop this Instance and start it again on the same configuration, the
    /// same database and the same store (#90).
    ///
    /// This is what a real restart is: the workers stop, the port is released,
    /// and the next boot picks up whatever the last one left behind — a Call
    /// still marked pending, an archive to leave alone, an ingest key in the
    /// env file. It replaces the preamble that used to stand a second app up on
    /// a hand-shared database URL while the first one was still running.
    ///
    /// The port changes, because the harness asks for an ephemeral one; the
    /// handle's `addr` follows it. Server-side state does not survive, so a
    /// test that was logged in logs in again — which is also true of the thing
    /// being modelled.
    pub async fn restart(&mut self) {
        self.restart_onto(None, |_| {}).await;
    }

    /// [`TestApp::restart`], with the configuration the next boot will have —
    /// an operator who edited `radio-scout.toml` before restarting.
    pub async fn restart_with(&mut self, edit: impl FnOnce(&mut Config)) {
        self.restart_onto(None, edit).await;
    }

    /// [`TestApp::restart_with`], and onto a different store — an operator
    /// whose `[storage]` now points somewhere else, or whose disk filled up
    /// while the process was down.
    pub async fn restart_onto(
        &mut self,
        store: Option<Arc<dyn AudioStore>>,
        edit: impl FnOnce(&mut Config),
    ) {
        let mut config = self.instance.config().clone();
        edit(&mut config);
        self.instance
            .restart_with(config, store)
            .await
            .expect("restart");
        self.addr = loopback(&self.instance);
        self.db = self.instance.db.clone();
        self.store = self.instance.store.clone();
        *self.session.lock().expect("session") = None;
    }

    /// What this app's own env file pins `var` to — the only copy of a
    /// credential its boot generated, exactly as an operator would read it.
    pub fn env_var(&self, var: &str) -> String {
        env_value(&self.tmp.path().join(ENV_FILE), var)
    }

    /// An absolute URL for `path` on this app.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// The `ws://` URL of the live feed.
    fn ws_url(&self) -> String {
        format!("ws://{}/api/live", self.addr)
    }

    /// The shared HTTP client, for a request too exotic to have a helper —
    /// a captured recorder body with its own `User-Agent`, say. It follows
    /// redirects; [`TestApp::get_without_redirects`] builds its own that doesn't.
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    // -- HTTP ---------------------------------------------------------------

    /// `GET path`, following redirects.
    pub async fn get(&self, path: &str) -> reqwest::Response {
        self.client.get(self.url(path)).send().await.expect("GET")
    }

    /// `GET path`, expecting 200 and a JSON body. The status is checked on the
    /// way past, so a 500 fails as a 500 rather than as "not an object".
    pub async fn get_json(&self, path: &str) -> serde_json::Value {
        let resp = self.get(path).await;
        assert_eq!(resp.status(), 200, "GET {path}");
        resp.json().await.expect("json body")
    }

    /// `GET path` with a `Range` header — the audio contract's other half
    /// (ADR-0002: iOS `<audio>` will not play without it).
    pub async fn get_range(&self, path: &str, range: &str) -> reqwest::Response {
        self.client
            .get(self.url(path))
            .header(reqwest::header::RANGE, range)
            .send()
            .await
            .expect("ranged GET")
    }

    /// `GET path` *without* following redirects, so a test can observe the 307
    /// itself — the S3 presigned-URL serving mode.
    pub async fn get_without_redirects(&self, path: &str) -> reqwest::Response {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client")
            .get(self.url(path))
            .send()
            .await
            .expect("GET")
    }

    /// POST a multipart form to `path`.
    async fn post_multipart(
        &self,
        path: &str,
        form: reqwest::multipart::Form,
    ) -> reqwest::Response {
        self.client
            .post(self.url(path))
            .multipart(form)
            .send()
            .await
            .expect("multipart POST")
    }

    /// POST a JSON body to `path`, keeping the whole response — the admin
    /// surface (#19) speaks JSON, and its interesting half is in the headers.
    pub async fn post_json(&self, path: &str, body: serde_json::Value) -> reqwest::Response {
        self.client
            .post(self.url(path))
            .json(&body)
            .send()
            .await
            .expect("JSON POST")
    }

    // -- The admin surface (#19) ---------------------------------------------

    /// Log in with the default [`ADMIN_PASSWORD`], returning the CSRF token the
    /// session is bound to — and remembering both, the way a browser holds the
    /// cookie in its jar and the token in the page. Every later
    /// `post_admin_*` on this handle is then authenticated.
    pub async fn login(&self) -> String {
        let response = self.login_as(ADMIN_PASSWORD).await;
        assert_eq!(response.status(), 200, "login failed");
        let cookie = header_of(&response, "set-cookie")
            .expect("a session cookie")
            .split(';')
            .next()
            .expect("a name=value pair")
            .to_string();
        let csrf = response
            .json::<serde_json::Value>()
            .await
            .expect("a session body")["csrf_token"]
            .as_str()
            .expect("a csrf token")
            .to_string();
        *self.session.lock().expect("session") = Some((cookie, csrf.clone()));
        csrf
    }

    /// The `name=value` pair of the session [`TestApp::login`] opened, ready to
    /// replay as a `Cookie` header.
    ///
    /// The client's jar already carries it — this is for the tests that must
    /// keep presenting a session *after* the browser has been told to drop it,
    /// which is how server-side revocation is told apart from the client merely
    /// losing the cookie.
    pub fn session_cookie(&self) -> String {
        self.session
            .lock()
            .expect("session")
            .clone()
            .expect("log in first")
            .0
    }

    /// Attempt a login with an arbitrary password, keeping the whole response —
    /// for the rejections (401, and the 429 the lockout answers with).
    pub async fn login_as(&self, password: &str) -> reqwest::Response {
        self.login_request(password).send().await.expect("login")
    }

    /// A login the caller adds headers to before sending: the proxy claims
    /// (`X-Forwarded-For`, `X-Forwarded-Proto`) that decide which address the
    /// lockout charges and whether the cookie is marked `Secure`.
    pub fn login_request(&self, password: &str) -> reqwest::RequestBuilder {
        self.client
            .post(self.url("/api/admin/login"))
            .json(&serde_json::json!({ "password": password }))
    }

    /// The CSRF token of the session [`TestApp::login`] opened.
    fn csrf(&self) -> String {
        self.session
            .lock()
            .expect("session")
            .clone()
            .expect("log in before posting to the admin surface")
            .1
    }

    /// POST to the admin surface as an authenticated client: the session from
    /// the cookie jar, and the CSRF token from the login that opened it.
    pub async fn post_admin_bytes(
        &self,
        path: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> (u16, String) {
        read(
            self.admin_request(path, Some(&self.csrf()))
                .header(reqwest::header::CONTENT_TYPE, content_type)
                .body(body)
                .send()
                .await
                .expect("admin POST"),
        )
        .await
    }

    /// POST to the admin surface with no body — logout, and anything else whose
    /// news is entirely in the status and the headers.
    pub async fn post_admin(&self, path: &str) -> reqwest::Response {
        self.admin_request(path, Some(&self.csrf()))
            .send()
            .await
            .expect("admin POST")
    }

    /// A POST to the admin surface carrying the CSRF token of the caller's
    /// choosing — a forged one, or none at all. For the tests *about* the CSRF
    /// check; everything else uses [`TestApp::post_admin_bytes`].
    pub fn admin_request(&self, path: &str, csrf: Option<&str>) -> reqwest::RequestBuilder {
        let request = self.client.post(self.url(path));
        match csrf {
            Some(csrf) => request.header(CSRF_HEADER, csrf),
            None => request,
        }
    }

    /// POST a raw body with a `Content-Type` of the caller's choosing — a
    /// recorder that is misconfigured (or mid-crash), or a captured payload the
    /// golden suite replays byte-for-byte.
    pub async fn post_bytes(&self, path: &str, content_type: &str, body: Vec<u8>) -> (u16, String) {
        let resp = self
            .client
            .post(self.url(path))
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body)
            .send()
            .await
            .expect("raw POST");
        read(resp).await
    }

    // -- Synthetic Calls ----------------------------------------------------

    /// `GET path` as a reverse proxy would relay it, claiming in
    /// `X-Forwarded-For` to be speaking for `client` — [`TestApp::upload_via_proxy`]
    /// for the read surfaces, where the question is what a *listener's* address
    /// does to the log (ADR-0011 rule 5) rather than a recorder's.
    pub async fn get_via_proxy(&self, path: &str, client: &str) -> reqwest::Response {
        self.client
            .get(self.url(path))
            .header(FORWARDED_FOR, client)
            .send()
            .await
            .expect("GET via proxy")
    }

    /// POST a synthetic Call as a reverse proxy would relay it, claiming in
    /// `X-Forwarded-For` to be speaking for `client`. Whether that claim is
    /// believed is `trusted_proxies`' answer (#17).
    pub async fn upload_via_proxy(&self, call: CallUpload, client: &str) -> (u16, String) {
        let resp = self
            .client
            .post(self.url("/api/call-upload"))
            .header(FORWARDED_FOR, client)
            .multipart(call.into_form())
            .send()
            .await
            .expect("multipart POST");
        read(resp).await
    }

    /// POST a synthetic Call to the generic rdio endpoint, returning the status
    /// and body a recorder would see.
    pub async fn upload(&self, call: CallUpload) -> (u16, String) {
        read(self.upload_response(call).await).await
    }

    /// [`TestApp::upload`], keeping the whole response — for the headers
    /// (`x-request-id`) a status-and-body pair drops.
    pub async fn upload_response(&self, call: CallUpload) -> reqwest::Response {
        self.post_multipart("/api/call-upload", call.into_form())
            .await
    }

    /// POST a synthetic Call and assert the recorder was told it landed.
    ///
    /// "Told it landed" is not the same as "became a row": a blacklisted or
    /// not-populated Call is deliberately dropped *and* answered 200, so the
    /// recorder never retries (#8). A test that means the row exists asserts on
    /// the row.
    pub async fn upload_ok(&self, call: CallUpload) {
        let (status, body) = self.upload(call).await;
        assert_eq!(status, 200, "{body:?}");
        assert!(
            body.contains("Call imported successfully."),
            "recorders match on this exact string; got {body:?}"
        );
    }

    /// POST to the Trunk-Recorder-native endpoint (#6). Build the upload with
    /// [`CallUpload::tr`].
    pub async fn upload_tr(&self, call: CallUpload) -> (u16, String) {
        read(
            self.post_multipart("/api/trunk-recorder-call-upload", call.into_form())
                .await,
        )
        .await
    }

    // -- Database -----------------------------------------------------------

    /// Register a global (any-System) ingest API key.
    pub async fn create_api_key(&self, key: &str) {
        repo::create_api_key(&self.db, key, None, None, 0)
            .await
            .expect("create api key");
    }

    /// Register an API key scoped to one System (ADR-0008).
    pub async fn create_api_key_for_system(&self, key: &str, system_ref: i64) {
        repo::create_api_key(&self.db, key, Some(system_ref), None, 0)
            .await
            .expect("create api key");
    }

    /// How many rows an entity has — `app.count::<call_patch::Entity>().await`.
    pub async fn count<E>(&self) -> u64
    where
        E: EntityTrait,
        E::Model: Send + Sync,
    {
        E::find().count(&self.db).await.expect("count rows")
    }

    /// Every stored Call, oldest first.
    pub async fn calls(&self) -> Vec<call::Model> {
        call::Entity::find()
            .order_by_asc(call::Column::CallAtMs)
            .order_by_asc(call::Column::Id)
            .all(&self.db)
            .await
            .expect("read calls")
    }

    /// The one stored Call. Fails if there isn't exactly one, because a test
    /// that meant "the first of several" should say so.
    pub async fn the_call(&self) -> call::Model {
        let calls = self.calls().await;
        assert_eq!(calls.len(), 1, "expected exactly one Call, got {calls:#?}");
        calls.into_iter().next().expect("one call")
    }

    /// Wait for a Call to leave the `pending` enhancement state, and hand back
    /// the row as it ended up.
    ///
    /// Enhancement is deliberately asynchronous — the whole point of #20 is
    /// that ingest answers before any of it starts — so this is a wait. Since
    /// #93 it is a wait on the worker's own idle signal rather than a poll of
    /// the row: the Call is owed from the moment ingest offered it, so settling
    /// means it has been enhanced, skipped, or refused, and the row can be read
    /// once and believed.
    pub async fn await_enhancement(&self, id: i64) -> call::Model {
        self.settle().await;
        let call = call::Entity::find_by_id(id)
            .one(&self.db)
            .await
            .expect("read call")
            .expect("the Call still exists");
        assert_ne!(
            call.enhancement,
            radio_scout::db::entities::call::EnhancementState::PENDING,
            "Call {id} is still pending with the enhancement worker owing nothing"
        );
        call
    }

    /// The System a Call belongs to.
    pub async fn system_of(&self, call: &call::Model) -> system::Model {
        system::Entity::find_by_id(call.system_id)
            .one(&self.db)
            .await
            .expect("read system")
            .expect("a Call's System row exists")
    }

    /// The Talkgroup a Call belongs to.
    pub async fn talkgroup_of(&self, call: &call::Model) -> talkgroup::Model {
        talkgroup::Entity::find_by_id(call.talkgroup_id)
            .one(&self.db)
            .await
            .expect("read talkgroup")
            .expect("a Call's Talkgroup row exists")
    }

    /// The Tag a Talkgroup carries. Fails if it has none — auto-populate always
    /// assigns one (`Untagged` by default), so a missing Tag is a bug, not a
    /// case to branch on.
    pub async fn tag_of(&self, talkgroup: &talkgroup::Model) -> tag::Model {
        tag::Entity::find_by_id(talkgroup.tag_id.expect("talkgroup has a tag"))
            .one(&self.db)
            .await
            .expect("read tag")
            .expect("a Talkgroup's Tag row exists")
    }

    /// Insert a Call row directly, for the read surfaces (archive search, audio
    /// serving) that care about rows rather than how they got there. Returns the
    /// new Call's id. No audio object is *written* — say [`audio_at`] for a row
    /// that points at one, and pair it with [`TestApp::put_object`] when the
    /// bytes matter too.
    ///
    /// `audio` is a second argument for the same reason ingest passes one (#96):
    /// what a Recorder said and what our store holds are two different sets of
    /// facts, and only one of them can be known before the object is written.
    /// `None` is an **Encrypted Call** — a row with nothing behind it (#42).
    ///
    /// Always auto-populates, matching the shipped default. That flag gates only
    /// the Unit roster, so it is inert for a `NewCall` with no `units`; a test
    /// that needs it off should say why and take the knob then.
    pub async fn seed_call(&self, new: NewCall, audio: Option<StoredAudio>) -> i64 {
        repo::insert_call(&self.db, &new, audio, &repo::Resolved::default(), true, 0)
            .await
            .expect("seed call")
            .id
    }

    /// **Emit** a Call that is already stored: give it its place in the emission
    /// sequence and push it to everything that follows the live-feed fanout
    /// (#94).
    ///
    /// The application's own `AppState::publish`, which is what ingest calls a
    /// breath after the insert and what a **Delay** (#73) will call whenever its
    /// policy releases a Call. It is here because nothing over the wire can
    /// produce the case a **Backfill** ordered by emission exists for — a Call
    /// stored early and emitted late — since ingest does both in one breath.
    ///
    /// Pair it with [`TestApp::seed_call`], which stores without emitting.
    pub async fn emit(&self, call_id: i64) {
        let row = call::Entity::find_by_id(call_id)
            .one(&self.db)
            .await
            .expect("read the Call")
            .expect("a Call to emit");
        let view = repo::stored_calls(&self.db, std::slice::from_ref(&row))
            .await
            .expect("denormalize the Call")
            .pop()
            .expect("one row in, one view out");
        self.instance.state.publish(std::sync::Arc::new(view)).await;
    }

    // -- The operator log surface (#30) --------------------------------------

    /// Store a log event directly, for the tests about the *read* surface —
    /// filters, ordering, paging — which care about rows rather than how they
    /// were logged.
    pub async fn seed_log(&self, event: NewLogEvent) {
        repo::insert_log_events(&self.db, std::slice::from_ref(&event))
            .await
            .expect("seed log event");
    }

    /// Send this thread's logging into this app's database as well as the
    /// console, the way a running instance does.
    ///
    /// Hold the returned guard for the life of the test: dropping it puts the
    /// thread's subscriber back. Events land through the real `tracing` layer,
    /// so what a test reads back is what an operator would.
    pub fn store_logs(&self) -> logs::LogCapture {
        logs::LogCapture::with_sink(self.log_sink.lock().expect("log sink").take().expect(
            "this app has no sink to feed — it has already been captured, \
                 or `[log] database_level` is off",
        ))
    }

    /// Wait until the Logs view has an event whose message is `needle`, and
    /// hand back the page it appeared on (logged in as admin on the way past).
    ///
    /// The sink is deliberately asynchronous — a log call must never wait on a
    /// database — so this is a wait. Since #93 it waits on the sink Worker's
    /// idle signal: an event is owed from the moment it was offered, so a
    /// settled sink has written everything anything had said by then.
    ///
    /// [`TestApp::settle`] rather than the sink alone, because the line being
    /// waited for is often written *by* another Worker — the sweeper's report,
    /// the enhancement worker's refusal — and a sink with nothing left to write
    /// says nothing about whether the thing that would write it has run.
    pub async fn await_logged(&self, needle: &str) -> serde_json::Value {
        if self.session.lock().expect("session").is_none() {
            self.login().await;
        }
        self.settle().await;
        let page = self.get_json("/api/admin/logs?limit=500").await;
        let found = page["results"]
            .as_array()
            .expect("a results array")
            .iter()
            .any(|event| event["message"] == needle);
        assert!(
            found,
            "no stored log event said {needle:?} once every Worker had settled; \
             the page holds {page:#}"
        );
        page
    }

    /// Insert a System row with an explicit ingest policy. No admin surface sets
    /// `auto_populate` or `blacklist` yet — per-System policy is a database row,
    /// so that is #19 — so tests that exercise them seed the row.
    pub async fn seed_system(&self, system_ref: i64, auto_populate: bool, blacklist: Option<&str>) {
        system::ActiveModel {
            r#ref: Set(system_ref),
            label: Set(Some(format!("sys{system_ref}"))),
            auto_populate: Set(auto_populate),
            blacklist: Set(blacklist.map(str::to_string)),
            created_at_ms: Set(0),
            ..Default::default()
        }
        .insert(&self.db)
        .await
        .expect("seed system");
    }

    /// Insert a Talkgroup row under a System, so a test can say "this System
    /// already knows this Ref" without minting a Call to teach it.
    ///
    /// Patch membership is the case that needs this (#81): a patch ref is kept
    /// only when the System has a Talkgroup for it, so a test about patches has
    /// to establish what the System knows *before* the patched Call arrives —
    /// and teaching it with an extra upload would leave an extra Call in every
    /// row assertion.
    ///
    /// The System is created if absent, carrying the same `System <ref>` label
    /// ingest itself would default to — seeding must not change what the Call
    /// under test would otherwise have produced. A label is only applied on
    /// create, so a test whose recorder *names* its System (`systemLabel`, or
    /// native Trunk Recorder's `short_name`) has to create that System with its
    /// label first, and this will then find it.
    pub async fn seed_talkgroup(&self, system_ref: i64, talkgroup_ref: i64) {
        let system = repo::resolve_or_create_system(
            &self.db,
            system_ref,
            Some(format!("System {system_ref}")),
            0,
        )
        .await
        .expect("seed talkgroup's system");
        talkgroup::ActiveModel {
            system_id: Set(system.id),
            r#ref: Set(talkgroup_ref),
            created_at_ms: Set(0),
            ..Default::default()
        }
        .insert(&self.db)
        .await
        .expect("seed talkgroup");
    }

    /// Give an existing Talkgroup another Ref to answer to (#45).
    ///
    /// The Talkgroup must already be there — this is a *member* Ref, and one
    /// with no owner is not a thing the schema can express. Seeded directly
    /// rather than through the CSV importer even though that is the operator's
    /// path, because a test about *resolution* should not fail when the importer
    /// does; the importer has its own tests, in `tests/import.rs`.
    pub async fn seed_member_ref(&self, system_ref: i64, primary_ref: i64, member_ref: i64) {
        let talkgroup = self
            .talkgroup_by_ref(system_ref, primary_ref)
            .await
            .expect("the Talkgroup a member Ref is seeded onto exists");
        talkgroup_ref::ActiveModel {
            talkgroup_id: Set(talkgroup.id),
            system_id: Set(talkgroup.system_id),
            r#ref: Set(member_ref),
            position: Set(0),
            created_at_ms: Set(0),
            ..Default::default()
        }
        .insert(&self.db)
        .await
        .expect("seed member ref");
    }

    /// Insert a Unit row under a System, so a test can own a Ref without minting
    /// a Call to roster it. Creates the System if absent, as
    /// [`TestApp::seed_talkgroup`] does.
    pub async fn seed_unit(&self, system_ref: i64, unit_ref: i64, label: &str) {
        let system = repo::resolve_or_create_system(
            &self.db,
            system_ref,
            Some(format!("System {system_ref}")),
            0,
        )
        .await
        .expect("seed unit's system");
        unit::ActiveModel {
            system_id: Set(system.id),
            r#ref: Set(unit_ref),
            label: Set(Some(label.to_string())),
            created_at_ms: Set(0),
            ..Default::default()
        }
        .insert(&self.db)
        .await
        .expect("seed unit");
    }

    /// Give an existing Unit a Range of Refs to answer to — a fleet's numbered
    /// block (#45, CONTEXT.md). Both ends inclusive.
    pub async fn seed_unit_range(&self, system_ref: i64, primary_ref: i64, from: i64, to: i64) {
        let unit = self
            .unit_by_ref(system_ref, primary_ref)
            .await
            .expect("the Unit a Range is seeded onto exists");
        // Through the production writer, not around it: a Range seeded past the
        // overlap check would let a test set up state an operator cannot, and
        // the first thing that would hide is the check itself.
        let added = repo::add_unit_range(&self.db, &unit, merge::Range::new(from, to), 0, 0)
            .await
            .expect("seed unit range");
        assert!(
            matches!(added, repo::RangeAdded::Added(_)),
            "the seeded Range overlaps one this Unit's System already owns: {added:?}"
        );
    }

    /// The Talkgroup row a System knows by that primary Ref, if any.
    pub async fn talkgroup_by_ref(
        &self,
        system_ref: i64,
        talkgroup_ref: i64,
    ) -> Option<talkgroup::Model> {
        let system = system::Entity::find()
            .filter(system::Column::Ref.eq(system_ref))
            .one(&self.db)
            .await
            .expect("read system")?;
        talkgroup::Entity::find()
            .filter(talkgroup::Column::SystemId.eq(system.id))
            .filter(talkgroup::Column::Ref.eq(talkgroup_ref))
            .one(&self.db)
            .await
            .expect("read talkgroup")
    }

    /// The Unit row a System knows by that primary Ref, if any.
    pub async fn unit_by_ref(&self, system_ref: i64, unit_ref: i64) -> Option<unit::Model> {
        let system = system::Entity::find()
            .filter(system::Column::Ref.eq(system_ref))
            .one(&self.db)
            .await
            .expect("read system")?;
        unit::Entity::find()
            .filter(unit::Column::SystemId.eq(system.id))
            .filter(unit::Column::Ref.eq(unit_ref))
            .one(&self.db)
            .await
            .expect("read unit")
    }

    /// The member Refs a Talkgroup answers to, in the operator's order (#45) —
    /// what a fold leaves behind and an unfold takes away.
    pub async fn member_refs(&self, system_ref: i64, primary_ref: i64) -> Vec<i64> {
        let Some(talkgroup) = self.talkgroup_by_ref(system_ref, primary_ref).await else {
            return Vec::new();
        };
        talkgroup_ref::Entity::find()
            .filter(talkgroup_ref::Column::TalkgroupId.eq(talkgroup.id))
            .order_by_asc(talkgroup_ref::Column::Position)
            .all(&self.db)
            .await
            .expect("read member refs")
            .into_iter()
            .map(|member| member.r#ref)
            .collect()
    }

    /// The Talkgroup Refs a Call is patched to, ascending — the `call_patches`
    /// rows that survived membership resolution (#81).
    pub async fn patch_refs(&self, call_id: i64) -> Vec<i64> {
        let mut refs: Vec<i64> = call_patch::Entity::find()
            .filter(call_patch::Column::CallId.eq(call_id))
            .all(&self.db)
            .await
            .expect("read call patches")
            .into_iter()
            .map(|patch| patch.talkgroup_ref)
            .collect();
        refs.sort_unstable();
        refs
    }

    /// Refuse every statement this app issues that names `table` (#97).
    ///
    /// How the 5xx paths (#29) are reached from outside a handler: every other
    /// failure mode is a 4xx by design. What it replaced was `DROP TABLE`, and
    /// the two differ in three ways that matter — the DDL was dialect-specific,
    /// it took the whole table away rather than one statement, and its failure
    /// could only be recognised by matching the driver's own wording for "no
    /// such table", one phrasing per dialect. A refusal here is
    /// [`REFUSED`] on both, and the rule is armed rather than executed, so the
    /// schema underneath is untouched and nothing has to be dropped in
    /// dependency order.
    ///
    /// It applies to this test's own queries too, because they go through the
    /// same handle — arrange first, then refuse.
    pub fn refuse_statements_on(&self, table: &str) {
        self.statements.refuse(table);
    }

    /// Refuse every statement naming `table` that **updates a row**, leaving
    /// reads and inserts working (#37).
    ///
    /// [`TestApp::refuse_statements_on`]'s companion, and the thing a dropped
    /// table could never do: every update arm worth reaching happens after a
    /// read *and* an insert of the same table have already succeeded — the
    /// enhancement worker updates a Call row it has just read, ingest marks a
    /// Call pending after inserting it — so taking the table away always broke
    /// the wrong statement, which is why #20 shipped those arms uncovered.
    pub fn refuse_updates_to(&self, table: &str) {
        self.statements.refuse_updates(table);
    }

    /// How many database statements this Instance has issued since it opened
    /// its database (#86) — the same seam, counting instead of refusing.
    ///
    /// What it is for is an N+1, which is invisible from outside: the answer is
    /// right, and only the number of round-trips behind it is wrong. Sample
    /// either side of the work and the difference is what that work cost; run
    /// it at two sizes and the difference between *those* is whether the cost
    /// grows per Call.
    ///
    /// It counts this test's own queries too, for the same reason
    /// [`TestApp::refuse_statements_on`] applies to them — one handle. So take
    /// the samples around the app's work and nothing else.
    pub fn statements_issued(&self) -> u64 {
        self.statements.issued()
    }

    // -- Stored objects -----------------------------------------------------

    /// Write an object directly to the store the app reads from.
    pub async fn put_object(&self, key: &str, bytes: &[u8]) {
        self.store
            .put(key, bytes::Bytes::copy_from_slice(bytes))
            .await
            .expect("put object");
    }

    /// Every object in the store, sorted — so an assertion can compare a whole
    /// store without depending on listing order.
    pub async fn object_keys(&self) -> Vec<String> {
        let mut keys = self.store.list_keys().await.expect("list objects");
        keys.sort();
        keys
    }

    /// Whether an object is still in the store — what retention and orphan-GC
    /// are actually judged on.
    pub async fn stored(&self, key: &str) -> bool {
        self.store.size(key).await.expect("stat object").is_some()
    }

    /// The bytes behind a key, or `None` if it isn't there.
    pub async fn object_bytes(&self, key: &str) -> Option<Vec<u8>> {
        self.store
            .get(key)
            .await
            .expect("read object")
            .map(|bytes| bytes.to_vec())
    }

    // -- Live feed ----------------------------------------------------------

    /// Open a live-feed WebSocket, consuming the `hello` greeting (#9) — the
    /// common case, leaving the socket ready for subscribe/call frames.
    pub async fn connect_ws(&self) -> Ws {
        self.connect_ws_with_hello().await.0
    }

    /// Open a live-feed WebSocket and hand back the parsed `hello` greeting with
    /// it, for a test that asserts about the greeting itself.
    pub async fn connect_ws_with_hello(&self) -> (Ws, serde_json::Value) {
        let mut ws = self.connect_ws_raw().await;
        let hello = serde_json::from_str(&next_text(&mut ws).await).expect("hello json");
        (ws, hello)
    }

    /// Open a live-feed WebSocket and read nothing — the raw socket, `hello`
    /// still queued.
    async fn connect_ws_raw(&self) -> Ws {
        let (ws, _) = tokio_tungstenite::connect_async(self.ws_url())
            .await
            .expect("ws connect");
        ws
    }
}

/// Describes an app that differs from the default in some way.
///
/// Since #90 the settings are **configuration** edits rather than subsystems
/// assembled by hand: the builder resolves a [`Config`] and hands it to
/// `instance::start`, which is the same call the binary makes. `.ingest()`,
/// `.enhancement()`, `.trusted_proxies()` and `.database_url()` survive as
/// one-line sugar over that `Config`, beside [`TestAppBuilder::config`] for
/// anything without sugar and [`TestAppBuilder::toml`] for the file itself.
///
/// Two knobs are not configuration and so are still knobs: the object store and
/// the clock. A third — the live-feed heartbeat — went away with #94: reaping is
/// a row in the live connection's own table now, so nothing has to shorten a
/// period from outside in order to watch one happen.
///
/// What went away: `.admin(AdminAuth)` and `.push(Push)`. Both handed the app a
/// finished subsystem, which is precisely how the old harness could be green
/// about an Instance that had never been provisioned. A shut admin surface and
/// a disabled Push are now *outcomes* — [`TestAppBuilder::without_credentials`]
/// and [`TestAppBuilder::without_push`].
#[derive(Default)]
pub struct TestAppBuilder {
    toml: Option<String>,
    edits: Vec<ConfigEdit>,
    store: Option<Arc<dyn AudioStore>>,
    clock: Option<Clock>,
    vapid: Option<String>,
    unwritable_env: bool,
}

/// The header a reverse proxy names the original client in — what
/// `[server] trusted_proxies` decides whether to believe (#17, #28).
pub const FORWARDED_FOR: &str = "x-forwarded-for";

/// The admin password every spawned app is gated by (#19), so any test can log
/// in without configuring one.
///
/// It is a *provisioned* password, not an injected `AdminAuth`: the harness
/// hands it to `instance::start` as `RADIO_SCOUT_ADMIN_PASSWORD` would, and the
/// Instance provisions the admin surface from it exactly as a boot does.
pub const ADMIN_PASSWORD: &str = "test-admin-password";

/// A value that is not a P-256 key, so provisioning leaves Web Push off — the
/// same outcome an operator gets from a typo'd `RADIO_SCOUT_VAPID_PRIVATE_KEY`.
const NOT_A_VAPID_KEY: &str = "not-a-key";

impl TestAppBuilder {
    /// Edit the configuration this app is started from — the general seam, for
    /// a setting with no sugar of its own.
    ///
    /// Applied after the harness's own baseline, so an edit always wins.
    pub fn config(mut self, edit: impl FnOnce(&mut Config) + 'static) -> Self {
        self.edits.push(Box::new(edit));
        self
    }

    /// Start from this `radio-scout.toml` — the file itself, resolved the way a
    /// boot resolves it, for a test about configuration rather than about a
    /// setting's effect.
    ///
    /// Everything not written in it is at its shipped default, including
    /// `[retention] days`; the harness still owns `base_dir` and the database
    /// URL, because a test cannot be given the operator's disk.
    pub fn toml(mut self, text: impl Into<String>) -> Self {
        self.toml = Some(text.into());
        self
    }

    /// Ingest configuration — the dedup window and the auto-populate toggle
    /// (#5, #8).
    pub fn ingest(self, ingest: IngestConfig) -> Self {
        self.config(move |config| config.ingest = ingest)
    }

    /// Read the time from here rather than from the machine's clock (#90).
    pub fn clock(mut self, clock: Clock) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Use this blob store instead of the one `[storage]` describes — an
    /// S3-backed store exercises the presigned-redirect serving path offline
    /// (SigV4 is computed locally), and [`faulty_store`] makes I/O fail.
    pub fn store(mut self, store: impl AudioStore + 'static) -> Self {
        self.store = Some(Arc::new(store));
        self
    }

    /// Believe `X-Forwarded-For` from these proxies — a comma-separated list of
    /// addresses or CIDR blocks, exactly as `[server] trusted_proxies` takes
    /// them. The default trusts nobody, which is what ships.
    pub fn trusted_proxies(self, proxies: &str) -> Self {
        let proxies: Vec<_> = proxies
            .split(',')
            .map(|entry| entry.trim().parse().expect("an address or CIDR block"))
            .collect();
        self.config(move |config| config.server.trusted_proxies = proxies)
    }

    /// Boot with no Web Push identity, the way an operator with a typo'd key
    /// does: notifications are off and the routes say so.
    pub fn without_push(mut self) -> Self {
        self.vapid = Some(NOT_A_VAPID_KEY.to_string());
        self
    }

    /// Boot with the admin surface shut, the way an operator whose env file
    /// cannot be written does: a password was generated, nobody can read it, so
    /// none is set and nothing authenticates.
    ///
    /// A provisioning *outcome* rather than an injected `AdminAuth` — the
    /// difference between proving the state is refusable and proving a boot can
    /// arrive at it. It takes Web Push with it, for the same reason: the two
    /// generated credentials share the file that cannot be written.
    pub fn without_credentials(mut self) -> Self {
        self.unwritable_env = true;
        self
    }

    /// Turn audio enhancement on (#20). The default is what ships — off — so
    /// every other test in the suite is untouched by this existing.
    pub fn enhancement(self, enhancement: EnhancementConfig) -> Self {
        self.config(move |config| config.enhancement = enhancement)
    }

    /// Use this database instead of the one this run would have chosen.
    ///
    /// A per-call-site override, for a test about *which* database an app used
    /// — two apps sharing one is how a restart used to be written, which
    /// [`TestApp::restart`] now does properly. The dual-dialect run ADR-0009
    /// asks for does not go through here: it is [`postgres_server`], which
    /// moves the whole suite at once and gives every spawned app a database of
    /// its own.
    pub fn database_url(self, url: impl Into<String>) -> Self {
        let url = url.into();
        self.config(move |config| config.database.url = Some(url))
    }

    /// Bring the app up.
    pub async fn spawn(self) -> TestApp {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut config = match &self.toml {
            Some(text) => radio_scout::config::resolve(
                &Cli::parse_from(["radio-scout"]),
                |_| None,
                Some(&radio_scout::config::ConfigFile::new(
                    tmp.path().join("radio-scout.toml"),
                    text.clone(),
                )),
            )
            .expect("a configuration the harness was given"),
            None => baseline_config(),
        };
        config.server.base_dir = tmp.path().to_path_buf();
        config.database.url = Some(match postgres_server() {
            Some(server) => create_test_database(&server).await,
            None => format!("sqlite://{}?mode=rwc", tmp.path().join("t.db").display()),
        });
        for edit in self.edits {
            edit(&mut config);
        }

        // The sink is made before the subscriber and drained by the Instance,
        // exactly as `main.rs` does it — so a test that calls `store_logs`
        // reads back what the *running Instance* stored, not a second sink
        // beside it.
        // `None` when `[log] database_level` is `off`, which a test is entitled
        // to configure — the sink is a setting like any other, and the harness
        // overriding configuration must not mean the harness refusing it.
        let (sink, log_writer) = match radio_scout::logsink::channel(config.log.database_level) {
            Some((sink, writer)) => (Some(sink), Some(writer)),
            None => (None, None),
        };

        // A directory is not a writable env file, so a generated credential
        // cannot be saved and is therefore never put into service.
        let env_file = match self.unwritable_env {
            true => tmp.path().join("env-is-a-directory"),
            false => tmp.path().join(ENV_FILE),
        };
        if self.unwritable_env {
            std::fs::create_dir(&env_file).expect("an unwritable env path");
        }
        // Every spawned app can be told to refuse a statement (#97), whether or
        // not it ever is: the Instance still opens and migrates its own
        // database exactly as a boot does, and this only composes a decorator
        // around the handle it opened.
        let (decorate, statements) = faults::refusals();
        let mut wiring = Wiring::default()
            .decorate_db(decorate)
            .credentials(Credentials {
                env_file: Some(env_file),
                // Configured, so `login()` knows it; the other two are
                // generated into this app's own env file, which is what makes a
                // spawned Instance genuinely provisioned.
                admin_password: (!self.unwritable_env).then(|| ADMIN_PASSWORD.to_string()),
                vapid_key: self.vapid,
                ingest_key: None,
            })
            // ...so an app with the sink off simply has no writer to drain.
            .maybe_log_writer(log_writer)
            // Loopback, not the binary's `0.0.0.0`: a suite that opened a port
            // on every interface would be a firewall prompt per test binary,
            // and there is nothing to prove by listening where nobody calls.
            .bind(LOOPBACK);
        if let Some(store) = self.store {
            wiring = wiring.store(store);
        }
        if let Some(clock) = self.clock {
            wiring = wiring.clock(clock);
        }

        let instance = instance::start(config, wiring).await.expect("start");
        TestApp {
            addr: loopback(&instance),
            db: instance.db.clone(),
            store: instance.store.clone(),
            statements,
            instance,
            log_sink: std::sync::Mutex::new(sink),
            // With a cookie jar, so the client behaves like the browser the
            // admin session (#19) is designed around: log in once and every
            // later request on this handle carries the session.
            client: reqwest::Client::builder()
                .cookie_store(true)
                .build()
                .expect("client"),
            session: std::sync::Mutex::new(None),
            tmp,
        }
    }
}

/// One caller-supplied change to the configuration an app is started from.
type ConfigEdit = Box<dyn FnOnce(&mut Config)>;

/// The env file an Instance writes its generated credentials into.
pub const ENV_FILE: &str = ".env";

/// What an env file pins `var` to — the operator's only copy of a generated
/// credential, and so the only way a test can present one.
pub fn env_value(env_file: &Path, var: &str) -> String {
    std::fs::read_to_string(env_file)
        .expect("an env file")
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{var}=")))
        .unwrap_or_else(|| panic!("{var} was never written to {}", env_file.display()))
        .to_string()
}

/// Where a spawned app listens: an ephemeral loopback port.
const LOOPBACK: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 0);

/// `host:port` for a running Instance, as a test addresses it.
fn loopback(instance: &Instance) -> String {
    format!("127.0.0.1:{}", instance.addr.port())
}

/// The configuration a spawned app starts from: everything shipped, except that
/// **Retention keeps everything** — both windows.
///
/// A spawned Instance runs the sweeper for real (#90), and the suite dates its
/// fixtures by hand so that assertions about ordering and filtering are
/// deterministic: Calls in 1970, log events in 2020. The shipped windows —
/// seven days of audio, thirty of logs — would prune those, and the boot sweep
/// is a background task, so *when* it got to them would decide whether a test
/// passed. Turning the policy off rather than the sweeper keeps what runs
/// identical to what ships; a test that is about pruning sets a window itself.
fn baseline_config() -> Config {
    let mut config = Config::default();
    config.retention.days = 0;
    config.retention.log_days = 0;
    config
}

/// The Postgres server this run was handed, or `None` for the SQLite default.
///
/// `TEST_POSTGRES_URL` is the whole switch for the dual-dialect run (#22): CI
/// stands one server up for the job and sets it, and every `TestApp::spawn` in
/// every binary then lands on Postgres. Unset — the everyday loop, and any
/// machine without Docker — is SQLite, so nothing about local TDD changes.
pub fn postgres_server() -> Option<String> {
    std::env::var("TEST_POSTGRES_URL").ok()
}

/// Create a database of this test's own on `server`, returning its URL.
///
/// Isolation is a property the SQLite default gets for free from having a file
/// each; Postgres buys it here, because one shared database would put every
/// concurrently running test's rows in the same tables. The name is a v4 UUID,
/// so it is unique across the *processes* nextest runs tests in, not just within
/// one.
///
/// The database is deliberately **not** dropped afterwards: `Drop` cannot await,
/// and the server these run against is a throwaway — CI's dies with the job, and
/// `docs/agents/dual-dialect.md` says to `docker rm -f` the local one.
pub async fn create_test_database(server: &str) -> String {
    let name = format!("rs_test_{}", uuid::Uuid::new_v4().simple());
    let admin = sea_orm::Database::connect(server)
        .await
        .expect("connect to the TEST_POSTGRES_URL server");
    admin
        .execute_unprepared(&format!(r#"CREATE DATABASE "{name}""#))
        .await
        .expect("create this test's database");
    admin.close().await.expect("close the admin connection");
    database_url_in(server, &name)
}

/// The same Postgres server, addressing the database called `name`.
///
/// Only the database name is replaced: the credentials before it and the
/// connection parameters after it (`sslmode`, `options`) are the caller's, and a
/// rewrite that ate either would quietly connect somewhere else.
pub fn database_url_in(server: &str, name: &str) -> String {
    let (base, query) = match server.split_once('?') {
        Some((base, query)) => (base, Some(query)),
        None => (server, None),
    };
    // The first `/` *after* the scheme separator starts the path; before it lies
    // `user:password@host:port`, whose own `//` must not be mistaken for one.
    let authority = base.find("://").map_or(0, |i| i + 3);
    let root = &base[..base[authority..]
        .find('/')
        .map_or(base.len(), |i| authority + i)];
    match query {
        Some(query) => format!("{root}/{name}?{query}"),
        None => format!("{root}/{name}"),
    }
}

/// A response header as text, or `None` if it is absent (or not ASCII). Header
/// assertions are half of the audio and SPA contracts — cache-control, MIME,
/// content-range, content-disposition — so reading one is one call.
pub fn header_of<'a>(response: &'a reqwest::Response, name: &str) -> Option<&'a str> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
}

/// A response reduced to what a recorder sees: its status and its body.
async fn read(resp: reqwest::Response) -> (u16, String) {
    let status = resp.status().as_u16();
    (status, resp.text().await.unwrap_or_default())
}

/// The correlation id a response carries (#28's `x-request-id`, which is also
/// #29's 5xx ref). Every response has one, so its absence is a failure, not a
/// `None` for the caller to handle.
pub fn request_id_of(response: &reqwest::Response) -> String {
    response
        .headers()
        .get(radio_scout::http_log::REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .expect("every response carries an x-request-id")
        .to_owned()
}
