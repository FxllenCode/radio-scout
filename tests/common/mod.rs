//! The integration-test harness (ticket #21, ADR-0009) — the project's primary
//! test seam.
//!
//! [`TestApp`] brings up the **real** Axum router in-process on an ephemeral
//! port, against a fresh SQLite database and a filesystem blob store in a temp
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

pub mod logs;
mod upload;
mod ws;

// Each binary uses a subset of these, so in most of them most are unused. The
// allow is scoped to the re-exports alone — the module's own `use` statements
// below stay checked.
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

use radio_scout::config::TrustedProxies;
use radio_scout::db::entities::{call, system, tag, talkgroup};
use radio_scout::db::repo::{self, NewCall};
use radio_scout::db::{self};
use radio_scout::live::LiveFeed;
use radio_scout::{AppState, BlobStore, IngestConfig, build_app};
use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryOrder, Set};

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

    /// POST a synthetic Call as a reverse proxy would relay it, claiming in
    /// `X-Forwarded-For` to be speaking for `client`. Whether that claim is
    /// believed is `trusted_proxies`' answer (#17).
    pub async fn upload_via_proxy(&self, call: CallUpload, client: &str) -> (u16, String) {
        let resp = self
            .client
            .post(self.url("/api/call-upload"))
            .header("x-forwarded-for", client)
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
}

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

    /// Use this database instead of a fresh SQLite file in the temp directory.
    ///
    /// This is the *seam* for the dual-dialect run ADR-0009 asks for, not the
    /// run itself: it is per-call-site, and every `TestApp::spawn` in the suite
    /// still takes the SQLite default. Making the whole suite run on Postgres is
    /// #22's job and needs two more things this deliberately does not have —
    /// somewhere to read the container's URL from (`tests/db.rs` already reads
    /// `TEST_POSTGRES_URL` and skips when it is unset), and a **per-test
    /// database or schema**, because one shared Postgres would put every
    /// concurrent test's rows in the same tables. Isolation is a property the
    /// SQLite default gets for free from having a file each; Postgres will have
    /// to buy it.
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
        let url = self
            .database_url
            .unwrap_or_else(|| format!("sqlite://{}?mode=rwc", tmp.path().join("t.db").display()));
        let db = db::connect(&url).await.expect("db connect");

        let mut state = AppState::new(store.clone(), db.clone(), self.ingest.unwrap_or_default());
        if let Some(heartbeat) = self.heartbeat {
            state.live = LiveFeed::with_heartbeat(heartbeat);
        }
        if let Some(proxies) = self.trusted_proxies {
            state.trusted_proxies = trusted_proxies(&proxies);
        }

        TestApp {
            addr: serve(build_app(state)).await,
            db,
            store,
            client: reqwest::Client::new(),
            tmp,
        }
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
