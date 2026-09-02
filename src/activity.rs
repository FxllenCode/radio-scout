//! **Activity**: how much of something happened, bucket by bucket (#62, spec
//! US 34–35, 41).
//!
//! Two surfaces want the same picture over two different subjects. The archive's
//! density ribbon and the hour-by-day heatmap are Calls per stretch of time
//! ([`crate::archive::call_activity`]); the listener chart is peak Listeners per
//! stretch of time ([`crate::listeners::history`]). The *subject* differs, the
//! *axis* does not — where it starts, how wide a bucket is, how many there are,
//! and which bucket an instant falls in — so the axis is a value here and the
//! two reads are two aggregates over it.
//!
//! # The arithmetic is written once, in two languages
//!
//! [`Axis::index_of`] and [`bucket_expr`] are the same expression: the offset
//! from the axis's origin, integer-divided by the bucket width. One runs in
//! Rust and one in SQL because the grouping has to happen in the database — a
//! county's month is half a million rows and nobody sends those over a socket to
//! be counted — and the client has to be able to say where a bucket *is* without
//! asking again.
//!
//! Two of them is one too many, so they sit next to each other and
//! `tests/db.rs` runs the SQL against both dialects and checks the buckets land
//! where the Rust says. That is the only thing that can catch the two drifting:
//! either alone answers confidently and wrongly.
//!
//! **Integer division is the whole reason this is dialect-safe.** SQLite and
//! Postgres both truncate `bigint / bigint` toward zero, and every row an
//! aggregate here sees is at or after the origin ([`Axis::from_ms`] is applied
//! as a filter, not merely as an origin), so truncation *is* flooring and the
//! two dialects cannot disagree. Grouping by a date function instead —
//! `strftime` against `date_trunc` — would have been a divergence *and* would
//! have put the fold into an hour-of-day in the server's timezone rather than
//! the Listener's.

use std::collections::HashMap;

use serde::Serialize;

use crate::query::{Filtered, Params};

/// The most buckets one response may carry.
///
/// A bound rather than a refusal, for the archive search's `MAX_LIMIT` reason: a
/// request must not be able to ask a Pi to serialize an unbounded array, and the
/// response says what width it actually used, so a client that asked for more
/// renders the truth rather than a mislabeled picture.
///
/// Sized by the widest thing that asks: the heatmap folds hourly buckets into a
/// 7×24 grid, and six weeks of hours is 1,008 of them.
pub const MAX_BUCKETS: usize = 1024;

/// How many buckets a caller that names no grain gets.
///
/// The density ribbon's, and it is really a pixel count: a phone is ~360 CSS px
/// wide, so a bar every three pixels is as fine as a thumb can address.
pub const DEFAULT_BUCKETS: usize = 120;

/// How finely a caller wants the range cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grain {
    /// Buckets this many milliseconds wide.
    ///
    /// What the hour-by-day heatmap asks for: folding buckets into local hours
    /// is only exact when a bucket *is* an hour, and an hourly bucket taken
    /// from a local-midnight origin stays hour-aligned across a daylight-saving
    /// shift, because those move by whole hours.
    Width(i64),
    /// About this many buckets across the whole range.
    ///
    /// What the ribbon asks for, because what it actually has is a width in
    /// pixels and no idea how long the search's range is.
    Count(usize),
}

/// The time axis a [`Series`] is read on.
///
/// Half-open: `from_ms` is included and [`Axis::to_ms`] is not, so two adjacent
/// axes share no instant and a Call cannot be counted twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Axis {
    from_ms: i64,
    bucket_ms: i64,
    buckets: usize,
}

impl Axis {
    /// The axis covering `from_ms..=last_ms` at `grain`, widened if that would
    /// have cost more than [`MAX_BUCKETS`].
    ///
    /// `last_ms` is inclusive because that is what an archive search's `before`
    /// means (`<=`), and an axis derived from a search must cover exactly what
    /// the search does. A range that is backwards, or a grain of zero, is read
    /// as one bucket rather than refused: this is reached from a query string,
    /// and a chart that draws nothing is a better answer to nonsense than a 400
    /// a client has to special-case.
    pub fn over(from_ms: i64, last_ms: i64, grain: Grain) -> Axis {
        let span = last_ms.saturating_sub(from_ms).saturating_add(1).max(1);
        let wanted = match grain {
            Grain::Width(width) => width.max(1),
            Grain::Count(count) => ceil_div(span, count.max(1) as i64),
        };
        // Widen until the array fits. One step, not a loop: dividing the span
        // by the cap is by definition the narrowest bucket that fits in it.
        let bucket_ms = wanted.max(ceil_div(span, MAX_BUCKETS as i64)).max(1);
        Axis {
            from_ms,
            bucket_ms,
            // The `min` is the *allocation* bound, not the arithmetic: this
            // value decides the length of a `Vec` and is reached from a query
            // string, so it must be bounded by something that does not depend
            // on the division above being right. With the division right it
            // never binds.
            buckets: (ceil_div(span, bucket_ms) as usize).clamp(1, MAX_BUCKETS),
        }
    }

    /// The first instant on the axis.
    pub fn from_ms(&self) -> i64 {
        self.from_ms
    }

    /// The first instant *past* the axis — exclusive, and at or after the
    /// `last_ms` it was built over.
    pub fn to_ms(&self) -> i64 {
        self.from_ms
            .saturating_add((self.buckets as i64).saturating_mul(self.bucket_ms))
    }

    /// How wide one bucket is.
    pub fn bucket_ms(&self) -> i64 {
        self.bucket_ms
    }

    /// How many buckets there are. Never zero — which is why this is not
    /// `len`, and why nothing here needs an `is_empty`.
    pub fn buckets(&self) -> usize {
        self.buckets
    }

    /// Which bucket an instant falls in, or `None` when it is off the axis.
    ///
    /// The Rust half of the pair this module exists to keep together — see
    /// [`bucket_expr`] for the SQL that has to agree with it.
    pub fn index_of(&self, at_ms: i64) -> Option<usize> {
        if at_ms < self.from_ms || at_ms >= self.to_ms() {
            return None;
        }
        Some(((at_ms - self.from_ms) / self.bucket_ms) as usize)
    }

    /// Spread `(bucket, value)` rows — a `GROUP BY` answered by the database,
    /// which says nothing at all about the empty buckets — across the whole
    /// axis.
    ///
    /// A bucket outside the axis is dropped rather than clamped into the edge,
    /// which would draw traffic that happened elsewhere onto the first or last
    /// bar. Nothing should produce one (the aggregates filter on the axis), and
    /// silently inventing a spike is the worse of the two ways to be wrong.
    pub fn series(&self, rows: impl IntoIterator<Item = (i64, i64)>) -> Series {
        let mut values = vec![0_i64; self.buckets];
        for (bucket, value) in rows {
            if let Ok(bucket) = usize::try_from(bucket)
                && let Some(slot) = values.get_mut(bucket)
            {
                *slot = value;
            }
        }
        Series {
            from_ms: self.from_ms,
            to_ms: self.to_ms(),
            bucket_ms: self.bucket_ms,
            values,
        }
    }
}

/// One number per bucket across one stretch of time — what both charts are
/// drawn from.
///
/// The axis rides with the values because the client has to place them, and it
/// is the *resolved* axis rather than the one that was asked for: a caller whose
/// grain was widened by [`MAX_BUCKETS`] is told, rather than drawing hourly
/// labels over three-hourly bars.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Series {
    /// The first instant covered, inclusive.
    pub from_ms: i64,
    /// The first instant past the end, exclusive.
    pub to_ms: i64,
    pub bucket_ms: i64,
    /// One value per bucket, oldest first. Never empty.
    pub values: Vec<i64>,
}

// How a series reaches a client, decided beside the type rather than at either
// handler that answers with one (#92).
crate::answers_json!(Series);

impl Series {
    /// Everything on this axis, summed — what a density ribbon's bars add up
    /// to, and what a search's own total has to equal.
    pub fn total(&self) -> i64 {
        self.values.iter().sum()
    }
}

/// `a / b` rounded up, for the positive values this module divides.
///
/// `i64::div_ceil` is still unstable, and the alternative — flooring and adding
/// one when there is a remainder — is the kind of arithmetic that is wrong by
/// one for exactly the inputs nobody tries by hand.
fn ceil_div(a: i64, b: i64) -> i64 {
    a / b + i64::from(a % b != 0)
}

/// The bucket a timestamp column falls in, as SQL.
///
/// The other half of [`Axis::index_of`], and the reason both are in this file.
/// Written as plain arithmetic rather than a date function so the two dialects
/// compute it identically (ADR-0003) and so the origin can be an arbitrary
/// instant — which is what lets the heatmap ask for buckets aligned to a
/// Listener's local midnight rather than to UTC's.
pub fn bucket_expr<C>(column: C, axis: &Axis) -> sea_orm::sea_query::SimpleExpr
where
    C: sea_orm::ColumnTrait,
{
    use sea_orm::IntoSimpleExpr;

    column
        .into_simple_expr()
        .sub(axis.from_ms())
        .div(axis.bucket_ms())
}

/// What [`bucket_expr`] is selected as, and what an aggregate over it groups by.
pub const BUCKET: &str = "bucket";

/// The grouping key, as the **output column's name** rather than as the
/// expression again.
///
/// This is a dialect difference and it cost a release to find. sea-orm renders
/// `bucket_expr`'s origin and width as *bound parameters*, so repeating the
/// expression in the `GROUP BY` emits `(at - $3) / $4` against a select list
/// holding `(at - $1) / $2`. SQLite compares those as equal and answers; Postgres
/// compares parameter *nodes*, decides the grouped expression is a different
/// one, and refuses the whole query with `column "…" must appear in the GROUP BY
/// clause` — so every chart in the app 500s on one of the two supported
/// databases and works perfectly on the other.
///
/// Grouping by the name sidesteps it entirely: both dialects resolve a bare name
/// in `GROUP BY` against the select list's aliases, and there is one expression
/// in the statement rather than two that have to be recognised as the same.
/// Neither `calls` nor `listener_samples` has a column called `bucket`, which is
/// the only thing that could shadow it.
pub fn bucket_group() -> sea_orm::sea_query::SimpleExpr {
    use sea_orm::sea_query::{Alias, Expr};

    Expr::col(Alias::new(BUCKET)).into()
}

// ---------------------------------------------------------------------------
// Reading an axis out of a query string
// ---------------------------------------------------------------------------

/// How finely a chart wants its range cut, read off a query string.
///
/// `bucketMs` wins over `buckets` because it is the stricter request: a caller
/// naming a width needs *that* width (the heatmap's fold into local hours is
/// only exact on hour edges), where a caller naming a count is really
/// describing how many pixels it has. Neither is refused for being
/// unreasonable — [`Axis::over`] bounds the array and the answer reports the
/// width it actually used, so a client is told rather than turned away.
///
/// Shared by both charts, so the two cannot come to disagree about what
/// `bucketMs` means.
pub(crate) fn parse_grain(params: &HashMap<String, String>) -> Filtered<Grain> {
    let read = Params::new(params);

    match read.number("bucketMs")? {
        Some(width) => Ok(Grain::Width(width)),
        None => Ok(Grain::Count(
            read.count("buckets")?.map_or(DEFAULT_BUCKETS, |buckets| {
                buckets.min(MAX_BUCKETS as u64) as usize
            }),
        )),
    }
}

/// The whole axis, for a chart whose subject has no extent of its own to derive
/// one from.
///
/// [`crate::archive::call_activity`] does not use this: a density ribbon over an
/// undated search has to cover the Archive's own reach under those filters,
/// which is a question only the database can answer. A listener chart has no
/// such question — the table is dense, one row a minute — so an absent bound is
/// simply `default_span_ms` back from now.
pub(crate) fn parse_axis(
    params: &HashMap<String, String>,
    now_ms: i64,
    default_span_ms: i64,
) -> Filtered<Axis> {
    let read = Params::new(params);
    let last_ms = read.time("before")?.unwrap_or(now_ms);
    let from_ms = read
        .time("after")?
        .unwrap_or(last_ms.saturating_sub(default_span_ms).saturating_add(1));

    Ok(Axis::over(from_ms, last_ms, parse_grain(params)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// A count grain gets about the buckets it asked for, and the axis always
    /// covers the whole range it was given — the last instant included, never
    /// falling off the end of the array.
    #[rstest]
    // An exact division: ten seconds into ten buckets is a bucket a second.
    #[case(0, 9_999, Grain::Count(10), 1_000, 10)]
    // A range that does not divide evenly rounds the *bucket* up, so the axis
    // still covers the end and the count comes in under what was asked for.
    #[case(0, 10_500, Grain::Count(10), 1_051, 10)]
    // One bucket is a legitimate answer.
    #[case(0, 5, Grain::Count(1), 6, 1)]
    // A single instant is one bucket, not zero.
    #[case(7, 7, Grain::Count(50), 1, 1)]
    // Backwards is nonsense from a query string, and reads as one bucket.
    #[case(1_000, 0, Grain::Count(10), 1, 1)]
    fn a_count_grain_covers_the_whole_range(
        #[case] from_ms: i64,
        #[case] last_ms: i64,
        #[case] grain: Grain,
        #[case] bucket_ms: i64,
        #[case] buckets: usize,
    ) {
        let axis = Axis::over(from_ms, last_ms, grain);

        assert_eq!(axis.bucket_ms(), bucket_ms);
        assert_eq!(axis.buckets(), buckets);
        assert_eq!(axis.from_ms(), from_ms);
        assert!(
            axis.to_ms() > last_ms || last_ms < from_ms,
            "the last instant is on the axis"
        );
    }

    /// A width grain is honoured exactly, because the heatmap's fold into local
    /// hours is only right when a bucket really is an hour.
    #[test]
    fn a_width_grain_is_honoured_exactly() {
        const HOUR: i64 = 3_600_000;
        let axis = Axis::over(1_000, 1_000 + 48 * HOUR - 1, Grain::Width(HOUR));

        assert_eq!(axis.bucket_ms(), HOUR);
        assert_eq!(axis.buckets(), 48);
        assert_eq!(axis.to_ms(), 1_000 + 48 * HOUR);
    }

    /// The bound is on the *array*, so a grain too fine for the range is
    /// widened — and the axis reports the width it actually used, which is what
    /// stops a client drawing hourly labels over three-hourly bars.
    #[test]
    fn a_grain_finer_than_the_cap_is_widened_and_says_so() {
        let span = MAX_BUCKETS as i64 * 10;
        let axis = Axis::over(0, span - 1, Grain::Width(1));

        assert!(axis.buckets() <= MAX_BUCKETS, "{} buckets", axis.buckets());
        assert_eq!(axis.bucket_ms(), 10, "widened to fit, not truncated");
        assert!(axis.to_ms() >= span, "and still covers the range");
    }

    /// The same, asked for as a count — a client wanting a bar per millisecond
    /// gets a bounded array rather than a Pi serializing a million zeroes.
    #[test]
    fn an_unreasonable_bucket_count_is_bounded() {
        let axis = Axis::over(0, 1_000_000, Grain::Count(usize::MAX));

        assert!(axis.buckets() <= MAX_BUCKETS, "{} buckets", axis.buckets());
    }

    /// A zero-width grain is read as one millisecond rather than dividing by
    /// zero — this is reached from a query string.
    #[rstest]
    #[case(Grain::Width(0))]
    #[case(Grain::Width(-5))]
    #[case(Grain::Count(0))]
    fn a_nonsense_grain_still_produces_an_axis(#[case] grain: Grain) {
        let axis = Axis::over(0, 99, grain);

        assert!(axis.bucket_ms() >= 1);
        assert!(axis.buckets() >= 1 && axis.buckets() <= MAX_BUCKETS);
    }

    /// Where an instant lands. The edges are what matter: a bucket owns its
    /// first instant and not its last, so nothing is counted twice.
    #[test]
    fn an_instant_falls_in_exactly_one_bucket() {
        let axis = Axis::over(1_000, 3_999, Grain::Width(1_000));

        assert_eq!(axis.buckets(), 3);
        assert_eq!(axis.index_of(1_000), Some(0));
        assert_eq!(axis.index_of(1_999), Some(0));
        assert_eq!(axis.index_of(2_000), Some(1));
        assert_eq!(axis.index_of(3_999), Some(2));
        // Off both ends.
        assert_eq!(axis.index_of(999), None);
        assert_eq!(axis.index_of(4_000), None);
    }

    /// Every instant the axis claims to cover is on it, and every bucket is
    /// reachable — the property `index_of` and `to_ms` have to agree on, and
    /// the one a hand-written edge table cannot cover.
    #[test]
    fn every_instant_on_the_axis_has_a_bucket() {
        use proptest::prelude::*;

        proptest!(|(from in -1_000_000_000_i64..1_000_000_000, span in 1_i64..100_000, buckets in 1_usize..200)| {
            let axis = Axis::over(from, from + span - 1, Grain::Count(buckets));

            prop_assert!(axis.index_of(axis.from_ms()) == Some(0));
            prop_assert!(axis.index_of(axis.to_ms() - 1) == Some(axis.buckets() - 1));
            prop_assert!(axis.index_of(axis.from_ms() - 1).is_none());
            prop_assert!(axis.index_of(axis.to_ms()).is_none());
            // The axis covers the range it was built over, and no more than one
            // bucket beyond it.
            prop_assert!(axis.to_ms() >= from + span);
            prop_assert!(axis.to_ms() - axis.bucket_ms() < from + span);
        });
    }

    /// A grouped query answers only about the buckets that had something in
    /// them; the empty ones are the chart's business and are filled here.
    #[test]
    fn the_empty_buckets_are_filled_in() {
        let axis = Axis::over(0, 4_999, Grain::Width(1_000));

        let series = axis.series([(0, 3), (3, 7)]);

        assert_eq!(series.values, vec![3, 0, 0, 7, 0]);
        assert_eq!(series.from_ms, 0);
        assert_eq!(series.to_ms, 5_000);
        assert_eq!(series.bucket_ms, 1_000);
        assert_eq!(series.total(), 10);
    }

    /// A bucket off the axis is dropped, never folded into an edge: a bar
    /// showing traffic that happened somewhere else is worse than a bar showing
    /// none.
    #[test]
    fn a_bucket_off_the_axis_is_dropped() {
        let axis = Axis::over(0, 2_999, Grain::Width(1_000));

        let series = axis.series([(-1, 5), (3, 9), (1, 2)]);

        assert_eq!(series.values, vec![0, 2, 0]);
    }
}
