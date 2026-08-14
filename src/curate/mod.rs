//! **Curation** — running an Instance's entities from a browser (#49, spec US
//! 45–46).
//!
//! Systems, Talkgroups, Groups, Tags, Units and API keys, each listed, created,
//! edited and deleted over `/api/admin/…`, behind the session/CSRF/lockout guard
//! #19 put over that prefix. A route added here is protected by construction —
//! [`routes`] is merged *inside* [`crate::admin_routes`]'s `route_layer`, so
//! forgetting the guard is not something a handler can do.
//!
//! # Improving on rdio-scanner
//!
//! rdio has one admin write: `PUT /api/admin/config` with the **entire
//! configuration document** (`admin.go:184`). The client sends everything back
//! and the server diffs it — a row missing from the document is deleted
//! (`system.go:487` walks the stored ids and `DELETE`s whatever the payload
//! didn't mention). Five things follow from that, and each is a reason this
//! module is shaped the way it is:
//!
//! | rdio | Radio-Scout |
//! |---|---|
//! | one PUT of the whole config; last write wins | **one row, one request** — two open tabs cannot silently undo each other |
//! | deletion is *inferred from absence* | **deletion is a `DELETE`** — a truncated or partial document destroys nothing |
//! | `FromMap` coerces what it can and drops the rest | **`deny_unknown_fields`** + a named refusal per field ([`Rejected`]) |
//! | a county-scale catalog crosses the wire per checkbox | a page of Talkgroups, filtered and paged server-side |
//! | deleting a System takes its Talkgroups and Units, and leaves its calls behind as unlabeled rows | a delete that would take **Calls** with it is **refused** until it is asked for on purpose ([`Force`]) |
//!
//! # What is *not* here
//!
//! **Member Refs and Ranges** (#45) are #50's screen — a Talkgroup's merge set
//! and a Unit's Ranges are curated there, and the rows below deliberately do not
//! carry them, so a page of Talkgroups stays one query rather than one per row.
//!
//! **Infrastructure configuration stays in the TOML** (ADR-0012). What an
//! Operator edits here is the *domain* — the entities Calls are addressed to —
//! never the port, the database URL or the storage backend. The admin password
//! stays in the environment, for the reason ADR-0008 gives.

pub mod document;
pub mod downstreams;
pub mod keys;
pub mod labels;
pub mod members;
pub mod systems;
pub mod talkgroups;
pub mod units;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use serde::{Deserialize, Deserializer, Serialize};

use crate::AppState;

/// Every curation route, for merging inside the admin guard.
///
/// One function rather than six, so that [`crate::admin_routes`] has exactly one
/// place to forget — and a test walks every path in it looking for a 401
/// (`tests/curate.rs::no_session_reaches_no_curation_route`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/admin/systems",
            get(systems::list).post(systems::create),
        )
        .route(
            "/api/admin/systems/{id}",
            patch(systems::update).delete(systems::remove),
        )
        .route(
            "/api/admin/talkgroups",
            get(talkgroups::list).post(talkgroups::create),
        )
        // Declared before the `{id}` route only for readability: a static
        // segment wins over a parameter whatever the order.
        .route("/api/admin/talkgroups/assign", post(talkgroups::assign))
        .route(
            "/api/admin/talkgroups/{id}",
            patch(talkgroups::update).delete(talkgroups::remove),
        )
        .route(
            "/api/admin/talkgroups/{id}/members",
            get(members::list_members).post(members::fold),
        )
        .route(
            "/api/admin/groups",
            get(labels::list_groups).post(labels::create_group),
        )
        .route(
            "/api/admin/groups/{id}",
            patch(labels::update_group).delete(labels::remove_group),
        )
        .route(
            "/api/admin/tags",
            get(labels::list_tags).post(labels::create_tag),
        )
        .route(
            "/api/admin/tags/{id}",
            patch(labels::update_tag).delete(labels::remove_tag),
        )
        .route("/api/admin/units", get(units::list).post(units::create))
        .route(
            "/api/admin/units/{id}",
            patch(units::update).delete(units::remove),
        )
        .route(
            "/api/admin/units/{id}/ranges",
            get(members::list_ranges).post(members::set_ranges),
        )
        .route("/api/admin/config", get(document::export))
        .route("/api/admin/config/import", post(document::import))
        .route(
            "/api/admin/downstreams",
            get(downstreams::list).post(downstreams::create),
        )
        .route(
            "/api/admin/downstreams/{id}",
            patch(downstreams::update).delete(downstreams::remove),
        )
        .route("/api/admin/api-keys", get(keys::list).post(keys::create))
        .route(
            "/api/admin/api-keys/{id}",
            patch(keys::update).delete(keys::remove),
        )
}

/// Which kind of row a refusal is about.
///
/// A closed vocabulary rather than a `&'static str` per call site, because these
/// spell the slugs an Operator greps and #70 is about to put them behind a
/// metric label — the argument [`crate::failure::Reason`] makes for its own
/// arms, one level down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    System,
    Talkgroup,
    Group,
    Tag,
    Unit,
    ApiKey,
    Downstream,
}

impl What {
    /// What an Operator calls it — the noun in a sentence they read.
    pub fn noun(self) -> &'static str {
        match self {
            What::System => "system",
            What::Talkgroup => "talkgroup",
            What::Group => "group",
            What::Tag => "tag",
            What::Unit => "unit",
            What::ApiKey => "API key",
            What::Downstream => "downstream",
        }
    }

    /// `reason=talkgroup-not-found`. Spelled as literals rather than composed
    /// from [`What::noun`] because a slug is `&'static str` by construction and
    /// a grep for one should find it written down.
    fn not_found(self) -> &'static str {
        match self {
            What::System => "system-not-found",
            What::Talkgroup => "talkgroup-not-found",
            What::Group => "group-not-found",
            What::Tag => "tag-not-found",
            What::Unit => "unit-not-found",
            What::ApiKey => "api-key-not-found",
            What::Downstream => "downstream-not-found",
        }
    }

    /// `reason=talkgroup-ref-taken`. Only the three Ref-bearing kinds can
    /// collide; the rest are named for completeness of the match rather than
    /// because anything reaches them.
    fn ref_taken(self) -> &'static str {
        match self {
            What::System => "system-ref-taken",
            What::Talkgroup => "talkgroup-ref-taken",
            What::Unit => "unit-ref-taken",
            What::Group | What::Tag | What::ApiKey | What::Downstream => "ref-taken",
        }
    }

    /// `reason=talkgroup-has-calls`.
    fn has_calls(self) -> &'static str {
        match self {
            What::System => "system-has-calls",
            What::Talkgroup => "talkgroup-has-calls",
            What::Group | What::Tag | What::Unit | What::ApiKey | What::Downstream => "has-calls",
        }
    }
}

/// A curation write the surface refused, and why.
///
/// Closed, and each arm decides its own slug and status — composed into the four
/// decisions every refusal owes by the single [`crate::failure::Reason::Curation`]
/// arm, the way [`crate::import::ParseError`] and
/// [`crate::webpush::InvalidSubscription`] already do. The body is JSON because
/// the client renders it **inline beside the field that caused it**, which is
/// the acceptance criterion this type exists for: an error an Operator has to go
/// and find in a log is one they will not find.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejected {
    /// A required value arrived blank. Names the field, so the message lands
    /// under the input rather than at the top of the form.
    Blank { field: &'static str },
    /// A name that must be unique is already another row's. rdio merges these
    /// silently, which is how two "Fire" groups become one and take half a
    /// county's talkgroups with them.
    NameTaken { name: String },
    /// A Ref already belongs to another row of the same kind, within the System
    /// it has to be unique in.
    RefTaken { what: What, taken: i64 },
    /// The row the path names is not there.
    NotFound(What),
    /// The body names a System that is not there. **400 rather than 404**: the
    /// route and its row are fine, the payload is what is wrong, and a 404 here
    /// reads as "no such endpoint".
    NoSuchSystem { system_id: i64 },
    /// An LED colour outside the palette the client can render. The same check
    /// the CSV import makes (#18) — a typo becomes a reported field, never a
    /// Talkgroup that renders with no LED.
    UnknownLed { led: String },
    /// A configuration document this Instance could not read at all (#51).
    ///
    /// Carries serde's own sentence, which names the field and the position —
    /// the caller is holding the file, so the useful answer is where in it to
    /// look. Distinct from [`Rejected::UnknownDocumentVersion`] on purpose: one
    /// says "this is from a newer release", the other "this is not the file you
    /// meant", and an Operator does different things about them.
    MalformedDocument { detail: String },
    /// A configuration document written by a version this one cannot read
    /// (#51). Refused **whole** rather than half-applied by guessing — rdio's
    /// own version check is commented out in its import component, so it takes
    /// any release's file and finds out afterwards.
    UnknownDocumentVersion { found: u32 },
    /// A Range collides with one this System already owns (#50).
    ///
    /// Names the Range *in the way* rather than saying "that overlaps
    /// something", which is not an actionable sentence about a fleet with forty
    /// of them. A Ref inside two Ranges belongs to whichever row the query
    /// returns first, so this is the refusal that keeps one radio's Calls from
    /// attributing to two different apparatus depending on the day
    /// ([`crate::merge`]).
    RangeOverlaps {
        wanted: crate::merge::Range,
        held: crate::merge::Range,
    },
    /// A delete that would take Calls with it, and nobody asked it to.
    ///
    /// Retention owns removing Calls end to end — the row, the audio object and
    /// the orphan sweep behind it — so admin refuses by default and says how
    /// many, rather than quietly becoming a second half-built deleter. `?force=true`
    /// is the Operator saying it out loud.
    HasCalls { what: What, calls: u64 },
}

impl Rejected {
    /// The slug an Operator greps: `reason=talkgroup-has-calls`.
    pub fn slug(&self) -> &'static str {
        match self {
            Rejected::Blank { .. } => "field-required",
            Rejected::NameTaken { .. } => "name-taken",
            Rejected::RefTaken { what, .. } => what.ref_taken(),
            Rejected::NotFound(what) => what.not_found(),
            Rejected::NoSuchSystem { .. } => "no-such-system",
            Rejected::RangeOverlaps { .. } => "range-overlaps",
            Rejected::UnknownDocumentVersion { .. } => "unknown-document-version",
            Rejected::MalformedDocument { .. } => "malformed-document",
            Rejected::UnknownLed { .. } => "unknown-led",
            Rejected::HasCalls { what, .. } => what.has_calls(),
        }
    }

    /// What the caller is owed on the wire.
    pub fn status(&self) -> StatusCode {
        match self {
            Rejected::Blank { .. }
            | Rejected::NoSuchSystem { .. }
            | Rejected::UnknownDocumentVersion { .. }
            | Rejected::MalformedDocument { .. }
            | Rejected::UnknownLed { .. } => StatusCode::BAD_REQUEST,
            Rejected::NotFound(_) => StatusCode::NOT_FOUND,
            Rejected::NameTaken { .. }
            | Rejected::RefTaken { .. }
            | Rejected::RangeOverlaps { .. }
            | Rejected::HasCalls { .. } => StatusCode::CONFLICT,
        }
    }

    /// The document the form renders. `error` is the slug a client branches on,
    /// `detail` the sentence it shows, and the rest is what a *particular*
    /// refusal needs to be shown well: which input to mark, and how much would
    /// have been destroyed.
    pub fn body(&self) -> serde_json::Value {
        let mut body = serde_json::json!({
            "error": self.slug(),
            "detail": self.to_string(),
        });
        if let Rejected::Blank { field } = self {
            body["field"] = serde_json::Value::from(*field);
        }
        if let Rejected::HasCalls { calls, .. } = self {
            body["calls"] = serde_json::Value::from(*calls);
        }
        body
    }
}

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Rejected::Blank { field } => write!(f, "{field} is required"),
            Rejected::NameTaken { name } => write!(f, "the name {name:?} is already taken"),
            Rejected::RefTaken { what, taken } => {
                write!(f, "another {} already answers to {taken}", what.noun())
            }
            Rejected::NotFound(what) => write!(f, "no such {}", what.noun()),
            Rejected::NoSuchSystem { system_id } => write!(f, "no system with id {system_id}"),
            Rejected::MalformedDocument { detail } => {
                write!(f, "this is not a configuration document: {detail}")
            }
            Rejected::UnknownDocumentVersion { found } => write!(
                f,
                "this is a version {found} configuration document, and this instance reads version {}",
                document::VERSION
            ),
            Rejected::RangeOverlaps { wanted, held } => write!(
                f,
                "{}-{} overlaps {}-{}, which is already owned on this system",
                wanted.from(),
                wanted.to(),
                held.from(),
                held.to()
            ),
            Rejected::UnknownLed { led } => write!(
                f,
                "{led:?} is not an LED colour: choose one of {}",
                crate::import::LED_PALETTE.join(", ")
            ),
            Rejected::HasCalls { what, calls } => write!(
                f,
                "this {} still has {calls} calls in the archive; \
                 delete them with it by asking for force, or wait for retention",
                what.noun()
            ),
        }
    }
}

impl From<Rejected> for crate::failure::Failure {
    fn from(rejected: Rejected) -> Self {
        crate::failure::Reason::Curation(rejected).into()
    }
}

/// One small collection, whole.
///
/// The curation counterpart to [`crate::query::Page`], and deliberately the same
/// `results` key so a client that has learned one has learned both. Systems,
/// Groups, Tags and API keys are counted in tens on the largest instance
/// anybody runs, so paging them would be a control that never does anything —
/// the argument [`crate::logview`] makes for not offering a log level the sink
/// cannot store. Talkgroups and Units *are* counted in thousands, and those two
/// answer with a `Page`.
#[derive(Debug, Clone, Serialize)]
pub struct Listing<T> {
    pub results: Vec<T>,
}

impl<T> Listing<T> {
    pub fn new(results: Vec<T>) -> Self {
        Listing { results }
    }
}

impl<T: Serialize> IntoResponse for Listing<T> {
    fn into_response(self) -> Response {
        axum::Json(self).into_response()
    }
}

/// A row that has just been created — `201`, with the row itself.
///
/// A wrapper rather than a bare `(StatusCode, Json<T>)` at each call site, so
/// "a create answers 201" is one decision in one place (#92's rule, applied to
/// the successes as well as the refusals).
pub struct Created<T>(pub T);

impl<T: Serialize> IntoResponse for Created<T> {
    fn into_response(self) -> Response {
        (StatusCode::CREATED, axum::Json(self.0)).into_response()
    }
}

/// A row that is gone — `204`, and nothing to say.
pub struct Removed;

impl IntoResponse for Removed {
    fn into_response(self) -> Response {
        StatusCode::NO_CONTENT.into_response()
    }
}

/// Which Calls a force-delete is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallScope {
    System(i64),
    Talkgroup(i64),
}

/// How many Calls one purge pass takes at a time.
///
/// [`crate::retention`]'s own default, and for its reason: a short SQLite
/// write-lock window per page is what keeps a delete over a large Archive from
/// stalling ingest on a Pi. A System with a year of traffic is a long delete
/// either way; it must not be a *blocking* one.
const PURGE_BATCH: u64 = 500;

/// Delete every Call in `scope`, audio objects included.
///
/// **Retention's pass, borrowed rather than re-implemented.** Removing a Call
/// correctly is row-then-object with an orphan sweep behind it for the objects a
/// delete could not reach ([`crate::retention::prune`]), and a second
/// half-written copy of that here is exactly how audio gets stranded in a bucket
/// nobody is billing anybody for by accident. What curation adds is the
/// configuration underneath the Calls, which retention has no reason to touch.
async fn purge_calls(state: &AppState, scope: CallScope) -> Result<u64, crate::failure::Failure> {
    use crate::failure::Stage;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};

    // **WARN, before a single row goes.** This is the most destructive thing an
    // Operator can ask this surface for, it is irreversible, and retention —
    // which does the same work on a policy — reports every sweep. A line only
    // *after* it finished would be missing from exactly the run that died
    // half-way (ADR-0011 rule 7).
    let (what, id) = match scope {
        CallScope::System(id) => ("system", id),
        CallScope::Talkgroup(id) => ("talkgroup", id),
    };
    tracing::warn!(%what, id, "deleting every call in the archive under this entity");

    let mut purged = 0;
    loop {
        let page = crate::db::entities::call::Entity::find()
            .filter(match scope {
                CallScope::System(id) => crate::db::entities::call::Column::SystemId.eq(id),
                CallScope::Talkgroup(id) => crate::db::entities::call::Column::TalkgroupId.eq(id),
            })
            .order_by_asc(crate::db::entities::call::Column::Id)
            .limit(PURGE_BATCH)
            .all(&state.db)
            .await
            .map_err(Stage::PurgeCalls.failed())?;
        if page.is_empty() {
            tracing::warn!(%what, id, purged, "archive purge finished");
            return Ok(purged);
        }
        purged += page.len() as u64;
        let batch: Vec<crate::db::repo::PrunableCall> = page.into_iter().map(Into::into).collect();
        crate::retention::prune(&state.db, state.audio.as_ref(), &batch)
            .await
            .map_err(Stage::PurgeCalls.failed())?;
    }
}

/// `?force=true` — the Operator asking for a delete that takes Calls with it.
///
/// A query parameter rather than a body field because a `DELETE` with a body is
/// a thing many proxies and clients quietly drop, and this is the one flag whose
/// loss would be silently catastrophic in the *other* direction (a refusal, not
/// a deletion — but an Operator who cannot delete anything is an Operator who
/// stops trusting the screen).
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Force {
    #[serde(default)]
    pub force: bool,
}

/// Read a nullable field so that **absent and null mean different things**.
///
/// A `PATCH` says "leave everything I did not mention alone", which makes
/// clearing a label impossible unless the wire can tell an omitted field from an
/// explicit `null`. Serde folds both onto `None` for a plain `Option`, so a
/// nullable field is `Option<Option<T>>` read through here: missing stays `None`
/// (the `#[serde(default)]`), and `null` arrives as `Some(None)`.
///
/// The CSV importer takes the opposite convention on purpose (#18: "a blank cell
/// means leave alone, never clear"), because a spreadsheet has no way to spell
/// the difference and a form does.
pub fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

/// A required name, trimmed — or the refusal for a blank one.
///
/// Trimming rather than refusing whitespace outright: an Operator who pastes
/// `"Fire "` out of a spreadsheet means `Fire`, and two Groups differing by a
/// trailing space are indistinguishable in every panel that renders them.
pub fn required_name(raw: &str, field: &'static str) -> Result<String, Rejected> {
    let trimmed = raw.trim();
    match trimmed.is_empty() {
        true => Err(Rejected::Blank { field }),
        false => Ok(trimmed.to_owned()),
    }
}

/// An optional text field, trimmed, where blank reads as **absent**.
///
/// A form's empty input and a cleared value are the same keystroke, so
/// `label: ""` clears the label rather than storing an empty string that renders
/// as a channel with a name made of nothing.
pub fn optional_text(raw: Option<String>) -> Option<String> {
    raw.map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

/// What a nullable text field becomes once a `PATCH` has named it.
///
/// `null` clears it and so does `""`, because a form's cleared input and an
/// explicit clear are the same keystroke — the composition of [`nullable`] (is
/// this field mentioned at all?) with [`optional_text`] (does it say anything?).
/// Spelled once because four fields on three entities need exactly this, and
/// four copies is four chances for one to store the empty string.
pub fn cleared(named: Option<String>) -> Option<String> {
    optional_text(named)
}

/// An LED colour the client can actually render, or the refusal naming it.
///
/// The palette is [`crate::import::LED_PALETTE`], shared with the CSV importer —
/// one list, so a colour the importer accepts is one this form offers and vice
/// versa.
pub fn checked_led(led: Option<String>) -> Result<Option<String>, Rejected> {
    let Some(led) = optional_text(led) else {
        return Ok(None);
    };
    let led = led.to_lowercase();
    match crate::import::LED_PALETTE.contains(&led.as_str()) {
        true => Ok(Some(led)),
        false => Err(Rejected::UnknownLed { led }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// Every kind there is, so the tables below cannot go stale by omission.
    const KINDS: [What; 7] = [
        What::System,
        What::Talkgroup,
        What::Group,
        What::Tag,
        What::Unit,
        What::ApiKey,
        What::Downstream,
    ];

    /// Every kind gets its **own** not-found slug — a shared one would make two
    /// different refusals indistinguishable to the client branching on them, and
    /// to #70's metric label.
    #[test]
    fn every_kind_gets_its_own_not_found_slug() {
        let slugs: std::collections::HashSet<&str> =
            KINDS.iter().map(|what| what.not_found()).collect();

        assert_eq!(slugs.len(), KINDS.len(), "a shared not-found slug");
    }

    /// The other two slug tables are total, and every kind reads as something.
    ///
    /// Three kinds cannot collide on a Ref and four cannot hold Calls, so those
    /// arms are unreachable through a handler — but a slug table with a hole in
    /// it is a `""` on an Operator's log line the day a kind grows the
    /// capability, and the match is cheaper to keep honest than to remember.
    #[test]
    fn every_kind_reads_as_something_in_every_table() {
        for what in KINDS {
            assert!(!what.noun().is_empty(), "{what:?}");
            assert!(!what.ref_taken().is_empty(), "{what:?}");
            assert!(!what.has_calls().is_empty(), "{what:?}");
        }
        // The kinds that really can collide or really can hold Calls say which
        // they are, rather than sharing the catch-all.
        assert_eq!(What::Talkgroup.ref_taken(), "talkgroup-ref-taken");
        assert_eq!(What::System.has_calls(), "system-has-calls");
        assert_eq!(What::Group.ref_taken(), "ref-taken");
        assert_eq!(What::Unit.has_calls(), "has-calls");
    }

    /// A refusal's body always carries the two fields a client branches on, and
    /// carries the extra ones only where they mean something — an empty `field`
    /// on a conflict would be a control the form would try to point at.
    #[rstest]
    #[case::blank(Rejected::Blank { field: "name" }, 400, "field-required", true, false)]
    #[case::taken(Rejected::NameTaken { name: String::from("Fire") }, 409, "name-taken", false, false)]
    #[case::ref_taken(
        Rejected::RefTaken { what: What::Talkgroup, taken: 100 },
        409,
        "talkgroup-ref-taken",
        false,
        false
    )]
    #[case::gone(Rejected::NotFound(What::Unit), 404, "unit-not-found", false, false)]
    #[case::no_system(Rejected::NoSuchSystem { system_id: 7 }, 400, "no-such-system", false, false)]
    #[case::led(Rejected::UnknownLed { led: String::from("puce") }, 400, "unknown-led", false, false)]
    #[case::overlap(
        Rejected::RangeOverlaps {
            wanted: crate::merge::Range::new(1250, 1350),
            held: crate::merge::Range::new(1200, 1299),
        },
        409,
        "range-overlaps",
        false,
        false
    )]
    #[case::calls(
        Rejected::HasCalls { what: What::System, calls: 12 },
        409,
        "system-has-calls",
        false,
        true
    )]
    fn a_refusal_decides_its_slug_status_and_body(
        #[case] rejected: Rejected,
        #[case] status: u16,
        #[case] slug: &str,
        #[case] names_a_field: bool,
        #[case] counts_calls: bool,
    ) {
        let body = rejected.body();

        assert_eq!(rejected.slug(), slug);
        assert_eq!(rejected.status().as_u16(), status);
        assert_eq!(body["error"], slug);
        assert!(
            body["detail"].as_str().is_some_and(|d| !d.is_empty()),
            "{body}"
        );
        assert_eq!(body.get("field").is_some(), names_a_field, "{body}");
        assert_eq!(body.get("calls").is_some(), counts_calls, "{body}");
    }

    /// An overlap names **both** Ranges — the one asked for and the one in the
    /// way. "That overlaps something" is not an actionable sentence about a
    /// fleet with forty of them, which is why [`crate::merge::first_overlap`]
    /// returns the collision rather than a boolean.
    #[test]
    fn an_overlapping_range_names_the_one_in_the_way() {
        let told = Rejected::RangeOverlaps {
            wanted: crate::merge::Range::new(1250, 1350),
            held: crate::merge::Range::new(1200, 1299),
        }
        .to_string();

        for number in ["1250", "1350", "1200", "1299"] {
            assert!(told.contains(number), "{told}");
        }
    }

    /// The unknown-LED sentence lists what *is* allowed, because "puce is not an
    /// LED colour" without the alternatives sends an Operator to the source.
    #[test]
    fn an_unknown_led_names_the_palette() {
        let told = Rejected::UnknownLed {
            led: String::from("puce"),
        }
        .to_string();

        for colour in crate::import::LED_PALETTE {
            assert!(told.contains(colour), "{told}");
        }
    }

    /// A name is trimmed, and whitespace alone is blank.
    #[rstest]
    #[case("Fire", Some("Fire"))]
    #[case("  Fire  ", Some("Fire"))]
    #[case("Fire Dispatch", Some("Fire Dispatch"))]
    #[case("", None)]
    #[case("   ", None)]
    #[case("\t\n", None)]
    fn a_required_name_is_trimmed_and_never_blank(
        #[case] raw: &str,
        #[case] expected: Option<&str>,
    ) {
        assert_eq!(
            required_name(raw, "name").ok().as_deref(),
            expected,
            "{raw:?}"
        );
    }

    /// An optional field's empty string is *absent*, which is how a form clears
    /// one — storing `""` would render a channel labelled with nothing.
    #[rstest]
    #[case(None, None)]
    #[case(Some(""), None)]
    #[case(Some("  "), None)]
    #[case(Some(" FD1 "), Some("FD1"))]
    fn a_blank_optional_field_is_absent(#[case] raw: Option<&str>, #[case] expected: Option<&str>) {
        assert_eq!(
            optional_text(raw.map(str::to_owned)).as_deref(),
            expected,
            "{raw:?}"
        );
    }

    /// The palette is checked case-insensitively — an Operator typing `Red` in a
    /// form means the same colour the CSV importer's `red` does.
    #[rstest]
    #[case(None, Ok(None))]
    #[case(Some(""), Ok(None))]
    #[case(Some("red"), Ok(Some("red")))]
    #[case(Some("RED"), Ok(Some("red")))]
    #[case(Some(" Cyan "), Ok(Some("cyan")))]
    #[case(Some("puce"), Err("puce"))]
    fn an_led_is_checked_against_the_palette(
        #[case] raw: Option<&str>,
        #[case] expected: Result<Option<&str>, &str>,
    ) {
        let checked = checked_led(raw.map(str::to_owned));

        match expected {
            Ok(colour) => assert_eq!(checked.expect("accepted").as_deref(), colour),
            Err(led) => assert_eq!(
                checked.expect_err("refused"),
                Rejected::UnknownLed {
                    led: led.to_owned()
                }
            ),
        }
    }

    /// Absent and `null` are different things, which is the whole reason this
    /// reader exists: without it a `PATCH` could set a label and never clear one.
    #[derive(Debug, Deserialize)]
    struct Patch {
        #[serde(default, deserialize_with = "nullable")]
        label: Option<Option<String>>,
    }

    #[rstest]
    #[case("{}", None)]
    #[case(r#"{"label": null}"#, Some(None))]
    #[case(r#"{"label": "FD1"}"#, Some(Some("FD1")))]
    fn an_omitted_field_and_an_explicit_null_are_told_apart(
        #[case] json: &str,
        #[case] expected: Option<Option<&str>>,
    ) {
        let patch: Patch = serde_json::from_str(json).expect("a patch");

        assert_eq!(
            patch.label,
            expected.map(|inner| inner.map(String::from)),
            "{json}"
        );
    }
}
