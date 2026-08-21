//! **The curated configuration as one portable document** (#51, spec US 47).
//!
//! Everything #49 made editable a row at a time, carried whole: Systems with
//! their blacklists, Talkgroups with their Groups, Tags, LED colours and member
//! Refs (#45), Units with their Ranges, and the API-key roster minus every
//! secret. Out through `GET /api/admin/config`, back in through
//! `POST /api/admin/config/import`.
//!
//! Infrastructure is **not** here and never will be: the port, the database URL
//! and the storage backend live in the TOML (ADR-0012), and an artifact that
//! moved those between machines would be a way to point a second Instance at the
//! first one's bucket.
//!
//! # Improving on rdio-scanner
//!
//! rdio has this feature, and its shape is instructive
//! (`admin/tools/import-export-config/`):
//!
//! | rdio | Radio-Scout |
//! |---|---|
//! | the config document **including `apikeys` with their plaintext keys** | **no secret in the file** — the roster's shape, never a credential |
//! | the version check is *literally commented out* in the component | the document carries a `version` and an unknown one is **refused whole** |
//! | import is client-side key-munging (`_id`→`id`, `apiKey`→`apikey`) then a whole-document `PUT` | the server reads the document; **absence never deletes** |
//! | no dry run; failures surface as `error as string` in a snackbar | **`?dryRun`**, and every refused entry names its **path** |
//! | rows keyed by database id, so a document is only meaningful to the instance that wrote it | keyed by **Ref** and name, so it **moves between Instances** |
//!
//! # A document is a state, where a CSV row is an edit
//!
//! This is the one thing to get right, and it is the opposite of [`crate::import`]'s
//! rule. A CSV cell that is blank means *leave the stored value alone* (#18),
//! because a spreadsheet has no way to spell the difference and an operator
//! importing a two-column `ref,led` file must not erase everything else. A
//! document entry is a **snapshot of the whole row**, so a field it does not
//! carry is a field the row does not have — which is exactly what makes
//! export → import reproduce what was exported rather than merge into it.
//!
//! So the two upserts are deliberately separate code. What they share is the
//! layer below: [`repo::resolve_or_create_tag`], [`repo::set_member_refs`],
//! [`repo::set_unit_ranges`], [`super::systems::blacklist_text`] — the writers,
//! not the policy.
//!
//! # What the export deliberately leaves out
//!
//! - **Secrets.** `api_keys.key_hash` is unsalted SHA-256, so an exported hash
//!   is offline-crackable for any Operator who chose a key they could remember,
//!   and this file gets emailed and committed. An imported key is **re-issued**
//!   and shown once, exactly as [`super::keys::create`] shows one.
//! - **Sites.** Mined out of what SDRTrunk already sent (#48), not curated, and
//!   they re-arrive from the next Call.
//! - **Units nobody curated.** An Instance rosters a Unit per radio it has ever
//!   heard (#47); a bare number re-rosters from the first Call that keys. Only a
//!   Unit with a name or a Range is carried, which is the difference between a
//!   readable county export and ten thousand contentless entries.
//! - **Ids, and the time it was taken.** The document is a **pure function of
//!   the configuration**: same configuration, same bytes. That is what makes it
//!   diffable in git — spec US 47's "versionable" — so a diff shows what an
//!   Operator changed rather than when they last looked. The date rides in the
//!   `Content-Disposition` filename, where they actually read it.

use std::collections::{HashMap, HashSet};

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter, Set,
};
use serde::{Deserialize, Serialize};

use super::members::Span;
use super::systems::{blacklist_of, blacklist_text};
use super::{Rejected, checked_led, optional_text};
use crate::AppState;
use crate::db::entities::{
    api_key, group, system, tag, talkgroup, talkgroup_group, talkgroup_ref, tone_profile, unit,
    unit_ref,
};
use crate::db::repo;
use crate::failure::{Failure, Stage};
use crate::merge::Range;

/// The only document shape this Instance writes or reads.
///
/// Bumped when the shape changes in a way an older reader would misread — never
/// for an added optional field, which an older reader ignores and a newer one
/// defaults. An unknown version is refused whole rather than half-applied by
/// guessing, which is the check rdio has and leaves commented out.
pub const VERSION: u32 = 1;

// -- The document ------------------------------------------------------------

/// Everything an Operator curated, as one artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Document {
    pub version: u32,
    /// Systems, each carrying the Talkgroups and Units addressed under it.
    ///
    /// **Nested rather than flat**, because a Ref is unique only within a System
    /// (CONTEXT.md) — a flat list would repeat the System on every row and make
    /// "which System is this Ref in?" a question the reader could get wrong.
    #[serde(default)]
    pub systems: Vec<SystemEntry>,
    /// Every Group name, including ones no Talkgroup is in yet — an Operator who
    /// made "Fire" before assigning it meant to keep it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// The key roster's *shape* — never a credential. See the module docs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub api_keys: Vec<KeyEntry>,
    /// **Downstream** peers' shape — never the key they issued us (#52).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub downstreams: Vec<DownstreamEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SystemEntry {
    pub r#ref: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub auto_populate: bool,
    /// Talkgroup Refs never ingested here — canonical, sorted, de-duplicated by
    /// the same parser the System form writes through.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blacklist: Vec<i64>,
    /// `null`/absent inherits the instance's `[enhancement] mode` (#20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enhancement: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub talkgroups: Vec<TalkgroupEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub units: Vec<UnitEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TalkgroupEntry {
    pub r#ref: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub led: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enhancement: Option<bool>,
    /// The other Refs this channel answers to (#45) — a merge is configuration,
    /// so a restore that dropped it would re-flood the panel with the churn the
    /// Operator folded away.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub member_refs: Vec<i64>,
    /// The **Tone profiles** paged on this channel (#55).
    ///
    /// Carried for `member_refs`' reason: a profile is something an Operator sat
    /// down and worked out from a recording, and a restore that dropped it would
    /// leave a station silently unwatched — which looks exactly like a station
    /// that has not been paged. There is no secret in one, unlike a
    /// [`crate::webhook`], so it belongs in a file an Operator can email.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tones: Vec<ToneEntry>,
}

/// One **Tone profile**, as a backup carries it (#55).
///
/// Keyed by `label` on the way back in — the way a Group and a Tag are — because
/// a profile has no Ref and its *name* is what an Operator identifies it by. Two
/// profiles on one channel with the same label are the same profile, which is
/// also what makes a restore idempotent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToneEntry {
    pub label: String,
    pub steps: Vec<crate::tone::Step>,
    pub tolerance_pct: f64,
    pub gap_max_ms: i64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnitEntry {
    pub r#ref: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ranges: Vec<super::members::Span>,
}

/// One API key, without the thing that makes it a key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeyEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The System Ref it may ingest to; `null` grants every System.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_ref: Option<i64>,
    #[serde(default)]
    pub disabled: bool,
}

/// One **Downstream** peer, as a backup can carry it (#52).
///
/// **No key**, for the reason no credential is in this file at all: it gets
/// emailed and committed. rdio's export includes every downstream's `apikey` in
/// plaintext, which is the same document an Operator pastes into a support
/// thread.
///
/// So an imported peer arrives **disabled**, whatever it was when it was
/// exported — a peer with no credential would answer `401` on every Call and
/// spend an Operator's afternoon looking like a network problem. Disabled is the
/// honest state for "here is the peer you configured; give it its key back", and
/// it is what the screen shows a `hasKey: false` row as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DownstreamEntry {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Which Calls reach it — the live feed's own **Selection** JSON, carried
    /// verbatim so what an Operator ticked is what comes back.
    pub scope: crate::selection::Selection,
}

// -- What an import did ------------------------------------------------------

/// How one kind of row fared. Exactly one of the three per entry, so the totals
/// add up to what the document named.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    pub created: u64,
    pub updated: u64,
    /// Rows the Instance already had exactly — the count that proves a re-import
    /// is a no-op, and therefore that a half-finished restore is safe to retry.
    pub unchanged: u64,
}

/// One entry the import would not take, and where it is.
///
/// `at` is a **path into the document** (`systems[0].talkgroups[3]`), which is a
/// JSON file's answer to the CSV importer's line number: an Operator has to be
/// able to find the entry in the file they are holding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectedEntry {
    pub at: String,
    pub reason: &'static str,
    pub detail: String,
}

/// What an import did — or, on a dry run, would do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentReport {
    /// True when nothing was written: the same work, rolled back.
    pub dry_run: bool,
    pub systems: Applied,
    pub talkgroups: Applied,
    pub units: Applied,
    /// **Tone profiles** (#55), counted like the rest: an Operator restoring a
    /// county wants to know how many stations are being watched again.
    pub tones: Applied,
    pub groups_created: u64,
    pub tags_created: u64,
    /// Keys issued by this import, each shown **once**. Empty on a dry run: a
    /// preview must not mint a credential, and showing one it then rolled back
    /// would be worse than showing none.
    pub api_keys: Vec<super::keys::IssuedKey>,
    /// How many keys the roster is short — counted on a **dry run too**, which
    /// is the only thing a preview can honestly say about a credential it must
    /// not create. Without it a preview of a six-key roster reports nothing and
    /// the real run then issues six.
    pub api_keys_to_issue: u64,
    /// Peers the import added, each **disabled until it is given a key** — the
    /// count rather than the rows, because the screen re-lists them anyway and
    /// what an Operator needs from the report is "and there are three peers to
    /// re-key".
    pub downstreams_to_key: u64,
    /// Every entry that was not applied, in document order.
    pub rejected: Vec<RejectedEntry>,
}

crate::answers_json!(DocumentReport);

// -- Handlers ----------------------------------------------------------------

/// `GET /api/admin/config` — the whole curated configuration, as a download.
pub async fn export(State(state): State<AppState>) -> Result<Exported, Failure> {
    let document = read(&state.db).await.map_err(Stage::Curate.failed())?;

    Ok(Exported {
        filename: filename(state.clock.now_ms()),
        document,
    })
}

/// `POST /api/admin/config/import` — take one back, wholly or not at all.
///
/// `?dryRun` reads as a flag exactly as the CSV import's does
/// ([`crate::import::asked_for_a_dry_run`]).
pub async fn import(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Result<DocumentReport, Failure> {
    let document = read_document(body)?;
    let dry_run = crate::import::asked_for_a_dry_run(&params);
    let now_ms = state.clock.now_ms();

    let txn = state.db.begin().await.map_err(Stage::Curate.failed())?;
    let report = apply(&txn, &document, dry_run, now_ms)
        .await
        .map_err(Stage::Curate.failed())?;
    match dry_run {
        true => txn.rollback().await,
        false => txn.commit().await,
    }
    .map_err(Stage::Curate.failed())?;

    // A restored document may have brought this Instance its first **Tone
    // profile** (#55), and the roster gate is a cached bit — so it is re-read
    // here as it is on every other write that could change it, and a restored
    // pager is watched from the next Call rather than from the next restart.
    // After the commit, and skipped on a dry run, because nothing was written.
    if !dry_run {
        state.tones.rearm(&state.db).await;
    }

    // Built before the macro rather than inside it, for the reason
    // `MergeChange::record` gives: a `tracing` field expression runs only when
    // a subscriber is interested, which makes it look unreachable to coverage.
    let systems = report.systems.created + report.systems.updated;
    let talkgroups = report.talkgroups.created + report.talkgroups.updated;
    let units = report.units.created + report.units.updated;
    let keys_issued = report.api_keys_to_issue;
    let rejected = report.rejected.len();
    tracing::info!(
        dry_run,
        systems,
        talkgroups,
        units,
        keys_issued,
        rejected,
        "configuration document imported"
    );
    Ok(report)
}

/// **The version is read before the shape is enforced**, and that order is the
/// whole point of having a version.
///
/// [`Document`] is `deny_unknown_fields`, which is what catches a typo in a
/// hand-edited file. Applied first, it would also reject every document from a
/// *newer* release — with serde's "unknown field `downstreams`" rather than
/// "this is from a version this instance cannot read", which is the one sentence
/// that tells an Operator to upgrade. So the version is taken off the raw body
/// first, and only a document claiming to be ours is held to our shape.
fn read_document(body: serde_json::Value) -> Result<Document, Rejected> {
    match body.get("version").and_then(serde_json::Value::as_u64) {
        Some(version) if version == u64::from(VERSION) => {}
        Some(version) => {
            return Err(Rejected::UnknownDocumentVersion {
                found: version.min(u64::from(u32::MAX)) as u32,
            });
        }
        // No version at all is not a document of ours in any release — most
        // often the wrong file entirely, so it is named as such rather than as
        // a version nobody wrote.
        None => {
            return Err(Rejected::MalformedDocument {
                detail: String::from("it carries no version"),
            });
        }
    }

    serde_json::from_value(body).map_err(|error| Rejected::MalformedDocument {
        detail: error.to_string(),
    })
}

/// The document, as a file a browser saves rather than a page it renders.
pub struct Exported {
    filename: String,
    document: Document,
}

impl IntoResponse for Exported {
    fn into_response(self) -> Response {
        (
            [(
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", self.filename),
            )],
            axum::Json(self.document),
        )
            .into_response()
    }
}

/// `radio-scout-config-2026-08-13.json`.
///
/// The date is here and **not in the document**, so the bytes stay a pure
/// function of the configuration and two backups still do not collide in a
/// Downloads folder. A clock that cannot be read is not worth failing an export
/// over, so an unformattable instant simply leaves the date off.
fn filename(now_ms: i64) -> String {
    let stamp = time::OffsetDateTime::from_unix_timestamp(now_ms.div_euclid(1_000))
        .ok()
        .and_then(|at| {
            at.format(&time::macros::format_description!("-[year]-[month]-[day]"))
                .ok()
        })
        .unwrap_or_default();

    format!("radio-scout-config{stamp}.json")
}

// -- Reading -----------------------------------------------------------------

/// Build the document from the Instance's rows.
///
/// **Nine statements whatever the size** — one per table, not one per row (#86's
/// rule). A county's configuration is hundreds of Talkgroups and thousands of
/// Units, and the per-row shape would be thousands of round trips on a Pi.
pub async fn read<C: ConnectionTrait>(db: &C) -> Result<Document, DbErr> {
    let systems = system::Entity::find().all(db).await?;
    let talkgroups = talkgroup::Entity::find().all(db).await?;
    let units = unit::Entity::find().all(db).await?;
    let tags: HashMap<i64, String> = tag::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|row| (row.id, row.name))
        .collect();
    let group_names: HashMap<i64, String> = group::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|row| (row.id, row.name))
        .collect();

    let mut groups_of: HashMap<i64, Vec<String>> = HashMap::new();
    for link in talkgroup_group::Entity::find().all(db).await? {
        if let Some(name) = group_names.get(&link.group_id) {
            groups_of
                .entry(link.talkgroup_id)
                .or_default()
                .push(name.clone());
        }
    }
    let mut members_of: HashMap<i64, Vec<(i32, i64)>> = HashMap::new();
    for member in talkgroup_ref::Entity::find().all(db).await? {
        members_of
            .entry(member.talkgroup_id)
            .or_default()
            .push((member.position, member.r#ref));
    }
    let mut tones_of: HashMap<i64, Vec<ToneEntry>> = HashMap::new();
    for profile in tone_profile::Entity::find().all(db).await? {
        tones_of
            .entry(profile.talkgroup_id)
            .or_default()
            .push(ToneEntry {
                label: profile.label,
                steps: crate::tone::steps_of(&profile.steps),
                tolerance_pct: profile.tolerance_pct,
                gap_max_ms: profile.gap_max_ms,
                disabled: profile.disabled,
            });
    }
    let mut ranges_of: HashMap<i64, Vec<(i32, Range)>> = HashMap::new();
    for span in unit_ref::Entity::find().all(db).await? {
        ranges_of
            .entry(span.unit_id)
            .or_default()
            .push((span.position, Range::new(span.ref_from, span.ref_to)));
    }

    let mut entries: Vec<SystemEntry> = systems
        .into_iter()
        .map(|row| {
            let mut channels: Vec<TalkgroupEntry> = talkgroups
                .iter()
                .filter(|channel| channel.system_id == row.id)
                .map(|channel| {
                    let mut groups = groups_of.get(&channel.id).cloned().unwrap_or_default();
                    groups.sort();
                    let mut members = members_of.get(&channel.id).cloned().unwrap_or_default();
                    members.sort();
                    // By label, so two Instances with the same setup write the
                    // same bytes — the document is a pure function of the
                    // configuration, never of insertion order (#51).
                    let mut tones = tones_of.get(&channel.id).cloned().unwrap_or_default();
                    tones.sort_by(|a, b| a.label.cmp(&b.label));
                    TalkgroupEntry {
                        r#ref: channel.r#ref,
                        label: channel.label.clone(),
                        name: channel.name.clone(),
                        tag: channel.tag_id.and_then(|id| tags.get(&id).cloned()),
                        groups,
                        led: channel.led.clone(),
                        enhancement: channel.enhancement,
                        member_refs: members.into_iter().map(|(_, r#ref)| r#ref).collect(),
                        tones,
                    }
                })
                .collect();
            channels.sort_by_key(|channel| channel.r#ref);

            let mut apparatus: Vec<UnitEntry> = units
                .iter()
                .filter(|radio| radio.system_id == row.id)
                .filter_map(|radio| {
                    let mut spans = ranges_of.get(&radio.id).cloned().unwrap_or_default();
                    spans.sort();
                    // Curation is a name or a Range. A bare roster entry is what
                    // the next Call would have created anyway.
                    if radio.label.is_none() && spans.is_empty() {
                        return None;
                    }
                    Some(UnitEntry {
                        r#ref: radio.r#ref,
                        label: radio.label.clone(),
                        ranges: spans.into_iter().map(|(_, span)| span.into()).collect(),
                    })
                })
                .collect();
            apparatus.sort_by_key(|radio| radio.r#ref);

            SystemEntry {
                r#ref: row.r#ref,
                label: row.label,
                auto_populate: row.auto_populate,
                blacklist: blacklist_of(row.blacklist.as_deref()),
                enhancement: row.enhancement,
                talkgroups: channels,
                units: apparatus,
            }
        })
        .collect();
    entries.sort_by_key(|entry| entry.r#ref);

    let mut group_list: Vec<String> = group_names.into_values().collect();
    group_list.sort();
    let mut tag_list: Vec<String> = tags.into_values().collect();
    tag_list.sort();
    let mut keys: Vec<KeyEntry> = api_key::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|row| KeyEntry {
            label: row.label,
            system_ref: row.system_ref,
            disabled: row.disabled,
        })
        .collect();
    // By value, never by id or instant: two Instances holding the same roster
    // must write the same bytes, and their ids and timestamps differ.
    keys.sort();

    let mut peers: Vec<DownstreamEntry> = crate::db::entities::downstream::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|row| DownstreamEntry {
            url: row.url,
            label: row.label,
            scope: crate::downstream::scope_of(&row.scope),
        })
        .collect();
    // By value like the keys above, and for the same reason: two Instances
    // holding the same roster must write the same bytes. By URL and label
    // rather than by the whole entry, because a **Selection** is a `HashMap` and
    // has no order — and the URL is the identity a restore matches on anyway, so
    // two entries that tie here are two peers pointed at one address, which sort
    // stably and therefore still write the same bytes.
    peers.sort_by(|left, right| (&left.url, &left.label).cmp(&(&right.url, &right.label)));

    Ok(Document {
        version: VERSION,
        systems: entries,
        groups: group_list,
        tags: tag_list,
        api_keys: keys,
        downstreams: peers,
    })
}

// -- Writing -----------------------------------------------------------------

/// Apply a document, accumulating the report.
///
/// Split from [`import`] so the transaction boundary — and therefore the
/// commit-or-roll-back a dry run turns on — belongs to the caller, exactly as
/// [`crate::import`] splits its own.
async fn apply<C: ConnectionTrait>(
    db: &C,
    document: &Document,
    dry_run: bool,
    now_ms: i64,
) -> Result<DocumentReport, DbErr> {
    let mut report = DocumentReport {
        dry_run,
        systems: Applied::default(),
        talkgroups: Applied::default(),
        units: Applied::default(),
        tones: Applied::default(),
        groups_created: 0,
        tags_created: 0,
        api_keys: Vec::new(),
        api_keys_to_issue: 0,
        downstreams_to_key: 0,
        rejected: Vec::new(),
    };

    // Groups and Tags first: a Talkgroup entry names them, and one the document
    // lists but nothing is in would otherwise be lost.
    for name in document
        .groups
        .iter()
        .filter_map(|name| optional_text(Some(name.clone())))
    {
        report.groups_created += u64::from(repo::ensure_group(db, &name, now_ms).await?.1);
    }
    for name in document
        .tags
        .iter()
        .filter_map(|name| optional_text(Some(name.clone())))
    {
        report.tags_created += u64::from(repo::ensure_tag(db, &name, now_ms).await?.1);
    }

    for (index, entry) in document.systems.iter().enumerate() {
        let at = format!("systems[{index}]");
        apply_system(db, entry, &at, now_ms, &mut report).await?;
    }
    apply_keys(db, document, dry_run, now_ms, &mut report).await?;
    apply_downstreams(db, document, now_ms, &mut report).await?;

    Ok(report)
}

/// Restore the **Downstream** roster's shape (#52).
///
/// Matched on the **URL**, which is a peer's identity here the way (label,
/// scope) is an API key's: two entries for one address are one peer configured
/// twice, and a restore run twice must not end with two. An existing peer keeps
/// its stored key and its enabled state — a restore is not a reason to switch
/// off a peer that is working — and only a genuinely new one is created, always
/// disabled, because it has no credential yet.
async fn apply_downstreams<C: ConnectionTrait>(
    db: &C,
    document: &Document,
    now_ms: i64,
    report: &mut DocumentReport,
) -> Result<(), DbErr> {
    let mut held: HashSet<String> = crate::db::entities::downstream::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|row| row.url)
        .collect();

    for entry in &document.downstreams {
        let Some(url) = optional_text(Some(entry.url.clone())) else {
            continue;
        };
        if !held.insert(url.clone()) {
            continue;
        }
        report.downstreams_to_key += 1;
        crate::db::entities::downstream::ActiveModel {
            label: Set(optional_text(entry.label.clone())),
            url: Set(url),
            // Nothing to put here, which is exactly why the row arrives off.
            api_key: Set(String::new()),
            scope: Set(super::downstreams::scope_json(&entry.scope)),
            disabled: Set(true),
            consecutive_failures: Set(0),
            created_at_ms: Set(now_ms),
            ..Default::default()
        }
        .insert(db)
        .await?;
    }
    Ok(())
}

/// Upsert one System and everything addressed under it.
async fn apply_system<C: ConnectionTrait>(
    db: &C,
    entry: &SystemEntry,
    at: &str,
    now_ms: i64,
    report: &mut DocumentReport,
) -> Result<(), DbErr> {
    let existing = system::Entity::find()
        .filter(system::Column::Ref.eq(entry.r#ref))
        .one(db)
        .await?;
    let blacklist = blacklist_text(&entry.blacklist);
    let label = optional_text(entry.label.clone());

    let system = match existing {
        None => {
            report.systems.created += 1;
            system::ActiveModel {
                r#ref: Set(entry.r#ref),
                label: Set(label.clone()),
                auto_populate: Set(entry.auto_populate),
                blacklist: Set(blacklist),
                enhancement: Set(entry.enhancement),
                created_at_ms: Set(now_ms),
                ..Default::default()
            }
            .insert(db)
            .await?
        }
        Some(found) => {
            let same = found.label == label
                && found.auto_populate == entry.auto_populate
                && found.blacklist == blacklist
                && found.enhancement == entry.enhancement;
            match same {
                true => {
                    report.systems.unchanged += 1;
                    found
                }
                false => {
                    report.systems.updated += 1;
                    let mut row = found.into_active_model();
                    row.label = Set(label);
                    row.auto_populate = Set(entry.auto_populate);
                    row.blacklist = Set(blacklist);
                    row.enhancement = Set(entry.enhancement);
                    row.update(db).await?
                }
            }
        }
    };

    // Every channel first, then every merge — folding a Ref that this same
    // document also lists as a channel must find that channel already there, or
    // the fold would record a bare member Ref and the row would arrive after it
    // as a duplicate.
    let mut owners: HashMap<i64, talkgroup::Model> = HashMap::new();
    for (index, channel) in entry.talkgroups.iter().enumerate() {
        let at = format!("{at}.talkgroups[{index}]");
        if let Some(row) = apply_talkgroup(db, system.id, channel, &at, now_ms, report).await? {
            owners.insert(channel.r#ref, row);
        }
    }
    for (index, channel) in entry.talkgroups.iter().enumerate() {
        let Some(owner) = owners.get(&channel.r#ref) else {
            continue;
        };
        let at = format!("{at}.talkgroups[{index}]");
        apply_members(db, owner, channel, &at, now_ms, report).await?;
        apply_tones(db, owner, channel, &at, now_ms, report).await?;
    }

    for (index, radio) in entry.units.iter().enumerate() {
        let at = format!("{at}.units[{index}]");
        apply_unit(db, system.id, radio, &at, now_ms, report).await?;
    }
    Ok(())
}

/// Upsert one Talkgroup to exactly what the entry says, returning **the row** —
/// or `None` if the entry was refused.
///
/// The row rather than its id, because the merge pass below needs the model and
/// every arm here already has one: reading it back would be a query per channel
/// on a county restore, and an unreachable "it is not there" arm to leave
/// untested.
async fn apply_talkgroup<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    entry: &TalkgroupEntry,
    at: &str,
    now_ms: i64,
    report: &mut DocumentReport,
) -> Result<Option<talkgroup::Model>, DbErr> {
    let led = match checked_led(entry.led.clone()) {
        Ok(led) => led,
        Err(rejected) => {
            report.rejected.push(refused(at, &rejected));
            return Ok(None);
        }
    };
    let label = optional_text(entry.label.clone());
    let name = optional_text(entry.name.clone());
    let groups: Vec<String> = entry
        .groups
        .iter()
        .filter_map(|name| optional_text(Some(name.clone())))
        .collect();
    let tag_id = match optional_text(entry.tag.clone()) {
        Some(tag) => {
            let (row, created) = repo::ensure_tag(db, &tag, now_ms).await?;
            report.tags_created += u64::from(created);
            Some(row.id)
        }
        None => None,
    };

    let existing = talkgroup::Entity::find()
        .filter(talkgroup::Column::SystemId.eq(system_id))
        .filter(talkgroup::Column::Ref.eq(entry.r#ref))
        .one(db)
        .await?;
    let row = match existing {
        None => {
            report.talkgroups.created += 1;
            talkgroup::ActiveModel {
                system_id: Set(system_id),
                r#ref: Set(entry.r#ref),
                label: Set(label.clone()),
                name: Set(name.clone()),
                tag_id: Set(tag_id),
                led: Set(led),
                enhancement: Set(entry.enhancement),
                created_at_ms: Set(now_ms),
                ..Default::default()
            }
            .insert(db)
            .await?
        }
        Some(found) => {
            let same = found.label == label
                && found.name == name
                && found.tag_id == tag_id
                && found.led == led
                && found.enhancement == entry.enhancement
                && stored_groups(db, found.id).await? == sorted(&groups);
            match same {
                true => {
                    report.talkgroups.unchanged += 1;
                    found
                }
                false => {
                    report.talkgroups.updated += 1;
                    let mut row = found.into_active_model();
                    row.label = Set(label);
                    row.name = Set(name);
                    row.tag_id = Set(tag_id);
                    row.led = Set(led);
                    row.enhancement = Set(entry.enhancement);
                    row.update(db).await?
                }
            }
        }
    };

    report.groups_created += repo::set_talkgroup_groups(db, row.id, &groups, now_ms).await?;
    Ok(Some(row))
}

/// Make this channel's member Refs exactly what the entry says.
async fn apply_members<C: ConnectionTrait>(
    db: &C,
    owner: &talkgroup::Model,
    entry: &TalkgroupEntry,
    at: &str,
    now_ms: i64,
    report: &mut DocumentReport,
) -> Result<(), DbErr> {
    if entry.member_refs.is_empty() && repo::member_refs_of(db, owner.id).await?.is_empty() {
        return Ok(());
    }

    // The same refusal the browser and the CSV both make: a Ref another channel
    // holds as a member is spoken for, and stealing it would break that channel
    // to fix this one.
    for wanted in &entry.member_refs {
        if repo::member_ref_held_elsewhere(
            db,
            owner.system_id,
            Some(owner.id),
            *wanted,
            &entry.member_refs,
        )
        .await?
        {
            report.rejected.push(RejectedEntry {
                at: at.to_owned(),
                reason: "member-ref-owned-elsewhere",
                detail: format!("another talkgroup already answers to {wanted}"),
            });
            return Ok(());
        }
    }

    let change = repo::set_member_refs(db, owner, &entry.member_refs, now_ms).await?;
    change.record(owner.id, owner.r#ref, report.dry_run);
    Ok(())
}

/// Upsert this channel's **Tone profiles** (#55), keyed by label.
///
/// **Absence never deletes**, which is the document's own rule: a file written
/// by a release that had no `tones` key must not silently unwatch every station
/// on the channel it names.
///
/// A profile that could never fire is *reported* rather than refusing the
/// document — the [`RejectedEntry`] convention — so a restore of four hundred
/// channels is not lost to one typo, and the Operator is told which entry to fix
/// by its path into the file.
async fn apply_tones<C: ConnectionTrait>(
    db: &C,
    owner: &talkgroup::Model,
    entry: &TalkgroupEntry,
    at: &str,
    now_ms: i64,
    report: &mut DocumentReport,
) -> Result<(), DbErr> {
    for (index, profile) in entry.tones.iter().enumerate() {
        let at = format!("{at}.tones[{index}]");
        if let Some(detail) =
            crate::tone::unusable(&profile.steps, profile.tolerance_pct, profile.gap_max_ms)
        {
            report.rejected.push(RejectedEntry {
                at,
                reason: "unusable-tone-profile",
                detail,
            });
            continue;
        }
        let steps = crate::tone::steps_json(&profile.steps);
        let existing = tone_profile::Entity::find()
            .filter(tone_profile::Column::TalkgroupId.eq(owner.id))
            .filter(tone_profile::Column::Label.eq(profile.label.clone()))
            .one(db)
            .await?;
        match existing {
            None => {
                report.tones.created += 1;
                tone_profile::ActiveModel {
                    talkgroup_id: Set(owner.id),
                    label: Set(profile.label.clone()),
                    tolerance_pct: Set(profile.tolerance_pct),
                    gap_max_ms: Set(profile.gap_max_ms),
                    steps: Set(steps),
                    disabled: Set(profile.disabled),
                    created_at_ms: Set(now_ms),
                    ..Default::default()
                }
                .insert(db)
                .await?;
            }
            Some(found)
                if found.steps == steps
                    && found.tolerance_pct == profile.tolerance_pct
                    && found.gap_max_ms == profile.gap_max_ms
                    && found.disabled == profile.disabled =>
            {
                report.tones.unchanged += 1;
            }
            Some(found) => {
                report.tones.updated += 1;
                let mut row = found.into_active_model();
                row.steps = Set(steps);
                row.tolerance_pct = Set(profile.tolerance_pct);
                row.gap_max_ms = Set(profile.gap_max_ms);
                row.disabled = Set(profile.disabled);
                row.update(db).await?;
            }
        }
    }
    Ok(())
}

/// Upsert one Unit and its Ranges.
async fn apply_unit<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    entry: &UnitEntry,
    at: &str,
    now_ms: i64,
    report: &mut DocumentReport,
) -> Result<(), DbErr> {
    let existing = unit::Entity::find()
        .filter(unit::Column::SystemId.eq(system_id))
        .filter(unit::Column::Ref.eq(entry.r#ref))
        .one(db)
        .await?;
    let label = optional_text(entry.label.clone());
    let wanted: Vec<Range> = entry.ranges.iter().copied().map(Span::range).collect();

    let unit = match existing {
        None => {
            report.units.created += 1;
            unit::ActiveModel {
                system_id: Set(system_id),
                r#ref: Set(entry.r#ref),
                label: Set(label.clone()),
                created_at_ms: Set(now_ms),
                ..Default::default()
            }
            .insert(db)
            .await?
        }
        Some(found) => {
            let held = repo::ranges_of(db, found.id).await?;
            match found.label == label && held == wanted {
                true => {
                    report.units.unchanged += 1;
                    found
                }
                false => {
                    report.units.updated += 1;
                    let mut row = found.into_active_model();
                    row.label = Set(label);
                    row.update(db).await?
                }
            }
        }
    };

    let set = repo::set_unit_ranges(db, &unit, &wanted, now_ms).await?;
    for (wanted, held) in set.refused {
        report
            .rejected
            .push(refused(at, &Rejected::RangeOverlaps { wanted, held }));
    }
    Ok(())
}

/// Re-issue the roster's keys, skipping any this Instance already has.
///
/// Matched on **(label, scope)**, which is everything a key row is once its
/// secret is gone — so a second import of the same document issues nothing and a
/// half-finished restore is safe to retry. Two keys sharing a label and a scope
/// are indistinguishable here, which is the other reason the export is worth
/// labelling.
async fn apply_keys<C: ConnectionTrait>(
    db: &C,
    document: &Document,
    dry_run: bool,
    now_ms: i64,
    report: &mut DocumentReport,
) -> Result<(), DbErr> {
    let mut held: HashSet<(Option<String>, Option<i64>)> = api_key::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|row| (row.label, row.system_ref))
        .collect();

    for entry in &document.api_keys {
        let label = optional_text(entry.label.clone());
        if !held.insert((label.clone(), entry.system_ref)) {
            continue;
        }
        report.api_keys_to_issue += 1;
        // **A preview mints nothing.** Showing a secret and then rolling it back
        // would leave an Operator holding a key that never existed, which is
        // worse than showing none.
        if dry_run {
            continue;
        }
        let key = uuid::Uuid::new_v4().simple().to_string();
        let row =
            repo::create_api_key(db, &key, entry.system_ref, entry.label.clone(), now_ms).await?;
        let mut row = super::keys::issued(key, row);
        if entry.disabled {
            let stored = api_key::ActiveModel {
                id: Set(row.row.id),
                disabled: Set(true),
                ..Default::default()
            }
            .update(db)
            .await?;
            row = super::keys::issued(row.key, stored);
        }
        report.api_keys.push(row);
    }
    Ok(())
}

// -- The parts they are made of ---------------------------------------------

/// A refusal, placed in the document.
fn refused(at: &str, rejected: &Rejected) -> RejectedEntry {
    RejectedEntry {
        at: at.to_owned(),
        reason: rejected.slug(),
        detail: rejected.to_string(),
    }
}

/// A copy of `names`, sorted — the shape [`stored_groups`] answers in, so the
/// "already right" comparison is between two lists in one order.
fn sorted(names: &[String]) -> Vec<String> {
    let mut sorted = names.to_vec();
    sorted.sort();
    sorted
}

/// The Group names a Talkgroup is in, sorted — for the unchanged comparison.
async fn stored_groups<C: ConnectionTrait>(
    db: &C,
    talkgroup_id: i64,
) -> Result<Vec<String>, DbErr> {
    let mut names: Vec<String> = talkgroup_group::Entity::find()
        .filter(talkgroup_group::Column::TalkgroupId.eq(talkgroup_id))
        .find_also_related(group::Entity)
        .all(db)
        .await?
        .into_iter()
        .filter_map(|(_, group)| group.map(|row| row.name))
        .collect();
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// The filename carries the date and nothing else that varies, so two
    /// backups do not collide and the *document* stays a pure function of the
    /// configuration.
    #[rstest]
    #[case(0, "radio-scout-config-1970-01-01.json")]
    #[case(1_755_000_000_000, "radio-scout-config-2025-08-12.json")]
    fn the_download_is_named_for_the_day_it_was_taken(#[case] now_ms: i64, #[case] expected: &str) {
        assert_eq!(filename(now_ms), expected);
    }

    /// An instant no calendar can render still exports — a backup is not worth
    /// failing over the name of its file.
    #[test]
    fn an_unreadable_instant_still_names_a_file() {
        assert_eq!(filename(i64::MIN), "radio-scout-config.json");
    }

    /// **A document says what it is.** An empty Instance still exports a
    /// versioned document rather than an empty object, so a restore of nothing
    /// is a restore rather than a parse error.
    #[test]
    fn an_empty_document_still_carries_its_version() {
        let document = Document {
            version: VERSION,
            systems: Vec::new(),
            groups: Vec::new(),
            tags: Vec::new(),
            api_keys: Vec::new(),
            downstreams: Vec::new(),
        };

        assert_eq!(
            serde_json::to_value(&document).expect("serialize"),
            serde_json::json!({"version": 1, "systems": []})
        );
    }

    /// The whole document round-trips through JSON, which is the property the
    /// export/import pair is: what one writes the other must read.
    #[test]
    fn a_document_reads_back_as_itself() {
        let document = Document {
            version: VERSION,
            systems: vec![SystemEntry {
                r#ref: 11,
                label: Some(String::from("Fulton")),
                auto_populate: true,
                blacklist: vec![9999],
                enhancement: Some(false),
                talkgroups: vec![TalkgroupEntry {
                    r#ref: 100,
                    label: Some(String::from("Fire Dispatch")),
                    name: None,
                    tag: Some(String::from("Fire")),
                    groups: vec![String::from("Fire")],
                    led: Some(String::from("red")),
                    enhancement: None,
                    member_refs: vec![8123],
                    tones: Vec::new(),
                }],
                units: vec![UnitEntry {
                    r#ref: 1200,
                    label: Some(String::from("Engine 1")),
                    ranges: vec![super::super::members::Span {
                        from: 1201,
                        to: 1299,
                    }],
                }],
            }],
            groups: vec![String::from("Fire")],
            tags: vec![String::from("Fire")],
            api_keys: vec![KeyEntry {
                label: Some(String::from("the pi")),
                system_ref: Some(11),
                disabled: false,
            }],
            downstreams: vec![DownstreamEntry {
                url: String::from("https://peer.example"),
                label: Some(String::from("county mirror")),
                scope: serde_json::from_value(
                    serde_json::json!({ "sel": { "11": { "*": true } } }),
                )
                .expect("a selection"),
            }],
        };

        let text = serde_json::to_string(&document).expect("serialize");
        let read: Document = serde_json::from_str(&text).expect("parse");

        assert_eq!(read, document);
    }

    /// A field this Instance does not know is refused rather than dropped: an
    /// Operator handed a document from a future version must be told, not have
    /// half of it silently ignored — which is what rdio's commented-out version
    /// check leaves it doing.
    ///
    /// The example used to be `downstreams`, and #52 made it real — which is the
    /// whole shape of this hazard, so the replacement is deliberately a field
    /// nothing in the roadmap is going to add.
    #[test]
    fn an_unknown_field_is_refused() {
        let parsed: Result<Document, _> =
            serde_json::from_str(r#"{"version": 1, "sprockets": []}"#);

        assert!(parsed.is_err());
    }

    /// **No credential leaves in a backup, in either direction** (#52). The peer
    /// issued us a key we must keep in a sendable form, which is exactly why it
    /// must not be in the file an Operator commits — and `deny_unknown_fields`
    /// means a document that tried to carry one is refused rather than silently
    /// stripped, so nothing downstream has to remember to drop it.
    #[test]
    fn a_downstream_entry_has_nowhere_to_put_a_key() {
        let entry = DownstreamEntry {
            url: String::from("https://peer.example"),
            label: None,
            scope: crate::selection::Selection::default(),
        };

        let json = serde_json::to_string(&entry).expect("serialize");
        assert!(!json.contains("key"), "{json}");

        let with_a_key: Result<DownstreamEntry, _> = serde_json::from_str(
            r#"{"url": "https://peer.example", "scope": {}, "apiKey": "s3cret"}"#,
        );
        assert!(with_a_key.is_err(), "a document carrying a key is refused");
    }
}
