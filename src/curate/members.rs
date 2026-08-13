//! **Merge curation** — the Refs a channel and an apparatus answer to, edited
//! from a browser (#50, spec US 17).
//!
//! #45 built folding and unfolding and gave it exactly one way in: a member-Refs
//! cell in a CSV. #49 built the curation surface and deliberately left these
//! sets off both row shapes, because carrying them would cost a query per row on
//! a page that cannot edit them. This module is the screen they were left for.
//!
//! # Why a delta and not the whole set
//!
//! The CSV cell means "this is the whole truth for the row it names", so
//! removing a Ref from it is how an operator unmerges. That convention is right
//! for a file and wrong for a form: a form is submitted by a tab that read the
//! set some minutes ago, and *inferring an unmerge from absence* is precisely
//! the rdio-scanner failure [`super`]'s own table calls out — one `PUT` of the
//! whole document where a row missing from the payload is deleted.
//!
//! So the wire is a delta ([`Delta`]): fold these, unfold those, and say nothing
//! about anything else. The **set is computed inside the transaction** that
//! writes it ([`after`]), against the members really held at that moment, and
//! then handed to [`repo::set_member_refs`] — the same writer the importer uses,
//! because a second fold path is how the two surfaces come to disagree about
//! what a fold is.
//!
//! # The preview
//!
//! A fold is the one curation act that rewrites the **archive** rather than the
//! configuration, so it is the one that has to be seen before it runs. The
//! preview is `?dryRun`, and it is the *real* transaction rolled back — #18's
//! dry run, for #18's reason: a separately-computed estimate is a second
//! implementation of the thing it claims to describe, and the first divergence
//! is an operator being told 12 Calls would move while 1,412 do.
//!
//! It reports per Ref and not merely in total ([`Moved`]), which is what lets a
//! confirmation name what it is about to destroy — and is the only thing that
//! catches a Ref selected from **another System**. A Ref is unique only within
//! one (CONTEXT.md), so folding another System's number resolves to no channel
//! here and records as a bare member Ref: a perfectly ordinary-looking row that
//! silently did nothing the operator wanted. The preview shows it as `recorded`
//! with nothing behind it, where a total of "1 folded" could not.
//!
//! # Ranges have no preview, and that is not an oversight
//!
//! A Call names the radios it heard by **Ref** (`call_units`), never by a Unit's
//! Id — which is why deleting a Unit takes no Call with it ([`super::units`]).
//! So editing a Unit's Ranges moves no Call and there is nothing to show before
//! committing: the archive reads back the same, and an apparatus is named or is
//! not. The asymmetry between the two halves of this module is that difference,
//! not two people's taste.
//!
//! What a Range edit *does* owe is atomicity. [`repo::set_unit_ranges`] reports
//! a refused span rather than failing — right for a CSV, where the rest of the
//! file should still apply — so this surface rolls the transaction back and
//! answers with the collision instead ([`Rejected::RangeOverlaps`]), because a
//! form that half-applied is a form whose screen now lies.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder};
use serde::{Deserialize, Serialize};

use super::{Listing, Rejected, What};
use crate::AppState;
use crate::db::entities::{talkgroup, talkgroup_ref, unit, unit_ref};
use crate::db::repo;
use crate::failure::{Failure, Stage};
use crate::merge::Range;

// -- What crosses the wire ---------------------------------------------------

/// One Ref a channel answers to besides its own.
///
/// `label` is what the folded channel called itself, so the list reads as the
/// names an Operator curated rather than the numbers a recorder sent — and
/// `null` for a Ref that was never a channel of its own, which is the honest
/// difference between "TAC 3, folded in" and "8123, written down".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberRow {
    pub r#ref: i64,
    pub label: Option<String>,
}

/// One span of radio ids an apparatus answers to, both ends inclusive.
///
/// The wire shape of [`Range`], which is deliberately not `Serialize` itself:
/// its constructor normalizes a reversed span, and a type that could be
/// *deserialized* past its own constructor would let `1299-1200` reach the
/// database as a row that owns nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Span {
    pub from: i64,
    pub to: i64,
}

impl Span {
    /// The Range this span means.
    ///
    /// Named rather than a `From` impl because [`Range::from`] is already the
    /// accessor for a Range's lower end, and `map(Range::from)` would silently
    /// resolve to that one.
    fn range(self) -> Range {
        Range::new(self.from, self.to)
    }
}

impl From<Range> for Span {
    fn from(range: Range) -> Self {
        Span {
            from: range.from(),
            to: range.to(),
        }
    }
}

/// Fold these, unfold those, and say nothing about anything else.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Delta {
    /// Refs this channel should also answer to. A Ref naming an existing
    /// channel absorbs it; one naming nothing is simply written down.
    #[serde(default)]
    pub fold: Vec<i64>,
    /// Member Refs that should become channels of their own again.
    #[serde(default)]
    pub unfold: Vec<i64>,
}

/// The same, for a Unit's Ranges.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RangeDelta {
    #[serde(default)]
    pub add: Vec<Span>,
    #[serde(default)]
    pub remove: Vec<Span>,
}

/// What a fold did — or, on a preview, would do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeReport {
    /// True when nothing was written: the same work, rolled back.
    pub dry_run: bool,
    pub folded: u64,
    pub unfolded: u64,
    pub calls_repointed: u64,
    /// Every Ref that changed hands, in the order it was named.
    pub moved: Vec<MovedRow>,
}

/// One Ref's share of a merge, as a confirmation renders it.
///
/// The Talkgroup id [`repo::Moved`] carries is deliberately **not** here: on a
/// preview a restored id is provisional (the row is rolled back with everything
/// else), and a number that means something different depending on `dryRun` is
/// worse than no number. The Ref is what an Operator recognises anyway, and the
/// ids go to the log line, where the run they describe is settled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MovedRow {
    pub r#ref: i64,
    pub movement: &'static str,
    pub label: Option<String>,
    pub calls: u64,
    pub carried: Vec<i64>,
}

impl From<repo::Moved> for MovedRow {
    fn from(moved: repo::Moved) -> Self {
        MovedRow {
            r#ref: moved.r#ref,
            movement: match moved.movement {
                repo::Movement::Folded => "folded",
                repo::Movement::Recorded => "recorded",
                repo::Movement::Unfolded => "unfolded",
            },
            label: moved.label,
            calls: moved.calls,
            carried: moved.carried,
        }
    }
}

/// What a Range edit did. No `dryRun`: nothing here moves a Call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeReport {
    pub added: u64,
    pub removed: u64,
}

crate::answers_json!(MergeReport, RangeReport);

// -- Handlers ----------------------------------------------------------------

/// `GET /api/admin/talkgroups/{id}/members` — the Refs this channel answers to.
///
/// A read of its own because the Talkgroup listing deliberately does not carry
/// them (#49): a query per row on a page of five hundred, for a column that page
/// cannot edit. Here there is exactly one row.
pub async fn list_members(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Listing<MemberRow>, Failure> {
    let db = &state.db;
    let owner = channel(db, id).await?;

    let members = talkgroup_ref::Entity::find()
        .filter(talkgroup_ref::Column::TalkgroupId.eq(owner.id))
        .order_by_asc(talkgroup_ref::Column::Position)
        .all(db)
        .await
        .map_err(Stage::Curate.failed())?;

    Ok(Listing::new(
        members
            .into_iter()
            .map(|member| MemberRow {
                r#ref: member.r#ref,
                label: member.label,
            })
            .collect(),
    ))
}

/// `POST /api/admin/talkgroups/{id}/members` — fold, unfold, or preview either.
///
/// `?dryRun` reads as a flag exactly as the CSV import's does
/// ([`crate::import::asked_for_a_dry_run`]) — a bare `?dryRun` is an Operator
/// plainly asking to preview, and making them spell a value would turn the one
/// mistake here that cannot be undone into a typo.
pub async fn fold(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(params): Query<HashMap<String, String>>,
    axum::Json(body): axum::Json<Delta>,
) -> Result<MergeReport, Failure> {
    let db = &state.db;
    let dry_run = crate::import::asked_for_a_dry_run(&params);
    let now_ms = state.clock.now_ms();

    // Everything happens inside one transaction, the preview included — the set
    // is computed against the members really held *at this moment* rather than
    // the ones a browser read some minutes ago, and a refusal rolls the whole
    // edit back by dropping the handle.
    let txn = db.begin().await.map_err(Stage::Curate.failed())?;
    let owner = channel(&txn, id).await?;
    let held = repo::member_refs_of(&txn, owner.id)
        .await
        .map_err(Stage::Curate.failed())?;
    let wanted = after(&held, &body.fold, &body.unfold);
    refuse_a_ref_another_channel_holds(&txn, &owner, &held, &wanted).await?;

    let change = repo::set_member_refs(&txn, &owner, &wanted, now_ms)
        .await
        .map_err(Stage::Curate.failed())?;
    match dry_run {
        true => txn.rollback().await,
        false => txn.commit().await,
    }
    .map_err(Stage::Curate.failed())?;

    change.record(owner.id, owner.r#ref, dry_run);

    Ok(MergeReport {
        dry_run,
        folded: change.folded,
        unfolded: change.unfolded,
        calls_repointed: change.calls_repointed,
        moved: change.moved.into_iter().map(MovedRow::from).collect(),
    })
}

/// `GET /api/admin/units/{id}/ranges` — the spans this apparatus answers to.
pub async fn list_ranges(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Listing<Span>, Failure> {
    let db = &state.db;
    let unit = apparatus(db, id).await?;

    Ok(Listing::new(
        spans_of(db, &unit)
            .await
            .map_err(Stage::Curate.failed())?
            .into_iter()
            .map(Span::from)
            .collect(),
    ))
}

/// `POST /api/admin/units/{id}/ranges` — add spans, remove spans, wholly or not
/// at all.
pub async fn set_ranges(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    axum::Json(body): axum::Json<RangeDelta>,
) -> Result<RangeReport, Failure> {
    let db = &state.db;
    let now_ms = state.clock.now_ms();
    // `Range::from` would resolve to the inherent accessor, not the conversion.
    let add: Vec<Range> = body.add.iter().copied().map(Span::range).collect();
    let remove: Vec<Range> = body.remove.iter().copied().map(Span::range).collect();

    let txn = db.begin().await.map_err(Stage::Curate.failed())?;
    let unit = apparatus(&txn, id).await?;
    let held = spans_of(&txn, &unit)
        .await
        .map_err(Stage::Curate.failed())?;
    let wanted = after(&held, &add, &remove);

    let set = repo::set_unit_ranges(&txn, &unit, &wanted, now_ms)
        .await
        .map_err(Stage::Curate.failed())?;
    // `set_unit_ranges` reports a refused span and applies the rest, which is
    // right for a CSV — the remainder of the file should still land. A form
    // that half-applied is a form whose screen now lies, so the handle is
    // dropped here and the transaction goes with it.
    if let Some((wanted, held)) = set.refused.first() {
        return Err(Rejected::RangeOverlaps {
            wanted: *wanted,
            held: *held,
        }
        .into());
    }
    txn.commit().await.map_err(Stage::Curate.failed())?;

    if set.added > 0 || set.removed > 0 {
        // The spans themselves, not merely how many: "added=1" a month later
        // does not tell an Operator which block of radios started answering to
        // which apparatus, which is the only question this line gets asked.
        //
        // Built before the macro rather than inside it, for the reason
        // `MergeChange::record` gives: a `tracing` field expression only runs
        // when a subscriber is interested, which makes it look unreachable.
        let spans = spans(&wanted);
        tracing::info!(
            unit_id = unit.id,
            unit_ref = unit.r#ref,
            added = set.added,
            removed = set.removed,
            %spans,
            "unit Ranges changed"
        );
    }

    Ok(RangeReport {
        added: set.added,
        removed: set.removed,
    })
}

// -- The parts the handlers are made of --------------------------------------

/// The channel a path names, or the refusal for one that is not there.
async fn channel<C: ConnectionTrait>(db: &C, id: i64) -> Result<talkgroup::Model, Failure> {
    talkgroup::Entity::find_by_id(id)
        .one(db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or_else(|| Rejected::NotFound(What::Talkgroup).into())
}

/// The apparatus a path names, or the refusal for one that is not there.
async fn apparatus<C: ConnectionTrait>(db: &C, id: i64) -> Result<unit::Model, Failure> {
    unit::Entity::find_by_id(id)
        .one(db)
        .await
        .map_err(Stage::Curate.failed())?
        .ok_or_else(|| Rejected::NotFound(What::Unit).into())
}

/// The spans a Unit owns, in the Operator's order.
async fn spans_of<C: ConnectionTrait>(db: &C, unit: &unit::Model) -> Result<Vec<Range>, DbErr> {
    Ok(unit_ref::Entity::find()
        .filter(unit_ref::Column::UnitId.eq(unit.id))
        .order_by_asc(unit_ref::Column::Position)
        .all(db)
        .await?
        .into_iter()
        .map(|span| Range::new(span.ref_from, span.ref_to))
        .collect())
}

/// Refuse a Ref that is already another channel's **member** Ref.
///
/// The rule is [`repo::member_ref_held_elsewhere`], shared with the CSV
/// importer so the two surfaces cannot come to disagree about what a fold may
/// take. What differs is only the shape of the refusal: a file gets a rejected
/// row naming the line, a form gets a status and a sentence beside the input.
///
/// Only Refs genuinely *arriving* are checked. One the owner already holds is
/// its own, and asking the question again would refuse every edit that left an
/// existing member alone — which is every edit.
async fn refuse_a_ref_another_channel_holds<C: ConnectionTrait>(
    db: &C,
    owner: &talkgroup::Model,
    held: &[i64],
    wanted: &[i64],
) -> Result<(), Failure> {
    for arriving in wanted.iter().filter(|r| !held.contains(r)) {
        let taken =
            repo::member_ref_held_elsewhere(db, owner.system_id, Some(owner.id), *arriving, wanted)
                .await
                .map_err(Stage::Curate.failed())?;
        if taken {
            return Err(Rejected::RefTaken {
                what: What::Talkgroup,
                taken: *arriving,
            }
            .into());
        }
    }
    Ok(())
}

/// The spans on a log line — `1201-1299,4471-4471`, in the Operator's order.
///
/// What the apparatus answers to *afterwards* rather than the delta that got it
/// there, because the delta is only meaningful beside a state nobody wrote down;
/// the resulting set is the thing an Operator can compare against what they see.
fn spans(ranges: &[Range]) -> String {
    ranges
        .iter()
        .map(|range| format!("{}-{}", range.from(), range.to()))
        .collect::<Vec<_>>()
        .join(",")
}

/// The set a delta leaves behind.
///
/// `held` in its own order with everything `remove` names excised, then whatever
/// `add` names that is not already there, appended in the order it was asked
/// for. Duplicates within `add` collapse: naming a Ref twice is a slip, and the
/// uniqueness index below would answer it with a 500.
///
/// Order matters only for display — it is what the members list renders — but it
/// has to be *stable*, or an edit that touches nothing still reshuffles the
/// screen underneath whoever made it.
///
/// Removal wins over addition. A delta naming the same Ref both ways is
/// incoherent and either answer is defensible; this one is the answer that
/// cannot destroy anything the operator did not ask to destroy, since the worst
/// case is a fold they have to repeat.
pub fn after<T: Copy + PartialEq>(held: &[T], add: &[T], remove: &[T]) -> Vec<T> {
    let mut kept: Vec<T> = held
        .iter()
        .copied()
        .filter(|entry| !remove.contains(entry))
        .collect();
    for entry in add {
        if !remove.contains(entry) && !kept.contains(entry) {
            kept.push(*entry);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// The delta's arithmetic, one claim per case. These are the four things an
    /// operator can do from the form — add, remove, do both at once, and submit
    /// a form they changed nothing on.
    #[rstest]
    #[case::nothing(&[100, 200], &[], &[], &[100, 200])]
    #[case::added(&[100], &[200], &[], &[100, 200])]
    #[case::removed(&[100, 200], &[], &[100], &[200])]
    #[case::both(&[100, 200], &[300], &[100], &[200, 300])]
    #[case::added_to_nothing(&[], &[100], &[], &[100])]
    #[case::removed_everything(&[100, 200], &[], &[100, 200], &[])]
    fn a_delta_leaves_exactly_what_it_named(
        #[case] held: &[i64],
        #[case] add: &[i64],
        #[case] remove: &[i64],
        #[case] expected: &[i64],
    ) {
        assert_eq!(after(held, add, remove), expected);
    }

    /// **Order is the operator's, and it is stable.** Additions land at the end
    /// in the order asked for, and everything already held keeps its place — so
    /// an edit that adds one Ref does not reshuffle the list underneath whoever
    /// is reading it.
    #[test]
    fn what_was_held_keeps_its_order_and_additions_land_behind_it() {
        assert_eq!(
            after(&[300, 100, 200], &[900, 800], &[]),
            [300, 100, 200, 900, 800]
        );
    }

    /// Re-adding what is already there is a no-op rather than a duplicate row —
    /// the uniqueness index would answer a second insert with a 500, and an
    /// operator clicking twice has done nothing wrong.
    #[test]
    fn a_ref_already_held_is_not_added_twice() {
        assert_eq!(after(&[100, 200], &[200], &[]), [100, 200]);
        assert_eq!(after(&[100], &[200, 200], &[]), [100, 200]);
    }

    /// Removing something nobody holds is a no-op, not a refusal: a second tab
    /// may have unfolded it already, and the operator's intent is satisfied.
    #[test]
    fn removing_what_is_not_held_changes_nothing() {
        assert_eq!(after(&[100], &[], &[999]), [100]);
    }

    /// A delta naming the same Ref both ways is incoherent, and removal wins —
    /// the arm that cannot destroy anything unasked.
    #[test]
    fn a_ref_named_both_ways_is_removed() {
        assert_eq!(after(&[100, 200], &[200], &[200]), [100]);
        assert_eq!(after(&[100], &[300], &[300]), [100]);
    }

    /// The same arithmetic serves a Unit's Ranges, which is why it is one
    /// function: a Range is a member Ref that happens to span (CONTEXT.md), and
    /// two copies of this would be two chances for the two screens to disagree
    /// about what removing something means.
    #[test]
    fn ranges_go_through_the_same_arithmetic() {
        let held = [Range::new(1200, 1299), Range::new(4471, 4471)];
        let add = [Range::new(1, 99)];
        let remove = [Range::new(4471, 4471)];

        assert_eq!(
            after(&held, &add, &remove),
            [Range::new(1200, 1299), Range::new(1, 99)]
        );
    }
}
