//! Bulk Talkgroup curation by CSV import (#18, spec US 37).
//!
//! An operator tidies an auto-populated archive by uploading a CSV: set the
//! Talkgroup labels, names, Tags, Groups, and LED colors in one shot, without a
//! per-entity admin UI.
//!
//! # Improving on rdio-scanner
//!
//! rdio's importer (`client/.../import-talkgroups.component.ts`) reads the
//! RadioReference export positionally, splits it with a regex, and `unshift`s
//! every row onto the selected System's talkgroup list. Each of those is a bug
//! we fix, and the fixes are the reason this module exists:
//!
//! | rdio | Radio-Scout |
//! |---|---|
//! | fixed column positions | **named headers in any order**, positional as the fallback |
//! | `split(/"*,"*/)` — a quoted `"Fire, EMS"` splits into two fields | **real RFC 4180 parsing** (quotes, doubled quotes, embedded newlines, CRLF, BOM) |
//! | `unshift` — re-importing duplicates every Talkgroup | **upsert keyed by (System, Ref)** — idempotent, so re-import is safe |
//! | a blank cell overwrites with nothing | **a blank cell means "leave alone"**, never "clear" |
//! | bad rows silently vanish | **every rejected row is reported with its line number and a machine-readable reason** |
//! | no LED support | **LED colors**, validated against the client palette |
//! | one System per import, chosen in the UI | **a per-row `system` column** (Ref or label), with a request-level default |
//! | partial application on failure | **one transaction**, plus a **dry run** that walks the identical path and rolls back |
//!
//! # Layouts
//!
//! [`Layout::Named`] — a header row names the columns; order is free and
//! unknown columns (RadioReference's `Hex`, `Mode`, `Priority`, …) are ignored.
//! [`Layout::RadioReference`] — no header, so the columns are read at the
//! positions RadioReference and Trunk Recorder export them at. Every CSV that
//! imports into rdio-scanner therefore imports here unchanged.
//!
//! # Security
//!
//! **`POST /api/admin/talkgroups/import` sits behind the admin session** (#19,
//! ADR-0008). It shipped unauthenticated — the deviation this route's
//! `/api/admin/` prefix existed to make cheap to close — and the prefix did its
//! job: one `route_layer` in [`crate::admin_routes`] now gates it and everything
//! added beside it. A caller needs a session cookie *and*, because this is a
//! state-changing `POST`, the session's CSRF token in `X-CSRF-Token`.

use std::collections::HashMap;

use axum::extract::{Query, State};

use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, Set,
};
use serde::Serialize;

use crate::AppState;
use crate::db::entities::{group, system, tag, talkgroup, talkgroup_group};
use crate::db::repo;
use crate::failure::{Failure, Reason, Stage};

/// The LED colors a Talkgroup may be painted, mirroring the client palette in
/// `client/src/lib/led.ts`. An import is the only way a Talkgroup gets one, so
/// this is where an unknown color is caught — a typo becomes a reported row, not
/// a Talkgroup that renders with no LED.
pub const LED_PALETTE: [&str; 8] = [
    "blue", "cyan", "green", "magenta", "orange", "red", "white", "yellow",
];

/// Separator for several Groups in one cell (`Fire;EMS`). A Talkgroup can belong
/// to many Groups (the `talkgroup_group` join table); rdio's importer only ever
/// assigned one.
const GROUP_SEPARATOR: char = ';';

/// The UTF-8 byte-order mark a spreadsheet prefixes to its CSV exports.
const BOM: &[u8] = "\u{feff}".as_bytes();

// ---------------------------------------------------------------------------
// Parsed shapes
// ---------------------------------------------------------------------------

/// Which column layout a CSV was read with — reported back so an operator can
/// see that their file was understood the way they meant it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layout {
    /// A header row named the columns. Order is free; unknown columns ignored.
    Named,
    /// No header row — the positional RadioReference / Trunk Recorder export:
    /// `Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category`.
    RadioReference,
}

/// How a row (or the request) picks the System a Talkgroup belongs to.
///
/// A Talkgroup Ref is unique only *within* a System, so an import that didn't
/// pin the System could write the right label onto the wrong channel. Numeric
/// input is a System **Ref**; anything else is a System **label** (a Trunk
/// Recorder `short_name`, which is what an operator actually knows). A Ref may
/// create the System — an operator can curate before the first Call arrives — but
/// a label never does: inventing a Ref for a mistyped name would silently
/// scatter Talkgroups into a System nothing ingests into.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SystemSelector {
    Ref(i64),
    Label(String),
}

impl SystemSelector {
    /// Read a selector from a cell or query parameter: digits are a Ref,
    /// anything else is a label.
    pub fn parse(value: &str) -> Self {
        let value = value.trim();
        match value.parse::<i64>() {
            Ok(system_ref) => SystemSelector::Ref(system_ref),
            Err(_) => SystemSelector::Label(value.to_owned()),
        }
    }
}

/// How a selector reads back in a rejection — the operator's own spelling, so
/// the report quotes what they wrote.
impl std::fmt::Display for SystemSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SystemSelector::Ref(system_ref) => write!(f, "{system_ref}"),
            SystemSelector::Label(label) => f.write_str(label),
        }
    }
}

/// One Talkgroup's curated values, as a single CSV row asks for them.
///
/// Every optional field is `None` when the CSV didn't supply it, and `None`
/// means **leave the stored value alone**. That is what makes a narrow CSV
/// (`ref,led`) safe to import over a fully-curated archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TalkgroupEdit {
    /// 1-based line in the uploaded file, so a report points at something the
    /// operator can actually open and fix.
    pub line: u64,
    pub system: SystemSelector,
    pub talkgroup_ref: i64,
    pub label: Option<String>,
    pub name: Option<String>,
    pub tag: Option<String>,
    /// Empty means "the CSV said nothing about Groups"; non-empty **replaces**
    /// the Talkgroup's Groups, so a CSV is the whole truth for the rows it names.
    pub groups: Vec<String>,
    pub led: Option<String>,
    /// The member Refs this channel should answer to (#45). `None` is a blank
    /// cell — say nothing — and `Some` **replaces** the set, which is how an
    /// operator unmerges. `Some(vec![])` is the empty set, written `-`: a
    /// set-valued column with no way to spell "none" could fold a Ref in and
    /// never let it out again.
    pub member_refs: Option<Vec<i64>>,
}

/// Why one row was not applied. `reason` is machine-readable and stable (the
/// same kebab-case vocabulary the logging policy uses for rejected ingests).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RejectedRow {
    pub line: u64,
    pub reason: &'static str,
    /// The offending value, so the operator doesn't have to guess which cell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl RejectedRow {
    fn new(line: u64, reason: &'static str, detail: impl Into<String>) -> Self {
        RejectedRow {
            line,
            reason,
            detail: Some(detail.into()),
        }
    }
}

/// A CSV read into rows ready to apply, plus the rows that never will be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub layout: Layout,
    pub edits: Vec<TalkgroupEdit>,
    pub rejected: Vec<RejectedRow>,
}

/// A failure that condemns the whole file rather than one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// No header named a Talkgroup-Ref column, and the first row isn't
    /// positional data either — so we'd be guessing what every column meant.
    NoRefColumn,
    /// The bytes aren't readable as UTF-8 CSV. In practice this is a spreadsheet
    /// that exported as Latin-1: rdio reads those with `readAsBinaryString` and
    /// silently mangles every accented label, where we say so and change nothing.
    Malformed(String),
    /// Parsed fine, but there is not a single data row to apply.
    Empty,
}

impl ParseError {
    /// The stable, machine-readable name for the wire.
    pub fn reason(&self) -> &'static str {
        match self {
            ParseError::NoRefColumn => "no-ref-column",
            ParseError::Malformed(_) => "malformed-csv",
            ParseError::Empty => "empty-csv",
        }
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::NoRefColumn => f.write_str(
                "no talkgroup-ref column: name one (ref/decimal/tgid) in a header row, \
                 or use the positional RadioReference layout",
            ),
            ParseError::Malformed(detail) => write!(
                f,
                "could not read the CSV (is it saved as UTF-8?): {detail}"
            ),
            ParseError::Empty => f.write_str("the CSV has no data rows"),
        }
    }
}

// ---------------------------------------------------------------------------
// Column mapping
// ---------------------------------------------------------------------------

/// The columns an import understands, and the header names that reach each one.
///
/// Aliases exist so the files operators already have just work: RadioReference
/// exports (`Decimal`, `Alpha Tag`, `Description`, `Category`), Trunk Recorder
/// talkgroup files, and the `ref,label,group,tag,led` shape the ticket names.
/// Matching is done on a normalized name (lowercased, non-alphanumerics
/// dropped), so `Alpha Tag`, `alpha_tag`, and `AlphaTag` are one thing.
const COLUMN_ALIASES: [(Column, &[&str]); 8] = [
    (
        Column::Ref,
        &["ref", "talkgroupref", "talkgroup", "tgid", "decimal", "dec"],
    ),
    (Column::Label, &["label", "alphatag", "alpha"]),
    (Column::Name, &["name", "description", "desc"]),
    (Column::Tag, &["tag", "servicetag", "servicetype"]),
    (Column::Group, &["group", "groups", "category", "cat"]),
    (
        Column::Led,
        &["led", "ledcolor", "ledcolour", "color", "colour"],
    ),
    (
        Column::System,
        &["system", "systemref", "sys", "shortname", "sysid"],
    ),
    (
        Column::MemberRefs,
        &["memberrefs", "memberref", "members", "mergedrefs", "merged"],
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Column {
    Ref,
    Label,
    Name,
    Tag,
    Group,
    Led,
    System,
    /// The other Refs this channel answers to (#45). No RadioReference export
    /// has one, so it is a named-header column only — which is right: a merge is
    /// this instance's curation, not something a third party's file knows about.
    MemberRefs,
}

/// Field positions in the headerless RadioReference / Trunk Recorder export:
/// `Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category`. These are exactly the
/// indices rdio-scanner reads, so its imports carry over byte for byte.
const RADIO_REFERENCE_COLUMNS: [(Column, usize); 5] = [
    (Column::Ref, 0),
    (Column::Label, 2),
    (Column::Name, 4),
    (Column::Tag, 5),
    (Column::Group, 6),
];

/// Lowercase and strip everything that isn't a letter or digit, so header
/// spelling (`Alpha Tag` / `alpha_tag` / `ALPHATAG`) stops mattering.
fn normalize_header(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Map a header record to column positions, or `None` for each column the
/// header doesn't name. The first spelling of a column wins, so a stray later
/// duplicate can't hijack it.
fn map_headers(record: &csv::StringRecord) -> HashMap<Column, usize> {
    let mut mapped = HashMap::new();
    for (index, raw) in record.iter().enumerate() {
        let normalized = normalize_header(raw);
        if normalized.is_empty() {
            continue;
        }
        for (column, aliases) in COLUMN_ALIASES {
            if aliases.contains(&normalized.as_str()) {
                mapped.entry(column).or_insert(index);
            }
        }
    }
    mapped
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Read CSV text into the Talkgroup edits it asks for.
///
/// `default_system` is the request-level fallback (`?system=`) for rows that
/// carry no System of their own — which is every row of a RadioReference export,
/// since that format has no System column.
///
/// Row-level problems become [`RejectedRow`]s rather than aborting: an operator
/// fixes the named lines and re-imports, and the upsert makes that safe. Only a
/// file we can't read at all is an `Err`.
pub fn parse(bytes: &[u8], default_system: Option<&SystemSelector>) -> Result<Parsed, ParseError> {
    // A UTF-8 BOM is what a spreadsheet writes on export; it would otherwise
    // glue itself to the first header name (or the first Ref) and break it.
    let bytes = bytes.strip_prefix(BOM).unwrap_or(bytes);

    let mut reader = csv::ReaderBuilder::new()
        // Headers are detected below, not by the reader: which layout we're in
        // is only decidable after looking at the first record.
        .has_headers(false)
        // Real files have ragged rows; a short row means "no value here", which
        // is a per-row decision, not a parse failure.
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(bytes);

    // Where every line begins, so a record's byte offset can be turned into the
    // line number an operator sees in their editor. `csv`'s own `Position::line`
    // does not count the blank lines it skips, which would make every number
    // after the first blank line point at the wrong row.
    // Line 1 starts at 0; every later line starts just past a newline.
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(
            bytes
                .iter()
                .enumerate()
                .filter(|&(_, &byte)| byte == b'\n')
                .map(|(index, _)| index + 1),
        )
        .collect();
    let line_of = |record: &csv::StringRecord| -> u64 {
        // The reported offset sits *before* any blank lines the reader skipped,
        // so walk forward over them to the record's real first byte.
        let mut byte = record.position().map_or(0, |position| position.byte()) as usize;
        while matches!(bytes.get(byte), Some(b'\n' | b'\r')) {
            byte += 1;
        }
        // The line whose start is the last one at or before the record.
        //
        // The loop above leaves `byte` on a non-newline (or past the end), so
        // no line start ever equals a newline's own index. That makes
        // `start <= byte` over line starts and `newline < byte` over newline
        // indices the same partition — cargo-mutants reports the difference as
        // a surviving mutant, and it is an equivalent one, not a missing test.
        debug_assert!(!matches!(bytes.get(byte), Some(b'\n')));
        line_starts.partition_point(|&start| start <= byte) as u64
    };

    let mut records = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|err| ParseError::Malformed(err.to_string()))?;
        // A row of nothing but separators is punctuation, not a failed attempt
        // at data — skip it silently rather than filling the report with noise.
        if record.iter().all(|field| field.trim().is_empty()) {
            continue;
        }
        let line = line_of(&record);
        records.push((line, record));
    }

    let mut records = records.into_iter();
    let Some((first_line, first)) = records.next() else {
        return Err(ParseError::Empty);
    };

    // Layout detection, in the only order that can't misread a file:
    //  1. If the first record *names* a Ref column, it's a header.
    //  2. Otherwise, if its first field is a number, it's positional data.
    //  3. Otherwise we would be guessing — say so instead.
    let (layout, columns, first_data) = {
        let named = map_headers(&first);
        if named.contains_key(&Column::Ref) {
            (Layout::Named, named, None)
        } else if first
            .get(0)
            .is_some_and(|f| f.trim().parse::<i64>().is_ok())
        {
            let positional = RADIO_REFERENCE_COLUMNS.into_iter().collect();
            (
                Layout::RadioReference,
                positional,
                Some((first_line, first)),
            )
        } else {
            return Err(ParseError::NoRefColumn);
        }
    };

    let mut parsed = Parsed {
        layout,
        edits: Vec::new(),
        rejected: Vec::new(),
    };
    for (line, record) in first_data.into_iter().chain(records) {
        match parse_row(&record, &columns, line, default_system) {
            Ok(edit) => parsed.edits.push(edit),
            Err(rejected) => parsed.rejected.push(rejected),
        }
    }

    if parsed.edits.is_empty() && parsed.rejected.is_empty() {
        return Err(ParseError::Empty);
    }
    Ok(parsed)
}

/// Read one data record into an edit, or say why it can't be one.
fn parse_row(
    record: &csv::StringRecord,
    columns: &HashMap<Column, usize>,
    line: u64,
    default_system: Option<&SystemSelector>,
) -> Result<TalkgroupEdit, RejectedRow> {
    // A cell the row is too short to have, or that holds only whitespace, is a
    // cell the CSV said nothing about.
    let cell = |column: Column| -> Option<&str> {
        columns
            .get(&column)
            .and_then(|&index| record.get(index))
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };

    let Some(raw_ref) = cell(Column::Ref) else {
        return Err(RejectedRow {
            line,
            reason: "missing-ref",
            detail: None,
        });
    };
    let talkgroup_ref = raw_ref
        .parse::<i64>()
        .map_err(|_| RejectedRow::new(line, "ref-not-a-number", raw_ref))?;

    let system = match cell(Column::System) {
        Some(raw) => SystemSelector::parse(raw),
        None => default_system.cloned().ok_or(RejectedRow {
            line,
            reason: "no-system",
            detail: None,
        })?,
    };

    // An unrecognized LED rejects the row rather than being dropped: silently
    // importing everything *except* the color the operator came for is exactly
    // the kind of quiet half-success this importer exists to avoid.
    let led = match cell(Column::Led) {
        Some(raw) => {
            let color = raw.to_lowercase();
            if !LED_PALETTE.contains(&color.as_str()) {
                return Err(RejectedRow::new(line, "led-not-in-palette", raw));
            }
            Some(color)
        }
        None => None,
    };

    let groups = cell(Column::Group)
        .map(|raw| {
            raw.split(GROUP_SEPARATOR)
                .map(str::trim)
                .filter(|g| !g.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();

    // `-` is rdio's placeholder token, and here it is the one column where it
    // is a *value*: "this channel answers to no other Refs". Everywhere else a
    // placeholder means silence, and a blank cell still does mean silence here —
    // the two are different sentences and an unmerge needs the first one.
    let member_refs = match cell(Column::MemberRefs) {
        None => None,
        Some("-") => Some(Vec::new()),
        Some(raw) => {
            let mut refs = Vec::new();
            for entry in raw.split(GROUP_SEPARATOR).map(str::trim) {
                if entry.is_empty() {
                    continue;
                }
                // A row that half-applies its merge is worse than one that does
                // not apply at all: the operator would have to work out which of
                // their Refs landed. Reject the row, name the cell.
                let member = entry
                    .parse::<i64>()
                    .map_err(|_| RejectedRow::new(line, "member-ref-not-a-number", entry))?;
                refs.push(member);
            }
            Some(refs)
        }
    };

    Ok(TalkgroupEdit {
        line,
        system,
        talkgroup_ref,
        label: cell(Column::Label).map(str::to_owned),
        name: cell(Column::Name).map(str::to_owned),
        tag: cell(Column::Tag).map(str::to_owned),
        groups,
        led,
        member_refs,
    })
}

// ---------------------------------------------------------------------------
// Applying
// ---------------------------------------------------------------------------

/// What an import did (or, on a dry run, would have done).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub layout: Layout,
    /// True when nothing was written — the same work, rolled back.
    pub dry_run: bool,
    /// Data rows read, rejected ones included (blank lines and the header not).
    pub rows: u64,
    pub talkgroups_created: u64,
    pub talkgroups_updated: u64,
    /// Rows whose values the archive already had — the count that proves a
    /// re-import is a no-op.
    pub talkgroups_unchanged: u64,
    /// Systems the import brought into existence. Non-zero on a run you
    /// expected to only touch known Systems means a mistyped `system` column.
    pub systems_created: u64,
    pub tags_created: u64,
    pub groups_created: u64,
    /// Talkgroups that were folded into another and stopped existing in their
    /// own right (#45).
    pub talkgroups_folded: u64,
    /// Member Refs that became Talkgroups again.
    pub talkgroups_unfolded: u64,
    /// Archived Calls whose owning Talkgroup a merge moved, in either direction.
    /// The number a dry run exists to show before it is real: this is the one
    /// curation act that rewrites history rather than configuration.
    pub calls_repointed: u64,
    /// Every row that was not applied, in file order.
    pub rejected: Vec<RejectedRow>,
}

// How a report reaches an operator. The refused case is JSON too
// ([`crate::failure::Reason::BadImport`]), so the client renders one shape
// whichever way the import went.
crate::answers_json!(ImportReport);

/// How the caller wants the import run.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// The System for rows that name none.
    pub default_system: Option<SystemSelector>,
    /// Walk the whole import and roll it back, reporting exactly what a real
    /// run would do. rdio has no equivalent — you find out by looking afterwards.
    pub dry_run: bool,
}

/// A CSV import that never started.
#[derive(Debug)]
pub enum ImportError {
    Parse(ParseError),
    Db(DbErr),
}

impl From<DbErr> for ImportError {
    fn from(err: DbErr) -> Self {
        ImportError::Db(err)
    }
}

/// Parse and apply a CSV in one transaction.
///
/// Valid rows apply together or not at all; rejected rows are reported and never
/// applied. A dry run walks the identical path and rolls back, so its report is
/// a promise about the real run rather than a separate estimate of it.
pub async fn import(
    db: &crate::db::Db,
    bytes: &[u8],
    options: &ImportOptions,
    now_ms: i64,
) -> Result<ImportReport, ImportError> {
    let parsed = parse(bytes, options.default_system.as_ref()).map_err(ImportError::Parse)?;

    let txn = db.begin().await?;
    let report = apply(&txn, &parsed, options.dry_run, now_ms).await?;
    if options.dry_run {
        txn.rollback().await?;
    } else {
        txn.commit().await?;
    }
    Ok(report)
}

/// Apply parsed edits to the database, accumulating the report.
///
/// Split out from [`import`] so the transaction boundary — and therefore the
/// commit-or-roll-back choice a dry run turns on — belongs to the caller rather
/// than being buried in the upsert.
async fn apply<C: ConnectionTrait>(
    db: &C,
    parsed: &Parsed,
    dry_run: bool,
    now_ms: i64,
) -> Result<ImportReport, DbErr> {
    let mut report = ImportReport {
        layout: parsed.layout,
        dry_run,
        rows: (parsed.edits.len() + parsed.rejected.len()) as u64,
        talkgroups_created: 0,
        talkgroups_updated: 0,
        talkgroups_unchanged: 0,
        systems_created: 0,
        tags_created: 0,
        groups_created: 0,
        talkgroups_folded: 0,
        talkgroups_unfolded: 0,
        calls_repointed: 0,
        rejected: parsed.rejected.clone(),
    };

    // A CSV names the same handful of Systems, Tags, and Groups on row after
    // row. Resolving each one once keeps the query count proportional to the
    // *distinct* names rather than to the row count — the difference between a
    // few hundred and a few thousand statements on a Pi for a full county list.
    let mut resolver = Resolver::default();

    for edit in &parsed.edits {
        let Some(system_id) = resolver
            .system(db, &edit.system, now_ms, &mut report)
            .await?
        else {
            report.rejected.push(RejectedRow::new(
                edit.line,
                "unknown-system",
                edit.system.to_string(),
            ));
            continue;
        };
        apply_edit(db, system_id, edit, now_ms, &mut resolver, &mut report).await?;
    }

    // File order, so a report reads alongside the file it came from. A stable
    // sort keeps same-line rejections in the order they were found.
    report.rejected.sort_by_key(|rejected| rejected.line);
    Ok(report)
}

/// Per-import memo of the entities rows keep re-naming.
#[derive(Default)]
struct Resolver {
    /// `None` marks a System that is known *not* to exist (an unresolvable
    /// label), so a thousand rows naming it cost one query, not a thousand.
    systems: HashMap<SystemSelector, Option<i64>>,
    tags: HashMap<String, i64>,
    groups: HashMap<String, i64>,
}

impl Resolver {
    /// Resolve a row's System to its id. A Ref may create it — curating ahead of
    /// the first Call is a legitimate workflow, and the report says how many
    /// Systems that made, so a mistyped column is visible. A label may not; see
    /// [`SystemSelector`].
    async fn system<C: ConnectionTrait>(
        &mut self,
        db: &C,
        selector: &SystemSelector,
        now_ms: i64,
        report: &mut ImportReport,
    ) -> Result<Option<i64>, DbErr> {
        if let Some(cached) = self.systems.get(selector) {
            return Ok(*cached);
        }
        let resolved = match selector {
            SystemSelector::Ref(system_ref) => {
                let existing = system::Entity::find()
                    .filter(system::Column::Ref.eq(*system_ref))
                    .one(db)
                    .await?;
                match existing {
                    Some(found) => Some(found.id),
                    None => {
                        report.systems_created += 1;
                        Some(
                            repo::resolve_or_create_system(
                                db,
                                *system_ref,
                                Some(format!("System {system_ref}")),
                                now_ms,
                            )
                            .await?
                            .id,
                        )
                    }
                }
            }
            SystemSelector::Label(label) => system::Entity::find()
                .filter(system::Column::Label.eq(label.as_str()))
                .one(db)
                .await?
                .map(|found| found.id),
        };
        self.systems.insert(selector.clone(), resolved);
        Ok(resolved)
    }

    /// Resolve a Tag by name, creating it if new and counting that.
    async fn tag<C: ConnectionTrait>(
        &mut self,
        db: &C,
        name: &str,
        now_ms: i64,
        report: &mut ImportReport,
    ) -> Result<i64, DbErr> {
        if let Some(cached) = self.tags.get(name) {
            return Ok(*cached);
        }
        let existed = tag::Entity::find()
            .filter(tag::Column::Name.eq(name))
            .one(db)
            .await?;
        let id = match existed {
            Some(found) => found.id,
            None => {
                report.tags_created += 1;
                repo::resolve_or_create_tag(db, name, now_ms).await?.id
            }
        };
        self.tags.insert(name.to_owned(), id);
        Ok(id)
    }

    /// Resolve a Group by name, creating it if new and counting that.
    async fn group<C: ConnectionTrait>(
        &mut self,
        db: &C,
        name: &str,
        now_ms: i64,
        report: &mut ImportReport,
    ) -> Result<i64, DbErr> {
        if let Some(cached) = self.groups.get(name) {
            return Ok(*cached);
        }
        let existed = group::Entity::find()
            .filter(group::Column::Name.eq(name))
            .one(db)
            .await?;
        let id = match existed {
            Some(found) => found.id,
            None => {
                report.groups_created += 1;
                repo::resolve_or_create_group(db, name, now_ms).await?.id
            }
        };
        self.groups.insert(name.to_owned(), id);
        Ok(id)
    }
}

/// Upsert one Talkgroup and its Groups, classifying the row as created,
/// updated, or unchanged — exactly one of the three.
async fn apply_edit<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    edit: &TalkgroupEdit,
    now_ms: i64,
    resolver: &mut Resolver,
    report: &mut ImportReport,
) -> Result<(), DbErr> {
    let tag_id = match &edit.tag {
        Some(name) => Some(resolver.tag(db, name, now_ms, report).await?),
        None => None,
    };

    let existing = talkgroup::Entity::find()
        .filter(talkgroup::Column::SystemId.eq(system_id))
        .filter(talkgroup::Column::Ref.eq(edit.talkgroup_ref))
        .one(db)
        .await?;

    // Both merge checks happen here, before a single field is written, because
    // the alternative is a row that renamed a channel and then refused to move
    // its Refs — an operator would have to work out which half landed.
    if let Some(rejected) = merge_conflict(db, system_id, edit, existing.as_ref()).await? {
        report.rejected.push(rejected);
        return Ok(());
    }

    // A Talkgroup this import *invents* must come out indistinguishable from one
    // the recorder would have auto-populated (#8), or it ends up with no Tag and
    // no Group — and since `insert_call` leaves an existing Talkgroup alone, the
    // first real Call would never repair it. Its Calls would then be missing from
    // the Tag/Group filter facets forever. Curating an existing Talkgroup keeps
    // using `None` = "leave alone"; only creation fills the blanks.
    let creating = existing.is_none();
    let tag_id = match (tag_id, creating) {
        (Some(id), _) => Some(id),
        (None, true) => Some(resolver.tag(db, repo::DEFAULT_TAG, now_ms, report).await?),
        (None, false) => None,
    };
    let groups: Vec<&str> = match (edit.groups.is_empty(), creating) {
        (false, _) => edit.groups.iter().map(String::as_str).collect(),
        (true, true) => vec![repo::DEFAULT_GROUP],
        (true, false) => Vec::new(),
    };

    let (talkgroup_id, created, fields_changed) = match existing {
        None => {
            let inserted = talkgroup::ActiveModel {
                system_id: Set(system_id),
                r#ref: Set(edit.talkgroup_ref),
                label: Set(edit.label.clone()),
                name: Set(edit.name.clone()),
                tag_id: Set(tag_id),
                led: Set(edit.led.clone()),
                created_at_ms: Set(now_ms),
                ..Default::default()
            }
            .insert(db)
            .await?;
            (inserted.id, true, false)
        }
        Some(found) => {
            // Only fields the CSV supplied move; a blank cell leaves the stored
            // value alone. This is what lets a two-column `ref,led` file be
            // imported over a fully-curated archive without erasing it.
            let mut update = talkgroup::ActiveModel {
                id: Set(found.id),
                ..Default::default()
            };
            let mut changed = false;
            if let Some(label) = &edit.label
                && found.label.as_deref() != Some(label.as_str())
            {
                update.label = Set(Some(label.clone()));
                changed = true;
            }
            if let Some(name) = &edit.name
                && found.name.as_deref() != Some(name.as_str())
            {
                update.name = Set(Some(name.clone()));
                changed = true;
            }
            if let Some(tag_id) = tag_id
                && found.tag_id != Some(tag_id)
            {
                update.tag_id = Set(Some(tag_id));
                changed = true;
            }
            if let Some(led) = &edit.led
                && found.led.as_deref() != Some(led.as_str())
            {
                update.led = Set(Some(led.clone()));
                changed = true;
            }
            if changed {
                update.update(db).await?;
            }
            (found.id, false, changed)
        }
    };

    // Groups are a set, and a non-empty cell *is* that set: an import is the
    // authority on the rows it names, so re-importing a Talkgroup with fewer
    // Groups is how one gets removed from a Group. An empty cell said nothing —
    // except on creation, where `groups` above holds the auto-populate default.
    let groups_changed = if groups.is_empty() {
        false
    } else {
        replace_groups(db, talkgroup_id, &groups, now_ms, resolver, report).await?
    };

    // Member Refs are a set on the same terms (#45), and the same sentence: a
    // non-empty cell is the whole truth for the row it names, so dropping a Ref
    // from the list is how an operator unmerges. `-` is the empty set.
    //
    // Done last, because folding deletes the Talkgroup a Ref used to name and
    // everything above wants the row it is curating to still be there.
    let merged = match &edit.member_refs {
        None => repo::MergeChange::default(),
        Some(wanted) => {
            let owner = talkgroup::Entity::find_by_id(talkgroup_id)
                .one(db)
                .await?
                .ok_or_else(|| DbErr::Custom("the Talkgroup just upserted is gone".into()))?;
            repo::set_member_refs(db, &owner, wanted, now_ms).await?
        }
    };
    if !merged.is_empty() {
        // A merge is the one curation act that rewrites the *archive* rather
        // than the configuration, so it leaves a line an operator reading
        // journald can find — the report only ever reaches whoever posted the
        // CSV (ADR-0011 rule 7: a notable normal event is INFO).
        tracing::info!(
            talkgroup_ref = edit.talkgroup_ref,
            folded = merged.folded,
            unfolded = merged.unfolded,
            calls_repointed = merged.calls_repointed,
            dry_run = report.dry_run,
            "talkgroup member Refs changed"
        );
    }
    report.talkgroups_folded += merged.folded;
    report.talkgroups_unfolded += merged.unfolded;
    report.calls_repointed += merged.calls_repointed;

    if created {
        report.talkgroups_created += 1;
    } else if fields_changed || groups_changed || !merged.is_empty() {
        report.talkgroups_updated += 1;
    } else {
        report.talkgroups_unchanged += 1;
    }
    Ok(())
}

/// Why this row's merge cannot be applied, if it cannot.
///
/// Two refusals, both of which exist because the quiet alternative is worse than
/// a reported row:
///
/// - **The row's own Ref is somebody's member Ref.** A county export still
///   listing a Ref an operator folded away would otherwise unfold it on the next
///   re-import — undoing curation by accident, which is precisely the class of
///   bug this importer was written to stop (#18). The operator is sent to the
///   owner's row, where an unmerge is something they wrote down on purpose.
/// - **A wanted member Ref belongs to another channel's member list.** Stealing
///   it would silently break the other channel to fix this one. A Ref that is
///   another Talkgroup's *primary* Ref is not a conflict at all — absorbing one
///   is exactly what a fold is, and neither is one held by a channel this same
///   row is absorbing, since it arrives here with it.
/// - **A wanted Ref names a channel that owns member Refs the cell does not.**
///   Carrying them across silently would leave the file no longer describing
///   what it made, and re-importing it would read them as members the operator
///   had removed. Refused, naming them, so the cell can say so.
async fn merge_conflict<C: ConnectionTrait>(
    db: &C,
    system_id: i64,
    edit: &TalkgroupEdit,
    existing: Option<&talkgroup::Model>,
) -> Result<Option<RejectedRow>, DbErr> {
    // Only worth asking when the Ref names no Talkgroup: primary and member Refs
    // are disjoint, so a Ref that is one cannot be the other.
    if existing.is_none()
        && let Some(owner) = repo::member_ref_owner(db, system_id, edit.talkgroup_ref).await?
        && let Some(channel) = talkgroup::Entity::find_by_id(owner.talkgroup_id)
            .one(db)
            .await?
    {
        return Ok(Some(RejectedRow::new(
            edit.line,
            "ref-is-a-member-ref",
            format!(
                "{} is a member Ref of Talkgroup {}; unmerge it from that row",
                edit.talkgroup_ref, channel.r#ref
            ),
        )));
    }

    for wanted in edit.member_refs.iter().flatten() {
        if *wanted == edit.talkgroup_ref {
            continue;
        }
        if let Some(owner) = repo::member_ref_owner(db, system_id, *wanted).await?
            && Some(owner.talkgroup_id) != existing.map(|found| found.id)
            // ...unless the channel holding it is one this very row is folding
            // in, in which case the Ref arrives here with it. That is not a
            // conflict, it is the chain fold the guard below insists on
            // spelling out.
            && !is_being_absorbed(db, owner.talkgroup_id, edit).await?
        {
            return Ok(Some(RejectedRow::new(
                edit.line,
                "member-ref-owned-elsewhere",
                wanted.to_string(),
            )));
        }
        // Folding a channel that owns member Refs of its own would silently
        // carry them onto this one, and the cell would then no longer describe
        // the archive it made: it would say `100` where the channel answered to
        // `100` and `8123`. Re-importing that same file would read the extra
        // member as one the operator had removed, and unfold it — a merge tool
        // whose own output does not round-trip. So it is refused, naming what
        // is missing, and a cell that lists them all applies cleanly.
        let Some(source) = talkgroup::Entity::find()
            .filter(talkgroup::Column::SystemId.eq(system_id))
            .filter(talkgroup::Column::Ref.eq(*wanted))
            .one(db)
            .await?
        else {
            continue;
        };
        let missing: Vec<String> = repo::member_refs_of(db, source.id)
            .await?
            .into_iter()
            .filter(|held| !edit.member_refs.iter().flatten().any(|w| w == held))
            .map(|held| held.to_string())
            .collect();
        if !missing.is_empty() {
            return Ok(Some(RejectedRow::new(
                edit.line,
                "member-ref-owns-members",
                format!(
                    "Talkgroup {wanted} answers to {}; list {} here too",
                    missing.join(", "),
                    if missing.len() == 1 { "it" } else { "them" }
                ),
            )));
        }
    }
    Ok(None)
}

/// Is `talkgroup_id` one of the channels this row's member-Refs cell folds in?
async fn is_being_absorbed<C: ConnectionTrait>(
    db: &C,
    talkgroup_id: i64,
    edit: &TalkgroupEdit,
) -> Result<bool, DbErr> {
    Ok(talkgroup::Entity::find_by_id(talkgroup_id)
        .one(db)
        .await?
        .is_some_and(|holder| {
            edit.member_refs
                .iter()
                .flatten()
                .any(|wanted| *wanted == holder.r#ref)
        }))
}

/// Make the Talkgroup's Group set exactly `names`, returning whether that
/// changed anything.
async fn replace_groups<C: ConnectionTrait>(
    db: &C,
    talkgroup_id: i64,
    names: &[&str],
    now_ms: i64,
    resolver: &mut Resolver,
    report: &mut ImportReport,
) -> Result<bool, DbErr> {
    let mut wanted = Vec::new();
    for name in names {
        let id = resolver.group(db, name, now_ms, report).await?;
        if !wanted.contains(&id) {
            wanted.push(id);
        }
    }

    let current: Vec<i64> = talkgroup_group::Entity::find()
        .filter(talkgroup_group::Column::TalkgroupId.eq(talkgroup_id))
        .all(db)
        .await?
        .into_iter()
        .map(|link| link.group_id)
        .collect();

    let mut changed = false;
    for group_id in &wanted {
        if !current.contains(group_id) {
            repo::link_talkgroup_group(db, talkgroup_id, *group_id).await?;
            changed = true;
        }
    }
    for group_id in &current {
        if !wanted.contains(group_id) {
            talkgroup_group::Entity::delete_by_id((talkgroup_id, *group_id))
                .exec(db)
                .await?;
            changed = true;
        }
    }
    Ok(changed)
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// `POST /api/admin/talkgroups/import` — bulk Talkgroup curation.
///
/// The body is the CSV itself (`text/csv`), so `curl --data-binary @tg.csv`
/// works and a browser can post a `FileReader` result unchanged. Query
/// parameters: `system` (the default System — a Ref or a label) and `dryRun`.
///
/// `dryRun` reads as a **flag**: `?dryRun`, `?dryRun=`, and `?dryRun=true` all
/// mean "don't write". Only an explicit denial (`false`/`0`/`no`/`off`) turns it
/// back off. Requiring a value would make the bare `?dryRun` an operator
/// plainly means as "preview this" silently perform the real import instead —
/// the one mistake here that can't be undone.
///
/// **Not authenticated** — see the module docs. #19 gates `/api/admin/`.
pub async fn import_talkgroups(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    body: bytes::Bytes,
) -> Result<ImportReport, Failure> {
    let options = ImportOptions {
        default_system: params
            .get("system")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(SystemSelector::parse),
        dry_run: params
            .get("dryRun")
            .is_some_and(|value| !matches!(value.trim(), "false" | "0" | "no" | "off")),
    };

    import(&state.db, &body, &options, state.clock.now_ms())
        .await
        .map_err(|error| match error {
            // A CSV we could not read at all is the operator's, and they are
            // told which of the three things was wrong with it.
            ImportError::Parse(err) => Reason::BadImport(err).into(),
            ImportError::Db(err) => Failure::broke(Stage::ImportTalkgroups, err),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    /// Parse with System 11 as the request-level default — the common case, and
    /// the only way a RadioReference export (which has no System column) lands.
    fn parse_with_default(csv: &str) -> Parsed {
        parse(csv.as_bytes(), Some(&SystemSelector::Ref(11))).expect("parses")
    }

    fn only_edit(csv: &str) -> TalkgroupEdit {
        let parsed = parse_with_default(csv);
        assert!(
            parsed.rejected.is_empty(),
            "unexpected rejections: {:?}",
            parsed.rejected
        );
        assert_eq!(parsed.edits.len(), 1, "expected exactly one edit");
        parsed.edits.into_iter().next().unwrap()
    }

    fn only_rejection(csv: &str) -> RejectedRow {
        let parsed = parse_with_default(csv);
        assert!(
            parsed.edits.is_empty(),
            "unexpected edits: {:?}",
            parsed.edits
        );
        assert_eq!(parsed.rejected.len(), 1, "expected exactly one rejection");
        parsed.rejected.into_iter().next().unwrap()
    }

    // -----------------------------------------------------------------------
    // Layout detection
    // -----------------------------------------------------------------------

    /// The shape the ticket names: `ref,label,group,tag,led`.
    #[test]
    fn named_header_reads_the_ticket_shape() {
        let parsed =
            parse_with_default("ref,label,group,tag,led\n54241,TDB A1,Fire,Dispatch,red\n");
        assert_eq!(parsed.layout, Layout::Named);
        assert_eq!(
            parsed.edits,
            vec![TalkgroupEdit {
                line: 2,
                system: SystemSelector::Ref(11),
                talkgroup_ref: 54241,
                label: Some("TDB A1".into()),
                name: None,
                tag: Some("Dispatch".into()),
                groups: vec!["Fire".into()],
                led: Some("red".into()),
                member_refs: None,
            }]
        );
    }

    /// Columns may appear in any order, and columns we don't know (`Hex`,
    /// `Mode`, `Priority`) are ignored rather than shifting everything along —
    /// the failure mode of a positional-only reader.
    #[test]
    fn named_columns_are_order_free_and_extras_are_ignored() {
        let edit = only_edit(
            "Priority,Category,Hex,Alpha Tag,Mode,Decimal,Description\n\
             3,Fire,0xD3E1,TDB A1,D,54241,Butler Dispatch\n",
        );
        assert_eq!(edit.talkgroup_ref, 54241);
        assert_eq!(edit.label.as_deref(), Some("TDB A1"));
        assert_eq!(edit.name.as_deref(), Some("Butler Dispatch"));
        assert_eq!(edit.groups, vec!["Fire".to_string()]);
    }

    /// Every spelling that should reach the Ref column, since getting this
    /// wrong rejects an operator's whole file.
    #[rstest]
    #[case("ref")]
    #[case("Ref")]
    #[case("REF")]
    #[case("talkgroup")]
    #[case("talkgroupRef")]
    #[case("Talkgroup Ref")]
    #[case("talkgroup_ref")]
    #[case("TGID")]
    #[case("Decimal")]
    #[case("dec")]
    fn ref_column_aliases(#[case] header: &str) {
        let parsed = parse_with_default(&format!("{header},label\n54241,TDB A1\n"));
        assert_eq!(parsed.layout, Layout::Named, "header={header:?}");
        assert_eq!(parsed.edits[0].talkgroup_ref, 54241, "header={header:?}");
    }

    /// The other columns' aliases, checked one at a time against a fixed Ref
    /// column so a mis-mapped alias can't hide behind a neighbour.
    #[rstest]
    #[case("label", "TDB A1")]
    #[case("Alpha Tag", "TDB A1")]
    #[case("alpha_tag", "TDB A1")]
    fn label_column_aliases(#[case] header: &str, #[case] value: &str) {
        let edit = only_edit(&format!("ref,{header}\n54241,{value}\n"));
        assert_eq!(edit.label.as_deref(), Some(value), "header={header:?}");
    }

    #[rstest]
    #[case("name")]
    #[case("Description")]
    #[case("desc")]
    fn name_column_aliases(#[case] header: &str) {
        let edit = only_edit(&format!("ref,{header}\n54241,Butler Dispatch\n"));
        assert_eq!(
            edit.name.as_deref(),
            Some("Butler Dispatch"),
            "header={header:?}"
        );
    }

    #[rstest]
    #[case("group")]
    #[case("groups")]
    #[case("Category")]
    fn group_column_aliases(#[case] header: &str) {
        let edit = only_edit(&format!("ref,{header}\n54241,Fire\n"));
        assert_eq!(edit.groups, vec!["Fire".to_string()], "header={header:?}");
    }

    #[rstest]
    #[case("led")]
    #[case("LED Color")]
    #[case("colour")]
    fn led_column_aliases(#[case] header: &str) {
        let edit = only_edit(&format!("ref,{header}\n54241,red\n"));
        assert_eq!(edit.led.as_deref(), Some("red"), "header={header:?}");
    }

    #[rstest]
    #[case("system")]
    #[case("System Ref")]
    #[case("shortName")]
    fn system_column_aliases(#[case] header: &str) {
        let edit = only_edit(&format!("ref,{header}\n54241,42\n"));
        assert_eq!(edit.system, SystemSelector::Ref(42), "header={header:?}");
    }

    /// The headerless RadioReference / Trunk Recorder export, read at exactly
    /// the indices rdio-scanner reads — so a file that imports there imports
    /// here. `Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category`.
    #[test]
    fn headerless_radioreference_export_uses_rdios_positions() {
        let parsed =
            parse_with_default("54241,D3E1,TDB A1,D,Butler Dispatch,Fire Dispatch,Butler County\n");
        assert_eq!(parsed.layout, Layout::RadioReference);
        assert_eq!(
            parsed.edits,
            vec![TalkgroupEdit {
                line: 1,
                system: SystemSelector::Ref(11),
                talkgroup_ref: 54241,
                label: Some("TDB A1".into()),
                name: Some("Butler Dispatch".into()),
                tag: Some("Fire Dispatch".into()),
                groups: vec!["Butler County".into()],
                led: None,
                member_refs: None,
            }]
        );
    }

    /// The same export *with* its header row reads identically — one file, two
    /// ways of saving it, one result.
    #[test]
    fn radioreference_with_and_without_its_header_agree() {
        let data = "54241,D3E1,TDB A1,D,Butler Dispatch,Fire Dispatch,Butler County\n";
        let headed = parse_with_default(&format!(
            "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category\n{data}"
        ));
        let bare = parse_with_default(data);

        assert_eq!(headed.layout, Layout::Named);
        assert_eq!(bare.layout, Layout::RadioReference);
        // Same values; only the line number differs (the header takes line 1).
        assert_eq!(headed.edits[0].line, 2);
        assert_eq!(bare.edits[0].line, 1);
        assert_eq!(
            TalkgroupEdit {
                line: bare.edits[0].line,
                ..headed.edits[0].clone()
            },
            bare.edits[0]
        );
    }

    /// The first data row of a headerless file is data, not a discarded header.
    /// rdio drops any row whose first field isn't numeric, which silently eats
    /// a real row if a file happens to lead with one.
    #[test]
    fn headerless_first_row_is_not_swallowed() {
        let parsed = parse_with_default("54241,,A\n54242,,B\n");
        assert_eq!(parsed.edits.len(), 2);
        assert_eq!(parsed.edits[0].talkgroup_ref, 54241);
        assert_eq!(parsed.edits[1].talkgroup_ref, 54242);
    }

    /// Neither a recognizable header nor positional data — refuse the file
    /// rather than guess what its columns mean.
    #[test]
    fn unrecognizable_header_is_a_whole_file_error() {
        let err = parse(b"foo,bar,baz\nx,y,z\n", Some(&SystemSelector::Ref(11))).unwrap_err();
        assert_eq!(err, ParseError::NoRefColumn);
        assert_eq!(err.reason(), "no-ref-column");
    }

    #[rstest]
    #[case(b"" as &[u8])]
    #[case(b"\n\n\n")]
    #[case(b"ref,label\n")] // header, no data
    #[case(b"ref,label\n\n,\n")] // header plus punctuation
    fn files_with_no_data_rows_are_rejected(#[case] csv: &[u8]) {
        let err = parse(csv, Some(&SystemSelector::Ref(11))).unwrap_err();
        assert_eq!(err, ParseError::Empty);
        assert_eq!(err.reason(), "empty-csv");
    }

    /// A Latin-1 spreadsheet export. rdio reads these with
    /// `readAsBinaryString` and silently writes mojibake into every accented
    /// label; we refuse the file and say why.
    #[test]
    fn non_utf8_bytes_are_a_named_error_not_mojibake() {
        // "Côté" in Latin-1: 0xF4 and 0xE9 are not valid UTF-8.
        let latin1 = b"ref,label\n54241,C\xf4t\xe9\n";
        let err = parse(latin1, Some(&SystemSelector::Ref(11))).unwrap_err();
        assert!(matches!(err, ParseError::Malformed(_)), "got {err:?}");
        assert_eq!(err.reason(), "malformed-csv");
        assert!(
            err.to_string().contains("UTF-8"),
            "the message should point at the encoding: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // Real CSV syntax (the rdio regex-split bugs)
    // -----------------------------------------------------------------------

    /// rdio splits on `/"*,"*/`, so a quoted `"Fire, EMS"` becomes two fields
    /// and every column after it shifts. Proper parsing keeps it one value.
    #[test]
    fn quoted_commas_stay_inside_one_field() {
        let edit = only_edit("ref,label,group\n54241,\"Dispatch, North\",\"Fire, EMS\"\n");
        assert_eq!(edit.label.as_deref(), Some("Dispatch, North"));
        assert_eq!(edit.groups, vec!["Fire, EMS".to_string()]);
    }

    #[rstest]
    // Doubled quotes are one literal quote (RFC 4180).
    #[case("ref,label\n54241,\"TDB \"\"A1\"\"\"\n", "TDB \"A1\"")]
    // A newline inside quotes does not end the record.
    #[case("ref,label\n54241,\"North\nDispatch\"\n", "North\nDispatch")]
    // Whitespace around an unquoted value is trimmed.
    #[case("ref,label\n54241,   TDB A1   \n", "TDB A1")]
    // CRLF line endings (a Windows-saved file).
    #[case("ref,label\r\n54241,TDB A1\r\n", "TDB A1")]
    fn csv_syntax_cases(#[case] csv: &str, #[case] expected_label: &str) {
        assert_eq!(only_edit(csv).label.as_deref(), Some(expected_label));
    }

    /// A spreadsheet writes a BOM; it must not glue itself to the first header
    /// name (which would lose the Ref column and reject the whole file).
    #[test]
    fn utf8_bom_is_stripped() {
        let parsed = parse(
            "\u{feff}ref,label\n54241,TDB A1\n".as_bytes(),
            Some(&SystemSelector::Ref(11)),
        )
        .expect("parses");
        assert_eq!(parsed.layout, Layout::Named);
        assert_eq!(parsed.edits[0].talkgroup_ref, 54241);
    }

    /// Blank and separator-only lines are punctuation, not failed rows: they
    /// neither produce edits nor clutter the report.
    #[test]
    fn blank_lines_are_skipped_silently() {
        let parsed = parse_with_default("ref,label\n\n54241,TDB A1\n,\n\n54242,TDB A2\n");
        assert_eq!(parsed.edits.len(), 2);
        assert!(parsed.rejected.is_empty(), "{:?}", parsed.rejected);
    }

    /// Ragged rows are normal in hand-edited files: a row too short to have a
    /// column simply said nothing about it.
    #[test]
    fn short_rows_leave_missing_columns_unset() {
        let edit = only_edit("ref,label,tag,led\n54241,TDB A1\n");
        assert_eq!(edit.label.as_deref(), Some("TDB A1"));
        assert_eq!(edit.tag, None);
        assert_eq!(edit.led, None);
    }

    /// Line numbers are the ones the operator sees in their editor — counting
    /// the header and every blank line — or the report can't be acted on.
    #[test]
    fn rejections_carry_the_editor_line_number() {
        let parsed = parse_with_default("ref,label\n54241,ok\n\nnope,bad\n54243,ok\n");
        assert_eq!(parsed.rejected.len(), 1);
        assert_eq!(parsed.rejected[0].line, 4);
        assert_eq!(parsed.rejected[0].reason, "ref-not-a-number");
    }

    // -----------------------------------------------------------------------
    // Row validation
    // -----------------------------------------------------------------------

    #[rstest]
    #[case("ref,label\n,TDB A1\n", "missing-ref")]
    #[case("ref,label\n   ,TDB A1\n", "missing-ref")]
    #[case("ref,label\nnope,TDB A1\n", "ref-not-a-number")]
    #[case("ref,label\n54.5,TDB A1\n", "ref-not-a-number")]
    #[case("ref,label\n\"54,241\",TDB A1\n", "ref-not-a-number")]
    #[case("ref,led\n54241,purple\n", "led-not-in-palette")]
    #[case("ref,led\n54241,#ff0000\n", "led-not-in-palette")]
    fn row_rejection_cases(#[case] csv: &str, #[case] expected_reason: &str) {
        assert_eq!(only_rejection(csv).reason, expected_reason);
    }

    /// A rejection names the offending value, so the operator doesn't have to
    /// guess which cell of the line was wrong.
    #[test]
    fn rejections_quote_the_offending_value() {
        assert_eq!(
            only_rejection("ref,led\n54241,purple\n").detail.as_deref(),
            Some("purple")
        );
        assert_eq!(
            only_rejection("ref,label\nnope,x\n").detail.as_deref(),
            Some("nope")
        );
    }

    /// One bad row doesn't cost the operator the other 399. rdio drops bad rows
    /// too — but silently, so nobody knows to fix them.
    #[test]
    fn good_rows_survive_alongside_bad_ones() {
        let parsed = parse_with_default(
            "ref,label,led\n\
             54241,A,red\n\
             oops,B,red\n\
             54243,C,chartreuse\n\
             54244,D,blue\n",
        );
        assert_eq!(
            parsed
                .edits
                .iter()
                .map(|e| e.talkgroup_ref)
                .collect::<Vec<_>>(),
            vec![54241, 54244]
        );
        assert_eq!(
            parsed.rejected.iter().map(|r| r.reason).collect::<Vec<_>>(),
            vec!["ref-not-a-number", "led-not-in-palette"]
        );
    }

    // -----------------------------------------------------------------------
    // LED palette
    // -----------------------------------------------------------------------

    #[rstest]
    #[case("blue")]
    #[case("cyan")]
    #[case("green")]
    #[case("magenta")]
    #[case("orange")]
    #[case("red")]
    #[case("white")]
    #[case("yellow")]
    fn every_palette_color_is_accepted(#[case] color: &str) {
        let edit = only_edit(&format!("ref,led\n54241,{color}\n"));
        assert_eq!(edit.led.as_deref(), Some(color));
    }

    /// Operators type `Red`; the palette stores `red`.
    #[rstest]
    #[case("Red")]
    #[case("RED")]
    #[case("  Red  ")]
    fn led_is_case_and_space_insensitive(#[case] written: &str) {
        assert_eq!(
            only_edit(&format!("ref,led\n54241,{written}\n"))
                .led
                .as_deref(),
            Some("red")
        );
    }

    /// The backend's palette and the client's must not drift: a color that
    /// imports but doesn't render is a silent half-feature. Pinned against the
    /// client source the same way `led.test.ts` pins it against `index.css`.
    #[test]
    fn palette_matches_the_client() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/client/src/lib/led.ts"
        ))
        .expect("client led.ts");
        // The `LED_HEX` record is the palette's full membership list.
        let hex_block = source
            .split_once("export const LED_HEX")
            .expect("LED_HEX")
            .1
            .split_once('}')
            .expect("closing brace")
            .0;
        let mut client: Vec<&str> = hex_block
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(name, _)| name.trim())
            .filter(|name| !name.is_empty())
            .collect();
        client.sort_unstable();

        let mut ours = LED_PALETTE.to_vec();
        ours.sort_unstable();
        assert_eq!(ours, client, "backend LED palette drifted from the client");
    }

    // -----------------------------------------------------------------------
    // Systems and groups
    // -----------------------------------------------------------------------

    /// A numeric `system` cell is a Ref; anything else is a label (a Trunk
    /// Recorder `short_name`, which is what an operator actually knows).
    #[rstest]
    #[case("42", SystemSelector::Ref(42))]
    #[case("  42  ", SystemSelector::Ref(42))]
    #[case("butco", SystemSelector::Label("butco".into()))]
    #[case("Butler County", SystemSelector::Label("Butler County".into()))]
    fn system_cell_selects_by_ref_or_label(#[case] cell: &str, #[case] expected: SystemSelector) {
        assert_eq!(
            only_edit(&format!("ref,system\n54241,{cell}\n")).system,
            expected
        );
    }

    /// A rejection quotes the operator's own spelling of the System back to
    /// them, whichever way they named it.
    #[rstest]
    #[case(SystemSelector::Ref(11), "11")]
    #[case(SystemSelector::Ref(-1), "-1")]
    #[case(SystemSelector::Label("butco".into()), "butco")]
    fn system_selector_reads_back_as_written(
        #[case] selector: SystemSelector,
        #[case] expected: &str,
    ) {
        assert_eq!(selector.to_string(), expected);
    }

    /// A database failure is carried, not flattened into a parse error — the
    /// two mean very different things to the operator (and to the status code).
    #[test]
    fn db_errors_stay_db_errors() {
        let err = ImportError::from(DbErr::Custom("pool closed".into()));
        assert!(matches!(err, ImportError::Db(_)), "got {err:?}");
    }

    /// A row's own System column beats the request-level default.
    #[test]
    fn row_system_overrides_the_default() {
        let parsed = parse_with_default("ref,system\n54241,42\n54242,\n");
        assert_eq!(parsed.edits[0].system, SystemSelector::Ref(42));
        assert_eq!(parsed.edits[1].system, SystemSelector::Ref(11));
    }

    /// With no System anywhere, the row can't be placed — a Talkgroup Ref is
    /// unique only within its System, so guessing would curate the wrong channel.
    #[test]
    fn a_row_with_no_system_anywhere_is_rejected() {
        let parsed = parse(b"ref,label\n54241,TDB A1\n", None).expect("parses");
        assert!(parsed.edits.is_empty());
        assert_eq!(parsed.rejected[0].reason, "no-system");
    }

    #[rstest]
    #[case("Fire", vec!["Fire"])]
    #[case("Fire;EMS", vec!["Fire", "EMS"])]
    #[case("Fire; EMS ;Police", vec!["Fire", "EMS", "Police"])]
    #[case("Fire;;EMS", vec!["Fire", "EMS"])] // empty parts dropped
    #[case(";", vec![])]
    fn group_cell_splits_into_a_set(#[case] cell: &str, #[case] expected: Vec<&str>) {
        let parsed = parse_with_default(&format!("ref,group\n54241,\"{cell}\"\n"));
        assert_eq!(parsed.edits[0].groups, expected, "cell={cell:?}");
    }

    // -----------------------------------------------------------------------
    // Properties
    // -----------------------------------------------------------------------

    proptest! {
        /// Whatever bytes arrive, parsing terminates without panicking and
        /// every edit it yields is well-formed: a real palette color, no blank
        /// strings where a value was promised.
        #[test]
        fn parse_never_panics_and_yields_well_formed_edits(
            body in "(?s)[ -~\n\r,\"]{0,200}",
        ) {
            if let Ok(parsed) = parse(body.as_bytes(), Some(&SystemSelector::Ref(11))) {
                for edit in &parsed.edits {
                    if let Some(led) = &edit.led {
                        prop_assert!(LED_PALETTE.contains(&led.as_str()), "led {led:?}");
                    }
                    for value in [&edit.label, &edit.name, &edit.tag].into_iter().flatten() {
                        prop_assert!(!value.is_empty(), "blank value kept as Some");
                    }
                    for group in &edit.groups {
                        prop_assert!(!group.is_empty(), "blank group kept");
                    }
                }
            }
        }

        /// Any Ref that survives is exactly the number the file wrote, and a
        /// well-formed row is never rejected.
        #[test]
        fn well_formed_rows_round_trip(
            talkgroup_ref in 0i64..1_000_000,
            // Leading alphanumeric, so trimming can never empty it — a wholly
            // blank cell is a cell that said nothing, covered separately.
            label in "[a-zA-Z0-9][a-zA-Z0-9 ]{0,19}",
            color_index in 0usize..LED_PALETTE.len(),
        ) {
            let led = LED_PALETTE[color_index];
            let csv = format!("ref,label,led\n{talkgroup_ref},{label},{led}\n");
            let parsed = parse(csv.as_bytes(), Some(&SystemSelector::Ref(11))).expect("parses");
            prop_assert!(parsed.rejected.is_empty(), "{:?}", parsed.rejected);
            prop_assert_eq!(parsed.edits.len(), 1);
            prop_assert_eq!(parsed.edits[0].talkgroup_ref, talkgroup_ref);
            prop_assert_eq!(parsed.edits[0].label.as_deref(), Some(label.trim()));
            prop_assert_eq!(parsed.edits[0].led.as_deref(), Some(led));
        }
    }
}
