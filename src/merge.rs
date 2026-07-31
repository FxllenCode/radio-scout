//! Channel merge's arithmetic (#45, spec US 15–18): which entity owns a Ref.
//!
//! Resolution itself is two SQL lookups ([`crate::db::repo::resolve_talkgroup`],
//! [`crate::db::repo::resolve_unit`]) because the answer lives in the database.
//! What lives *here* is the part that has to be decided the same way everywhere
//! and can be reasoned about without one: what a **Range** contains, when two
//! Ranges collide, and which of several claims on a Ref wins.
//!
//! It is separated for a reason that is not tidiness. A Range is written by an
//! operator and read by ingest, and those are different code paths months apart:
//! the writer must refuse `1200-1299` beside `1250-1350`, because a Ref inside
//! both is owned by whichever row the query happened to return first — a bug
//! that would present as one radio's Calls attributing to two different
//! apparatus depending on the day. The uniqueness index that stops this for
//! member *Refs* cannot express it for spans, so the guarantee has to be a
//! function, and a function is a thing tests can be exhaustive about.

/// A contiguous span of Refs owned as member Refs (CONTEXT.md: **Range**) —
/// `unitFrom..unitTo`, both ends **inclusive**.
///
/// Inclusive because the domain is inclusive: an operator writing `1200-1299`
/// means both of those radios. An exclusive end would also make the empty span
/// expressible, and an empty span in a table of ownership reads as owning one
/// Ref while owning none.
///
/// A lone member Ref is a Range of one (`from == to`), which is why Units need
/// only this one table: a fleet's block and its odd spare are the same sentence.
///
/// [`Range::contains`] has no caller outside the tests, and deliberately: the
/// production containment test is a SQL predicate in
/// [`crate::db::repo::resolve_unit`], which nothing in Rust can be exhaustive
/// about. Its job here is to be the *definition* those tests hold `overlaps`
/// to — the property an operator cares about ("some Ref belongs to both")
/// rather than the arithmetic that decides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Range {
    from: i64,
    to: i64,
}

impl Range {
    /// A Range over both Refs, whichever order they were given in.
    ///
    /// Reversed input is normalized rather than refused. `1299-1200` is a typo
    /// with exactly one sensible reading, and the alternative — storing it
    /// verbatim — is a row that owns nothing and silently never matches, which
    /// an operator would debug by staring at correct-looking configuration.
    pub fn new(from: i64, to: i64) -> Self {
        Range {
            from: from.min(to),
            to: from.max(to),
        }
    }

    pub fn from(&self) -> i64 {
        self.from
    }

    pub fn to(&self) -> i64 {
        self.to
    }

    /// Does this Range own `r`?
    pub fn contains(&self, r: i64) -> bool {
        self.from <= r && r <= self.to
    }

    /// Do these two Ranges claim any Ref in common?
    ///
    /// The test is deliberately the two-comparison one rather than "does either
    /// endpoint of one fall inside the other" — the latter is the version people
    /// write from memory, and it answers `false` for a Range wholly containing
    /// the other, which is the case an operator is most likely to create by
    /// widening a block they already had.
    pub fn overlaps(&self, other: &Range) -> bool {
        self.from <= other.to && other.from <= self.to
    }
}

/// The first Range in `existing` that `candidate` collides with, if any.
///
/// Returned rather than a boolean because the caller's whole job is to tell the
/// operator *which* of their Ranges is in the way; "that overlaps something" is
/// not an actionable sentence about a fleet with forty of them.
pub fn first_overlap(existing: &[Range], candidate: &Range) -> Option<Range> {
    existing
        .iter()
        .find(|range| range.overlaps(candidate))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    /// The boundaries, named one at a time. An off-by-one at either end of a
    /// Range is the whole failure mode here, and it is invisible in the middle.
    #[rstest]
    #[case::below(1199, false)]
    #[case::first(1200, true)]
    #[case::inside(1250, true)]
    #[case::last(1299, true)]
    #[case::above(1300, false)]
    fn a_range_owns_both_of_its_endpoints_and_nothing_beyond(#[case] r: i64, #[case] owned: bool) {
        assert_eq!(Range::new(1200, 1299).contains(r), owned, "for {r}");
    }

    /// The case the naive endpoint-in-endpoint test gets wrong: an operator
    /// widening a block they already had.
    #[test]
    fn a_range_wholly_containing_another_overlaps_it() {
        let wide = Range::new(1000, 2000);
        let inner = Range::new(1200, 1299);
        assert!(wide.overlaps(&inner));
        assert!(inner.overlaps(&wide));
    }

    #[test]
    fn a_reversed_range_is_read_the_only_way_it_could_be_meant() {
        assert_eq!(Range::new(1299, 1200), Range::new(1200, 1299));
    }

    #[test]
    fn an_overlap_is_reported_as_the_range_that_is_in_the_way() {
        let existing = [Range::new(1, 99), Range::new(1200, 1299)];
        assert_eq!(
            first_overlap(&existing, &Range::new(1250, 1350)),
            Some(Range::new(1200, 1299))
        );
        assert_eq!(first_overlap(&existing, &Range::new(500, 600)), None);
    }

    proptest! {
        /// However a Range is written down, it owns exactly the Refs between its
        /// ends. Asserted against an independent definition — Rust's own
        /// inclusive range — rather than against the implementation's own
        /// comparison, which would pass by construction.
        #[test]
        fn a_range_owns_exactly_the_refs_between_its_ends(
            a in -1_000i64..1_000,
            b in -1_000i64..1_000,
            r in -1_100i64..1_100,
        ) {
            let range = Range::new(a, b);
            prop_assert_eq!(range.contains(r), (a.min(b)..=a.max(b)).contains(&r));
        }

        /// Overlap is symmetric, and it is exactly "some Ref belongs to both".
        /// The second half is the one that matters: it pins the predicate to the
        /// property an operator cares about rather than to its arithmetic, so a
        /// rewritten comparison still has to mean the same thing.
        #[test]
        fn two_ranges_overlap_exactly_when_some_ref_belongs_to_both(
            a in -50i64..50, b in -50i64..50,
            c in -50i64..50, d in -50i64..50,
        ) {
            let (left, right) = (Range::new(a, b), Range::new(c, d));
            let shared = (-60i64..60).any(|r| left.contains(r) && right.contains(r));

            prop_assert_eq!(left.overlaps(&right), shared);
            prop_assert_eq!(left.overlaps(&right), right.overlaps(&left), "symmetric");
        }

        /// **Resolution is deterministic on a set that was admitted.** This is
        /// the property the whole write-time overlap check exists to buy: if
        /// every Range was accepted against the ones before it, then no Ref is
        /// claimed twice, and which row the database returns first cannot
        /// change whose Call it is.
        #[test]
        fn a_set_built_through_the_overlap_check_never_owns_a_ref_twice(
            spans in prop::collection::vec((-50i64..50, 0i64..12), 0..8),
            r in -60i64..60,
        ) {
            let mut admitted: Vec<Range> = Vec::new();
            for (start, width) in spans {
                let candidate = Range::new(start, start + width);
                if first_overlap(&admitted, &candidate).is_none() {
                    admitted.push(candidate);
                }
            }

            let owners = admitted.iter().filter(|range| range.contains(r)).count();
            prop_assert!(owners <= 1, "{r} claimed by {owners} of {admitted:?}");
        }
    }
}
