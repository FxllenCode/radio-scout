//! The operator log surface's read half (#30): `GET /api/admin/logs`.
//!
//! [`crate::logsink`] writes the rows; this serves them to the Settings → Logs
//! view, behind the admin session (#19) like everything else under
//! `/api/admin/`. An Instance's log names its recorders, its rejections and its
//! failures — that is configuration detail, not something a public instance
//! hands out.
//!
//! The shape is [`crate::archive`]'s, deliberately: one page of results with the
//! total behind it, `hasMore` computed here rather than by the client, filters
//! that are **named when they are wrong** rather than silently coerced, and a
//! blank value read as absent because a form's "no filter" is the empty string.
//! An operator who has already learned the archive search has learned this.
//!
//! Two departures from rdio-scanner's Logs page, both about being usable at 2am:
//!
//! - **`level` is a floor, not a match.** "Warnings and worse" is the question
//!   an operator actually has; rdio makes you look at one level at a time.
//! - **An event is its parts.** rdio stores a rendered sentence, so its page can
//!   only substring-match it. Ours carries `target`, the structured `fields`
//!   ADR-0011 rule 6 insists on, and #28's `requestId` — so the ref in an
//!   `internal error (ref: …)` a listener read out over the phone is findable
//!   without a shell, which is the whole point of the ref.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use tracing::Level;

use crate::AppState;
use crate::archive::{bad_request, parse_time_ms};
use crate::db::repo::{self, LogSearch};
use crate::failure::ServerError;

/// Page size when the client asks for none — a screenful and then some.
const DEFAULT_LIMIT: u64 = 100;

/// Hard ceiling on a page, so one request can't ask a Pi to serialize a month
/// of logging. Requests above it are clamped, not refused, and the response
/// reports the limit actually applied.
const MAX_LIMIT: u64 = 500;

/// The levels a stored event can have, loudest first — [`Level`]'s own
/// ordering, which is what makes `level=warn` mean "warnings and worse".
///
/// DEBUG and TRACE are absent because the sink cannot store them (ADR-0011 rule
/// 5, `LogSinkConfig::LEVELS`), so offering them as filters would offer an
/// operator a control that does nothing.
const STORED_LEVELS: [Level; 3] = [Level::ERROR, Level::WARN, Level::INFO];

/// One page of the operator log.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogPage {
    pub results: Vec<LogView>,
    /// Total events matching the filters, ignoring the page window.
    pub count: u64,
    pub limit: u64,
    pub offset: u64,
    /// Whether another page follows — saves the client doing the arithmetic.
    pub has_more: bool,
}

/// One stored event, as the Logs view reads it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogView {
    pub id: i64,
    pub at_ms: i64,
    pub level: String,
    pub target: String,
    pub message: String,
    /// The structured fields, as the object they were stored as. Parsed here so
    /// the client renders `reason=duplicate` without parsing JSON out of a
    /// string; a row whose text somehow isn't an object reads as absent rather
    /// than failing the page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<serde_json::Value>,
    /// #28's correlation id, when the event was logged during a request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl From<crate::db::entities::log_event::Model> for LogView {
    fn from(event: crate::db::entities::log_event::Model) -> Self {
        LogView {
            id: event.id,
            at_ms: event.at_ms,
            level: event.level,
            target: event.target,
            message: event.message,
            fields: event
                .fields
                .as_deref()
                .and_then(|text| serde_json::from_str(text).ok()),
            request_id: event.request_id,
        }
    }
}

/// `GET /api/admin/logs` — one page of the operator log, newest first.
pub async fn search(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let search = match parse_search(&params) {
        Ok(search) => search,
        Err(message) => return bad_request(&message),
    };

    match load_page(&state.db, &search).await {
        Ok(page) => axum::Json(page).into_response(),
        Err(err) => ServerError::new("search-logs", err).into_response(),
    }
}

/// The page and the total behind it.
async fn load_page(
    db: &sea_orm::DatabaseConnection,
    search: &LogSearch,
) -> Result<LogPage, sea_orm::DbErr> {
    let results: Vec<LogView> = repo::search_log_events(db, search)
        .await?
        .into_iter()
        .map(LogView::from)
        .collect();
    let count = repo::count_log_events(db, search).await?;

    Ok(LogPage {
        has_more: search.offset + (results.len() as u64) < count,
        count,
        limit: search.limit,
        offset: search.offset,
        results,
    })
}

/// Read the filters out of a query string, or say which parameter was wrong.
fn parse_search(params: &HashMap<String, String>) -> Result<LogSearch, String> {
    // Blank values are the "unset" the client's own form produces.
    let raw = |key: &str| {
        params
            .get(key)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
    };
    let time = |key: &str| -> Result<Option<i64>, String> {
        raw(key)
            .map(|value| {
                parse_time_ms(value)
                    .ok_or_else(|| format!("{key} must be unix milliseconds or an RFC3339 time"))
            })
            .transpose()
    };
    let count = |key: &str| -> Result<Option<u64>, String> {
        raw(key)
            .map(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| format!("{key} must be a non-negative integer"))
            })
            .transpose()
    };

    Ok(LogSearch {
        after_ms: time("after")?,
        before_ms: time("before")?,
        levels: raw("level").map(levels_from).transpose()?,
        limit: count("limit")?
            .filter(|limit| *limit > 0)
            .unwrap_or(DEFAULT_LIMIT)
            .min(MAX_LIMIT),
        offset: count("offset")?.unwrap_or(0),
    })
}

/// The stored level names a severity floor admits: `warn` -> `ERROR`, `WARN`.
///
/// A floor rather than a match, because "show me warnings" never means "and
/// hide the errors".
fn levels_from(floor: &str) -> Result<Vec<String>, String> {
    let floor = STORED_LEVELS
        .into_iter()
        .find(|level| level.as_str().eq_ignore_ascii_case(floor))
        .ok_or_else(|| {
            let names: Vec<String> = STORED_LEVELS
                .into_iter()
                .map(|level| level.as_str().to_ascii_lowercase())
                .collect();
            format!("level must be one of {} (got {floor:?})", names.join(", "))
        })?;
    Ok(STORED_LEVELS
        .into_iter()
        // `Level`'s ordering is by verbosity, so "at least as severe as the
        // floor" is `<=`.
        .filter(|level| *level <= floor)
        .map(|level| level.as_str().to_owned())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// Parse a query string the way axum's `Query` extractor would.
    fn params(query: &str) -> HashMap<String, String> {
        query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                (key.to_owned(), value.to_owned())
            })
            .collect()
    }

    /// A severity floor admits its own level and everything louder — never the
    /// other way round.
    #[rstest]
    #[case("error", &["ERROR"])]
    #[case("warn", &["ERROR", "WARN"])]
    #[case("info", &["ERROR", "WARN", "INFO"])]
    // The client sends what it was given; a level an operator typed in a URL
    // should not depend on their shift key.
    #[case("WARN", &["ERROR", "WARN"])]
    fn a_level_filter_admits_everything_at_least_as_severe(
        #[case] floor: &str,
        #[case] expected: &[&str],
    ) {
        assert_eq!(levels_from(floor).expect("a level"), expected);
    }

    /// A level the sink cannot store is not a filter — offering it would offer
    /// a control that silently does nothing (ADR-0011 rule 5).
    #[rstest]
    #[case("debug")]
    #[case("trace")]
    #[case("off")]
    #[case("chatty")]
    fn a_level_that_is_never_stored_is_refused(#[case] floor: &str) {
        let error = levels_from(floor).expect_err("refused");
        assert!(error.contains("level must be one of"), "{error}");
        assert!(error.contains("error, warn, info"), "{error}");
    }

    /// Blank is absent, not an error: the client builds this query from form
    /// state where "no filter" is the empty string.
    #[test]
    fn blank_filters_are_no_filter_at_all() {
        let search = parse_search(&params("level=&after=&before=&limit=&offset=")).expect("parse");

        assert_eq!(search.levels, None);
        assert_eq!(search.after_ms, None);
        assert_eq!(search.before_ms, None);
        assert_eq!(search.limit, DEFAULT_LIMIT);
        assert_eq!(search.offset, 0);
    }

    /// A page is bounded whatever was asked for, so one request can't make a Pi
    /// serialize a month of logging.
    #[rstest]
    #[case("limit=10", 10)]
    #[case("limit=100000", MAX_LIMIT)]
    // Zero is "unset", the archive's reading (#13) — never "an empty page".
    #[case("limit=0", DEFAULT_LIMIT)]
    #[case("", DEFAULT_LIMIT)]
    fn a_page_is_bounded(#[case] query: &str, #[case] expected: u64) {
        assert_eq!(parse_search(&params(query)).expect("parse").limit, expected);
    }

    /// Both spellings of a boundary, so a query can be hand-written.
    #[rstest]
    #[case("after=1577836800000", 1_577_836_800_000)]
    #[case("after=2020-01-01T00:00:00Z", 1_577_836_800_000)]
    fn a_boundary_may_be_milliseconds_or_a_timestamp(#[case] query: &str, #[case] expected: i64) {
        assert_eq!(
            parse_search(&params(query)).expect("parse").after_ms,
            Some(expected)
        );
    }

    /// Bad input is named by parameter — rdio-scanner coerces what it can and
    /// ignores the rest, so a typo quietly returns the wrong events.
    #[rstest]
    #[case("after=yesterday", "after")]
    #[case("before=soon", "before")]
    #[case("limit=lots", "limit")]
    #[case("offset=-1", "offset")]
    #[case("level=chatty", "level")]
    fn a_bad_filter_names_itself(#[case] query: &str, #[case] parameter: &str) {
        let error = parse_search(&params(query)).expect_err("refused");
        assert!(error.contains(parameter), "{error}");
    }
}
