//! Reading filters out of a query string, and answering with one page of what
//! matched.
//!
//! Shared by the archive search (#13) and the operator log (#30), which is the
//! whole reason it exists: two read surfaces that page and filter the same way
//! should not each have their own idea of what a blank value means, what a bad
//! one is called, or how `hasMore` is computed. The second surface copied the
//! first before this module did; the copy is what this replaces.
//!
//! Two conventions, both departures from rdio-scanner, which coerces whatever
//! it can and silently ignores the rest — so a typo returns the wrong rows and
//! says nothing:
//!
//! - **Bad input is an error, named by parameter.**
//! - **Blank is absent.** A client builds these queries from form state where
//!   "no filter" is the empty string, so `?system=` means every System.

use std::collections::HashMap;

use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::failure::Reason;

/// A filter that could not be read, in the one shape a handler returns.
///
/// Every reading below yields this rather than a bare `String`, so the three
/// read surfaces cannot each decide for themselves what a bad parameter is worth
/// on the wire — which is what they did before #92, in three copies of the same
/// four lines.
pub(crate) type Filtered<T> = Result<T, Reason>;

/// A request's query string, read as filters.
pub(crate) struct Params<'a>(&'a HashMap<String, String>);

impl<'a> Params<'a> {
    pub(crate) fn new(params: &'a HashMap<String, String>) -> Self {
        Params(params)
    }

    /// A parameter's text, trimmed — `None` when absent *or* blank.
    pub(crate) fn raw(&self, key: &str) -> Option<&'a str> {
        self.0
            .get(key)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
    }

    /// A signed number — a Ref, which is the recorder's own numbering and has
    /// no width we get to choose.
    pub(crate) fn number(&self, key: &str) -> Filtered<Option<i64>> {
        self.parsed(key, "an integer")
    }

    /// An unsigned count — a limit, an offset.
    pub(crate) fn count(&self, key: &str) -> Filtered<Option<u64>> {
        self.parsed(key, "a non-negative integer")
    }

    /// A time boundary as unix milliseconds (what we store) or an RFC3339 time
    /// (so a human or a script can hand-write a query — the client sends ms,
    /// since only it knows the listener's timezone).
    pub(crate) fn time(&self, key: &str) -> Filtered<Option<i64>> {
        self.raw(key)
            .map(|value| {
                parse_time_ms(value).ok_or_else(|| {
                    bad(format!(
                        "{key} must be unix milliseconds or an RFC3339 time"
                    ))
                })
            })
            .transpose()
    }

    /// A page size, defaulted and **clamped** rather than refused: one request
    /// must not be able to ask a Pi to serialize the whole table. Zero is read
    /// as "unset", never as "an empty page".
    pub(crate) fn limit(&self, default: u64, max: u64) -> Filtered<u64> {
        Ok(self
            .count("limit")?
            .filter(|limit| *limit > 0)
            .unwrap_or(default)
            .min(max))
    }

    /// How far into the results this page starts.
    pub(crate) fn offset(&self) -> Filtered<u64> {
        Ok(self.count("offset")?.unwrap_or(0))
    }

    fn parsed<T: std::str::FromStr>(&self, key: &str, expected: &str) -> Filtered<Option<T>> {
        self.raw(key)
            .map(|value| {
                value
                    .parse::<T>()
                    .map_err(|_| bad(format!("{key} must be {expected}")))
            })
            .transpose()
    }
}

/// Turn a filter down, saying which parameter was wrong.
pub(crate) fn bad(detail: impl Into<String>) -> Reason {
    Reason::BadQuery(detail.into())
}

/// A time boundary as unix milliseconds or an RFC3339 timestamp.
pub(crate) fn parse_time_ms(raw: &str) -> Option<i64> {
    if let Ok(ms) = raw.parse::<i64>() {
        return Some(ms);
    }
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;
    OffsetDateTime::parse(raw, &Rfc3339)
        .ok()
        .map(|dt| (dt.unix_timestamp_nanos() / 1_000_000) as i64)
}

/// One page of results, plus what the client needs to page through the rest.
///
/// `has_more` is computed here rather than by each caller — and never by the
/// client, which would have to redo the arithmetic in a second language.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub results: Vec<T>,
    /// Total matching the filters, ignoring the page window.
    pub count: u64,
    pub limit: u64,
    pub offset: u64,
    /// Whether another page follows.
    pub has_more: bool,
}

impl<T> Page<T> {
    /// One page of `results`, `offset` into `count` matches.
    pub(crate) fn new(results: Vec<T>, count: u64, limit: u64, offset: u64) -> Self {
        Page {
            has_more: offset + (results.len() as u64) < count,
            count,
            limit,
            offset,
            results,
        }
    }
}

/// How a page reaches a client — one decision, shared by both read surfaces,
/// so neither handler names a JSON wrapper (#92). Generic, so it is the one
/// answer the macro cannot spell.
impl<T: Serialize> IntoResponse for Page<T> {
    fn into_response(self) -> Response {
        axum::Json(self).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// A query string, as axum's `Query` extractor would hand it over.
    fn query(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    /// Blank is absent, because the form a client builds these from produces
    /// the empty string for "no filter".
    #[rstest]
    #[case("")]
    #[case("   ")]
    fn a_blank_value_is_no_filter_at_all(#[case] value: &str) {
        let params = query(&[("after", value)]);
        let params = Params::new(&params);

        assert_eq!(params.raw("after"), None);
        assert_eq!(params.time("after"), Ok(None));
        assert_eq!(params.count("after"), Ok(None));
        assert_eq!(params.number("after"), Ok(None));
    }

    /// Every reading names the parameter it refused *and* what it wanted, so a
    /// client gets a message it can act on rather than a bare 400 — the whole
    /// difference from rdio-scanner, which coerces and says nothing.
    #[test]
    fn a_bad_value_names_its_own_parameter() {
        let params = query(&[
            ("limit", "lots"),
            ("offset", "-1"),
            ("system", "eleven"),
            ("after", "yesterday"),
        ]);
        let params = Params::new(&params);

        assert_eq!(
            params.count("limit").expect_err("a bad limit").told(),
            "limit must be a non-negative integer"
        );
        assert_eq!(
            params.count("offset").expect_err("a bad offset").told(),
            "offset must be a non-negative integer"
        );
        assert_eq!(
            params.number("system").expect_err("a bad Ref").told(),
            "system must be an integer"
        );
        assert_eq!(
            params.time("after").expect_err("a bad boundary").told(),
            "after must be unix milliseconds or an RFC3339 time"
        );
    }

    /// A page is bounded whatever was asked for, and zero means "unset".
    #[rstest]
    #[case(&[], 100)]
    #[case(&[("limit", "10")], 10)]
    #[case(&[("limit", "0")], 100)]
    #[case(&[("limit", "100000")], 500)]
    fn a_page_is_defaulted_and_clamped(#[case] pairs: &[(&str, &str)], #[case] expected: u64) {
        let params = query(pairs);

        assert_eq!(Params::new(&params).limit(100, 500), Ok(expected));
    }

    #[rstest]
    // Unix milliseconds, which is what we store and what the client sends.
    #[case("1577836800000", Some(1_577_836_800_000))]
    #[case("-1", Some(-1))]
    // ...and RFC3339, so a query can be hand-written.
    #[case("2020-01-01T00:00:00Z", Some(1_577_836_800_000))]
    #[case("2020-01-01T00:00:00+01:00", Some(1_577_833_200_000))]
    #[case("yesterday", None)]
    #[case("2020-01-01", None)]
    #[case("", None)]
    fn a_boundary_is_milliseconds_or_a_timestamp(#[case] raw: &str, #[case] expected: Option<i64>) {
        assert_eq!(parse_time_ms(raw), expected);
    }

    /// `has_more` is the one piece of arithmetic both read surfaces would
    /// otherwise each get right separately — until one of them didn't.
    #[rstest]
    // A full page with more behind it.
    #[case(2, 5, 0, true)]
    // The last page: offset + shown reaches the total.
    #[case(2, 4, 2, false)]
    // Everything on one page.
    #[case(3, 3, 0, false)]
    // Nothing matched at all.
    #[case(0, 0, 0, false)]
    // An offset past the end returns nothing and claims nothing follows.
    #[case(0, 5, 9, false)]
    fn has_more_is_computed_once(
        #[case] shown: usize,
        #[case] count: u64,
        #[case] offset: u64,
        #[case] expected: bool,
    ) {
        let page = Page::new(vec![0_u8; shown], count, 100, offset);

        assert_eq!(page.has_more, expected);
        assert_eq!(page.results.len(), shown);
    }
}
