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
//! [`TestApp::builder`]: an [`IngestConfig`], a short live-feed heartbeat, a
//! caller-supplied blob store (the S3 serving mode), or a caller-supplied
//! database URL.
//!
//! Included via `mod common;` from each `tests/*.rs` binary. Every binary is its
//! own crate and recompiles this module whole while using a subset of it, so an
//! unused helper is the normal state here — hence the blanket `dead_code` allow
//! rather than an attribute on every item.
//!
//! The harness's own tests are `tests/harness.rs`.
#![allow(dead_code)]

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
pub use faults::{Faults, INJECTED_IO, faulty_store};
#[allow(unused_imports)]
pub use push::{
    PushService, Pushed, SUBSCRIBER_AUTH, SUBSCRIBER_PRIVATE, SUBSCRIBER_PUBLIC, VAPID_PRIVATE_KEY,
    VAPID_PUBLIC_KEY,
};
#[allow(unused_imports)]
pub use upload::CallUpload;
#[allow(unused_imports)]
pub use ws::{
    Drained, FILTER_BUDGET, Ws, drain_until, expect_ping, expect_server_closed, frame_within,
    next_json, next_text, no_frame_within, subscribe,
};

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use radio_scout::admin::{AdminAuth, AdminConfig, CSRF_HEADER};
use radio_scout::config::TrustedProxies;
use radio_scout::db::entities::{call, call_patch, system, tag, talkgroup};
use radio_scout::db::repo::{self, NewCall, NewLogEvent};
use radio_scout::db::{self};
use radio_scout::enhance::{EnhancementConfig, Enhancer};
use radio_scout::live::LiveFeed;
use radio_scout::push::{Push, PushConfig};
use radio_scout::webpush::VapidKey;
use radio_scout::{AppState, BlobStore, IngestConfig, build_app};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, Set,
};

/// A running Radio-Scout, and everything needed to observe it.
///
/// Field order is drop order: `tmp` is declared last, so the temp directory
/// holding the database and the audio outlives every handle onto it.
pub struct TestApp {
    /// `host:port` the app is listening on.
    pub addr: String,
    /// The same connection the handlers use — for seeding rows and asserting on
    /// them.
    pub db: DatabaseConnection,
    /// The same blob store the app writes audio to.
    pub store: Arc<BlobStore>,
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
    /// Polls, because enhancement is deliberately asynchronous: the whole point
    /// of #20 is that ingest answers before any of it starts, so there is no
    /// synchronous moment for a test to hook. Polling the *observable* state —
    /// the row — rather than reaching into the worker keeps this an assertion
    /// about what an operator could see.
    ///
    /// Fails rather than hanging: a worker that never finishes should be a
    /// named failure, not a test run that times out with no explanation.
    pub async fn await_enhancement(&self, id: i64) -> call::Model {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let call = call::Entity::find_by_id(id)
                .one(&self.db)
                .await
                .expect("read call")
                .expect("the Call still exists");
            if call.enhancement != radio_scout::db::entities::call::EnhancementState::PENDING {
                return call;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Call {id} was still pending enhancement after 20s"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
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
    /// new Call's id. No audio object is written — use
    /// [`TestApp::put_object`] when the bytes matter too.
    ///
    /// Always auto-populates, matching the shipped default. That flag gates only
    /// the Unit roster, so it is inert for a `NewCall` with no `units`; a test
    /// that needs it off should say why and take the knob then.
    pub async fn seed_call(&self, new: NewCall) -> i64 {
        repo::insert_call(&self.db, &new, true, 0)
            .await
            .expect("seed call")
            .id
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
        logs::LogCapture::storing(&self.db, radio_scout::logsink::LogSinkConfig::default())
    }

    /// Wait until the Logs view has an event whose message is `needle`, and
    /// hand back the page it appeared on (logged in as admin on the way past).
    ///
    /// Polling, because the sink is deliberately asynchronous: there is no
    /// completion for a test to await from the outside, which is the whole
    /// point of a log call that never waits on a database.
    pub async fn await_logged(&self, needle: &str) -> serde_json::Value {
        if self.session.lock().expect("session").is_none() {
            self.login().await;
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let page = self.get_json("/api/admin/logs?limit=500").await;
            let found = page["results"]
                .as_array()
                .expect("a results array")
                .iter()
                .any(|event| event["message"] == needle);
            if found {
                return page;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no stored log event said {needle:?} within 20s; the page holds {page:#}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
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

    /// Take a table out from under the running app, the way a half-applied
    /// migration or a hand-edited database does.
    ///
    /// This is the only way to reach the 5xx paths (#29) from outside a handler:
    /// every other failure mode is a 4xx by design. The DDL is **dialect-
    /// specific**, which is why it lives here rather than in five test files —
    /// Postgres refuses to drop a table that constraints still depend on, and
    /// SQLite has no `CASCADE` to offer it.
    pub async fn break_table(&self, table: &str) {
        let cascade = match self.db.get_database_backend() {
            db::DbBackend::Postgres => " CASCADE",
            _ => "",
        };
        self.db
            .execute_unprepared(&format!("DROP TABLE {table}{cascade}"))
            .await
            .unwrap_or_else(|err| panic!("break table {table}: {err}"));
    }

    /// Make every **update** to `table` fail, while reads and inserts go on
    /// working (#37).
    ///
    /// [`TestApp::break_table`]'s companion, and the thing it cannot do. A
    /// dropped table fails the *first* statement that touches it, but the
    /// failures worth reaching on the write side all happen after a read of the
    /// same table has already succeeded — the enhancement worker updates a Call
    /// row it has just read, ingest marks a Call pending after inserting it. So
    /// a dropped table always breaks the wrong statement, which is exactly why
    /// #20 shipped those arms uncovered.
    ///
    /// A trigger is how the failure is aimed at one statement kind. The DDL is
    /// **dialect-specific** — Postgres has no trigger body without a function to
    /// call, SQLite has no function to call — so it lives here beside
    /// `break_table` rather than in five test files. Both raise
    /// [`INJECTED_WRITE`], so an assertion about the cause reads the same on
    /// either dialect.
    pub async fn fail_writes_to(&self, table: &str) {
        let statements = match self.db.get_database_backend() {
            db::DbBackend::Postgres => vec![
                format!(
                    "CREATE OR REPLACE FUNCTION rs_fail_write() RETURNS trigger \
                     LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION '{INJECTED_WRITE}'; END $$"
                ),
                format!(
                    "CREATE TRIGGER rs_fail_write_{table} BEFORE UPDATE ON {table} \
                     FOR EACH ROW EXECUTE FUNCTION rs_fail_write()"
                ),
            ],
            _ => vec![format!(
                "CREATE TRIGGER rs_fail_write_{table} BEFORE UPDATE ON {table} \
                 BEGIN SELECT RAISE(ABORT, '{INJECTED_WRITE}'); END"
            )],
        };
        for statement in statements {
            self.db
                .execute_unprepared(&statement)
                .await
                .unwrap_or_else(|err| panic!("fail writes to {table}: {err}"));
        }
    }

    /// How **this** dialect words "that table isn't there" — SQLite says
    /// `no such table: calls`, Postgres says `relation "calls" does not exist`.
    ///
    /// The 5xx tests assert that the driver's own explanation reaches the
    /// operator's log and never the client's body ([`TestApp::break_table`] is
    /// how they get one). A test that pins a single wording keeps asserting that
    /// on one dialect and quietly asserts nothing on the other — precisely the
    /// half-blind green a dual-dialect run exists to catch.
    pub fn missing_table_cause(&self, table: &str) -> String {
        match self.db.get_database_backend() {
            db::DbBackend::Postgres => format!(r#"relation "{table}" does not exist"#),
            _ => format!("no such table: {table}"),
        }
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

/// Describes an app that differs from the default in some way. Every knob is
/// optional; [`TestAppBuilder::spawn`] fills in the rest.
#[derive(Default)]
pub struct TestAppBuilder {
    ingest: Option<IngestConfig>,
    heartbeat: Option<Duration>,
    store: Option<BlobStore>,
    database_url: Option<String>,
    trusted_proxies: Option<String>,
    admin: Option<AdminAuth>,
    push: Option<Push>,
    enhancement: Option<EnhancementConfig>,
}

/// What [`TestApp::fail_writes_to`] raises, on either dialect — so a test can
/// assert that the driver's own explanation of an injected failure reached the
/// operator's log, the way [`TestApp::missing_table_cause`] does for a table
/// that isn't there. Its opposite number for the store seam is
/// [`faults::INJECTED_IO`].
pub const INJECTED_WRITE: &str = "injected write failure";

/// The header a reverse proxy names the original client in — what
/// `[server] trusted_proxies` decides whether to believe (#17, #28).
pub const FORWARDED_FOR: &str = "x-forwarded-for";

/// The admin password every spawned app is gated by (#19), so any test can log
/// in without configuring one. A test *about* provisioning takes
/// [`TestAppBuilder::admin`] instead.
pub const ADMIN_PASSWORD: &str = "test-admin-password";

impl TestAppBuilder {
    /// Ingest configuration — the dedup window and the auto-populate toggle
    /// (#5, #8).
    pub fn ingest(mut self, ingest: IngestConfig) -> Self {
        self.ingest = Some(ingest);
        self
    }

    /// Live-feed heartbeat period. Drive it short so heartbeat and
    /// dead-connection reaping (#9) are observable without waiting the
    /// production 30 s.
    pub fn heartbeat(mut self, heartbeat: Duration) -> Self {
        self.heartbeat = Some(heartbeat);
        self
    }

    /// Use this blob store instead of a temp filesystem one — an S3-backed store
    /// exercises the presigned-redirect serving path offline (SigV4 is computed
    /// locally).
    pub fn store(mut self, store: BlobStore) -> Self {
        self.store = Some(store);
        self
    }

    /// Believe `X-Forwarded-For` from these proxies — a comma-separated list of
    /// addresses or CIDR blocks, exactly as `[server] trusted_proxies` takes
    /// them. The default trusts nobody, which is what ships.
    pub fn trusted_proxies(mut self, proxies: &str) -> Self {
        self.trusted_proxies = Some(proxies.to_string());
        self
    }

    /// Gate the admin surface with this rather than the default
    /// [`ADMIN_PASSWORD`] — a shorter session TTL, or
    /// [`AdminAuth::locked`] for an app nobody can get into.
    pub fn admin(mut self, admin: AdminAuth) -> Self {
        self.admin = Some(admin);
        self
    }

    /// Wire Web Push differently — a shorter coalescing window, or
    /// [`Push::disabled`] for a server with no VAPID identity at all. The
    /// default gives every app [`VAPID_PRIVATE_KEY`], so push is on and its
    /// public key is a constant.
    pub fn push(mut self, push: Push) -> Self {
        self.push = Some(push);
        self
    }

    /// Turn audio enhancement on (#20). The default is what ships — off — so
    /// every other test in the suite is untouched by this existing.
    pub fn enhancement(mut self, enhancement: EnhancementConfig) -> Self {
        self.enhancement = Some(enhancement);
        self
    }

    /// Use this database instead of the one this run would have chosen.
    ///
    /// A per-call-site override, for a test about *which* database an app used.
    /// The dual-dialect run ADR-0009 asks for does not go through here: it is
    /// [`postgres_server`], which moves the whole suite at once and gives every
    /// spawned app a database of its own.
    pub fn database_url(mut self, url: impl Into<String>) -> Self {
        self.database_url = Some(url.into());
        self
    }

    /// Bring the app up.
    pub async fn spawn(self) -> TestApp {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(
            self.store
                .unwrap_or_else(|| BlobStore::filesystem(tmp.path().join("audio")).expect("blob")),
        );
        let url = match self.database_url {
            Some(url) => url,
            None => match postgres_server() {
                Some(server) => create_test_database(&server).await,
                None => format!("sqlite://{}?mode=rwc", tmp.path().join("t.db").display()),
            },
        };
        let db = db::connect(&url).await.expect("db connect");

        let mut state = AppState::new(store.clone(), db.clone(), self.ingest.unwrap_or_default());
        if let Some(heartbeat) = self.heartbeat {
            state.live = LiveFeed::with_heartbeat(heartbeat);
        }
        if let Some(proxies) = self.trusted_proxies {
            state.trusted_proxies = trusted_proxies(&proxies);
        }
        state.admin = self
            .admin
            .unwrap_or_else(|| AdminAuth::new(ADMIN_PASSWORD, AdminConfig::default()));
        state.push = self.push.unwrap_or_else(|| {
            Push::new(
                VapidKey::parse(VAPID_PRIVATE_KEY).expect("the test VAPID key"),
                PushConfig::default(),
            )
        });

        state.enhancer = match self.enhancement {
            Some(config) => Enhancer::from_config(config),
            None => Enhancer::disabled(),
        };

        // The real sender, on the real fanout: a test asserts on the request
        // that leaves the process, not on a decision to make one.
        radio_scout::push::spawn(state.clone());
        // ...and the real worker, on the real queue, for the same reason.
        radio_scout::enhance::spawn(state.clone());

        TestApp {
            addr: serve(build_app(state)).await,
            db,
            store,
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

/// The trust list `[server] trusted_proxies = [...]` would have produced.
fn trusted_proxies(entries: &str) -> TrustedProxies {
    entries
        .split(',')
        .map(|entry| entry.trim().parse().expect("an address or CIDR block"))
        .collect()
}

/// Serve `app` on an ephemeral loopback port, returning its `host:port`.
///
/// With connect info, exactly as the binary serves it: the request log (#28)
/// reads the peer address from there, so a harness without it would make the
/// address field untestable.
async fn serve(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("serve");
    });
    format!("127.0.0.1:{}", addr.port())
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
