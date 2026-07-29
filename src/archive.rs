//! Archive search — the read side of the archive (#13, spec US 24–27).
//!
//! rdio-scanner searches over its proprietary WebSocket and answers with bare
//! `{id, system, talkgroup, dateTime}` rows, so the client fetches every Call it
//! wants to show *again*, one at a time. Radio-Scout searches over plain HTTP
//! (`GET /api/calls`) and answers with the same denormalized [`StoredCall`] the
//! live feed delivers — a page renders and plays with no follow-up round trip,
//! and the query is cacheable, linkable, and reachable by anything that speaks
//! HTTP.
//!
//! The wire contract is [`SearchPage`] here plus [`FilterOptions`] in
//! [`crate::call`]; the queries behind them live in [`crate::db::repo`].

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::AppState;
use crate::call::{CallId, StoredCall};
use crate::db::repo::{self, CallSearch, CallSort};
use crate::failure::ServerError;

// The cascading filter-option view types live in `crate::call` beside
// `StoredCall`, so the data layer can build them without depending on this one.
pub use crate::call::{FilterOptions, SystemOption, TalkgroupOption};

/// Page size when the client asks for none. Big enough that a phone screen
/// scrolls for a while, small enough to stay snappy on a Pi.
const DEFAULT_LIMIT: u64 = 100;
/// Hard ceiling on a page, so one request can't ask the Pi to denormalize the
/// whole archive. Requests above it are clamped, not refused, and the response
/// reports the limit actually applied.
const MAX_LIMIT: u64 = 500;

/// One page of archive-search results.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPage {
    /// The Calls on this page, fully denormalized (no N+1 follow-up fetches).
    pub results: Vec<StoredCall>,
    /// Total Calls matching the filters, ignoring the page window.
    pub count: u64,
    pub limit: u64,
    pub offset: u64,
    /// Whether another page follows — saves the client doing the arithmetic.
    pub has_more: bool,
}

// ---------------------------------------------------------------------------
// Query parsing
// ---------------------------------------------------------------------------

/// Read the archive-search filters out of a query string, or say which
/// parameter was wrong.
///
/// Two deliberate departures from rdio-scanner, which coerces whatever it can
/// and silently ignores the rest (so a typo quietly returns the wrong Calls):
/// - **Bad input is an error**, named by parameter, not a silent default.
/// - **Blank is absent.** A client builds this query from form state where "no
///   filter" is the empty string, so `?system=` means every System.
fn parse_search(params: &HashMap<String, String>) -> Result<CallSearch, String> {
    // Blank values are the "unset" the client's own form produces.
    let raw = |key: &str| {
        params
            .get(key)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
    };
    let number = |key: &str| -> Result<Option<i64>, String> {
        raw(key)
            .map(|value| {
                value
                    .parse::<i64>()
                    .map_err(|_| format!("{key} must be an integer"))
            })
            .transpose()
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

    let sort = match raw("sort") {
        None | Some("newest") | Some("desc") => CallSort::Newest,
        Some("oldest") | Some("asc") => CallSort::Oldest,
        Some(other) => {
            return Err(format!(
                "sort must be one of newest, oldest, desc, asc (got {other:?})"
            ));
        }
    };

    Ok(CallSearch {
        after_ms: time("after")?,
        before_ms: time("before")?,
        system_ref: number("system")?,
        talkgroup_ref: number("talkgroup")?,
        group_name: raw("group").map(str::to_owned),
        tag_name: raw("tag").map(str::to_owned),
        sort,
        limit: count("limit")?
            .filter(|limit| *limit > 0)
            .unwrap_or(DEFAULT_LIMIT)
            .min(MAX_LIMIT),
        offset: count("offset")?.unwrap_or(0),
    })
}

/// A search boundary as unix milliseconds (what we store) or an RFC3339 time
/// (so a human or a script can hand-write a query — the client sends ms, since
/// only it knows the listener's timezone).
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

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/calls` — one page of archive-search results (spec US 24/25).
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
        Err(err) => ServerError::new("search-calls", err).into_response(),
    }
}

/// The three queries behind a result page: the page itself, its denormalized
/// view, and the total behind it.
async fn load_page(
    db: &sea_orm::DatabaseConnection,
    search: &CallSearch,
) -> Result<SearchPage, sea_orm::DbErr> {
    let rows = repo::search_calls(db, search).await?;
    let results = repo::stored_calls(db, &rows).await?;
    let count = repo::count_calls(db, search).await?;

    Ok(SearchPage {
        has_more: search.offset + (results.len() as u64) < count,
        count,
        limit: search.limit,
        offset: search.offset,
        results,
    })
}

/// `GET /api/calls/filters` — the cascading filter options for the filters
/// already chosen (spec US 24). `sort`/`limit`/`offset` are accepted and
/// ignored, so a client can reuse one query string for both endpoints.
pub async fn filters(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let search = match parse_search(&params) {
        Ok(search) => search,
        Err(message) => return bad_request(&message),
    };
    match repo::filter_options(&state.db, &search).await {
        Ok(options) => axum::Json(options).into_response(),
        Err(err) => ServerError::new("load-filter-options", err).into_response(),
    }
}

/// `GET /api/call/{id}/download` — the Call's audio as a named file attachment
/// (spec US 27).
///
/// Unlike [`crate::serve_audio`], this always proxies the bytes, even on an S3
/// backend that could redirect to a presigned URL: the browser would then save
/// the file under the opaque object key, which is precisely what this endpoint
/// exists to avoid. Downloads are occasional and a Call is seconds of audio, so
/// the proxy costs little.
pub async fn download(State(state): State<AppState>, Path(id): Path<CallId>) -> Response {
    let (view, audio_name) = match load_call(&state.db, id).await {
        Ok(Some(found)) => found,
        Ok(None) => return (StatusCode::NOT_FOUND, "call not found\n").into_response(),
        Err(err) => return ServerError::new("look-up-call", err).into_response(),
    };

    let bytes = match state.audio.get(&view.object_key).await {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return (StatusCode::NOT_FOUND, "audio not found\n").into_response(),
        Err(err) => return ServerError::new("read-audio", err).into_response(),
    };

    let filename = download_filename(&view, audio_name.as_deref());
    let mime = view
        .audio_mime
        .as_deref()
        .unwrap_or("application/octet-stream");

    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                header_value(mime, "application/octet-stream"),
            ),
            (
                header::CONTENT_DISPOSITION,
                header_value(
                    &format!("attachment; filename=\"{filename}\""),
                    "attachment",
                ),
            ),
        ],
        bytes,
    )
        .into_response()
}

/// A Call's denormalized view plus the recorder's own filename (the one column
/// the view doesn't carry, used to pick the download's extension).
async fn load_call(
    db: &sea_orm::DatabaseConnection,
    id: CallId,
) -> Result<Option<(StoredCall, Option<String>)>, sea_orm::DbErr> {
    let Some(call) = repo::find_call(db, id).await? else {
        return Ok(None);
    };
    let audio_name = call.audio_name.clone();
    Ok(repo::stored_calls(db, std::slice::from_ref(&call))
        .await?
        .pop()
        .map(|view| (view, audio_name)))
}

/// A header value from runtime text, falling back when the text can't be one.
/// Recorder-supplied MIME types reach this unvalidated, so the fallback is a
/// real path, not a formality.
fn header_value(raw: &str, fallback: &'static str) -> HeaderValue {
    HeaderValue::from_str(raw).unwrap_or(HeaderValue::from_static(fallback))
}

pub(crate) fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, format!("{message}\n")).into_response()
}

// ---------------------------------------------------------------------------
// Download filenames
// ---------------------------------------------------------------------------

/// The longest stem a download filename gets, so a pathological label can't
/// produce a name a filesystem rejects.
const MAX_STEM: usize = 120;

/// Name a downloaded Call after what it *is*: `System-Talkgroup-time.ext`.
///
/// rdio-scanner hands back the recorder's own filename, which names neither the
/// System nor the Talkgroup — a folder of them is unreadable. The recorder's
/// name is still consulted for the file *extension*, since it knows the real
/// container; the MIME type is the fallback.
fn download_filename(call: &StoredCall, audio_name: Option<&str>) -> String {
    let system = call
        .system_label
        .clone()
        .unwrap_or_else(|| call.system_ref.to_string());
    let talkgroup = call
        .talkgroup_label
        .clone()
        .unwrap_or_else(|| call.talkgroup_ref.to_string());
    let at_ms = call.timestamp.unwrap_or_default();
    let extension = download_extension(audio_name, call.audio_mime.as_deref());

    format!(
        "{}.{extension}",
        slug(&format!("{system}-{talkgroup}-{at_ms}"))
    )
}

/// The container extension for a download: the recorder's filename knows best,
/// the MIME type is the fallback, and `bin` is the last resort.
fn download_extension(audio_name: Option<&str>, mime: Option<&str>) -> String {
    let from_name = audio_name
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext.trim().to_ascii_lowercase())
        .filter(|ext| {
            (1..=8).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric())
        });
    if let Some(ext) = from_name {
        return ext;
    }

    // Strip any `; codecs=…` parameter before matching.
    let mime = mime
        .map(|mime| mime.split(';').next().unwrap_or_default().trim())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match mime.as_str() {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" => "m4a",
        "audio/aac" => "aac",
        "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/vnd.wave" => "wav",
        "audio/ogg" | "audio/opus" => "ogg",
        "audio/flac" | "audio/x-flac" => "flac",
        _ => "bin",
    }
    .to_string()
}

/// Reduce a label to something safe in a `Content-Disposition` header and on
/// every filesystem: ASCII word characters, dots, dashes and underscores, with
/// everything else collapsed to a single dash.
fn slug(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(MAX_STEM));
    for ch in raw.chars() {
        let keep = ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_');
        if keep {
            out.push(ch);
        } else if !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= MAX_STEM {
            break;
        }
    }
    let trimmed = out.trim_matches(['-', '.', '_']);
    if trimmed.is_empty() {
        "call".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    fn query(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn an_empty_query_is_the_default_page_newest_first() {
        let search = parse_search(&query(&[])).unwrap();
        assert_eq!(search.sort, CallSort::Newest);
        assert_eq!(search.limit, DEFAULT_LIMIT);
        assert_eq!(search.offset, 0);
        assert_eq!(search.after_ms, None);
        assert_eq!(search.before_ms, None);
        assert_eq!(search.system_ref, None);
        assert_eq!(search.talkgroup_ref, None);
        assert_eq!(search.group_name, None);
        assert_eq!(search.tag_name, None);
    }

    #[test]
    fn every_filter_is_read_and_whitespace_trimmed() {
        let search = parse_search(&query(&[
            ("after", " 1000 "),
            ("before", "2000"),
            ("system", "11"),
            ("talkgroup", "54241"),
            ("group", " Fire "),
            ("tag", "Fire Dispatch"),
            ("sort", "oldest"),
            ("limit", "25"),
            ("offset", "50"),
        ]))
        .unwrap();

        assert_eq!(search.after_ms, Some(1000));
        assert_eq!(search.before_ms, Some(2000));
        assert_eq!(search.system_ref, Some(11));
        assert_eq!(search.talkgroup_ref, Some(54241));
        assert_eq!(search.group_name.as_deref(), Some("Fire"));
        assert_eq!(search.tag_name.as_deref(), Some("Fire Dispatch"));
        assert_eq!(search.sort, CallSort::Oldest);
        assert_eq!(search.limit, 25);
        assert_eq!(search.offset, 50);
    }

    /// The client's form state renders "no filter" as an empty value; blank and
    /// whitespace-only must read as absent, not as a filter matching nothing.
    #[rstest]
    #[case("")]
    #[case("   ")]
    fn blank_values_are_absent(#[case] blank: &str) {
        let search = parse_search(&query(&[
            ("system", blank),
            ("talkgroup", blank),
            ("group", blank),
            ("tag", blank),
            ("after", blank),
            ("before", blank),
            ("sort", blank),
            ("limit", blank),
            ("offset", blank),
        ]))
        .unwrap();

        assert_eq!(search.system_ref, None);
        assert_eq!(search.talkgroup_ref, None);
        assert_eq!(search.group_name, None);
        assert_eq!(search.tag_name, None);
        assert_eq!(search.after_ms, None);
        assert_eq!(search.before_ms, None);
        assert_eq!(search.sort, CallSort::Newest);
        assert_eq!(search.limit, DEFAULT_LIMIT);
        assert_eq!(search.offset, 0);
    }

    #[rstest]
    #[case("newest", CallSort::Newest)]
    #[case("desc", CallSort::Newest)]
    #[case("oldest", CallSort::Oldest)]
    #[case("asc", CallSort::Oldest)]
    fn sort_spellings(#[case] raw: &str, #[case] expected: CallSort) {
        assert_eq!(
            parse_search(&query(&[("sort", raw)])).unwrap().sort,
            expected
        );
    }

    #[rstest]
    #[case("0", DEFAULT_LIMIT)] // zero means "unspecified", not "no rows"
    #[case("1", 1)]
    #[case("500", MAX_LIMIT)]
    #[case("501", MAX_LIMIT)] // clamped, not refused
    #[case("99999", MAX_LIMIT)]
    fn limit_defaults_and_clamps(#[case] raw: &str, #[case] expected: u64) {
        assert_eq!(
            parse_search(&query(&[("limit", raw)])).unwrap().limit,
            expected
        );
    }

    /// Every rejection names the parameter at fault — rdio-scanner silently
    /// ignores what it can't parse, so a typo returns plausible wrong results.
    #[rstest]
    #[case("system", "abc")]
    #[case("system", "1.5")]
    #[case("talkgroup", "twelve")]
    #[case("after", "yesterday")]
    #[case("after", "2026-13-45T00:00:00Z")]
    #[case("before", "")] // (blank is fine — see below; this case is overridden)
    #[case("sort", "sideways")]
    #[case("limit", "-1")]
    #[case("offset", "-1")]
    #[case("offset", "1e3")]
    fn malformed_values_are_named_in_the_error(#[case] key: &str, #[case] value: &str) {
        let result = parse_search(&query(&[(key, value)]));
        if value.is_empty() {
            assert!(result.is_ok(), "blank {key} is absent, not malformed");
            return;
        }
        let message = result.expect_err("should reject");
        assert!(
            message.contains(key),
            "error {message:?} should name {key:?}"
        );
    }

    #[rstest]
    #[case("0", Some(0))]
    #[case("1669740338000", Some(1_669_740_338_000))]
    #[case("-1000", Some(-1000))] // before the epoch is still a time
    #[case("2022-11-29T18:05:38Z", Some(1_669_745_138_000))]
    #[case("2022-11-29T13:05:38-05:00", Some(1_669_745_138_000))] // offsets honored
    #[case("2022-11-29", None)] // date-only is ambiguous without a timezone
    #[case("18:05:38", None)]
    #[case("", None)]
    #[case("nonsense", None)]
    fn time_parsing(#[case] raw: &str, #[case] expected: Option<i64>) {
        assert_eq!(parse_time_ms(raw), expected);
    }

    fn call() -> StoredCall {
        StoredCall {
            id: 42,
            system_ref: 11,
            system_label: Some("Butler County".into()),
            talkgroup_ref: 54241,
            talkgroup_label: Some("TDB A1".into()),
            talkgroup_group: None,
            talkgroup_tag: None,
            led: None,
            patches: vec![],
            frequency: None,
            source: None,
            date_time: None,
            timestamp: Some(1_669_740_338_000),
            audio_mime: Some("audio/mp4".into()),
            object_key: "ab/opaque-key.m4a".into(),
            audio_url: "/api/call/42/audio".into(),
        }
    }

    /// The name says what the Call *is* — System, Talkgroup, when — instead of
    /// the recorder's or the object store's opaque key.
    #[test]
    fn download_filename_describes_the_call() {
        assert_eq!(
            download_filename(&call(), Some("54241-1669740338_774031250.m4a")),
            "Butler-County-TDB-A1-1669740338000.m4a"
        );
    }

    /// With no labels curated yet, the Refs carry the identity.
    #[test]
    fn download_filename_falls_back_to_refs_and_the_mime_type() {
        let bare = StoredCall {
            system_label: None,
            talkgroup_label: None,
            audio_mime: Some("audio/x-wav".into()),
            ..call()
        };
        assert_eq!(download_filename(&bare, None), "11-54241-1669740338000.wav");
    }

    /// A Call whose labels are blank or unprintable still gets a safe name —
    /// no empty stem, no leading dot, no missing extension.
    #[test]
    fn download_filename_survives_a_call_with_no_usable_metadata() {
        let empty = StoredCall {
            system_ref: 0,
            system_label: Some(String::new()),
            talkgroup_label: Some("///".into()),
            timestamp: None,
            audio_mime: None,
            ..call()
        };
        // Only the (zero) call time survives the slug.
        assert_eq!(download_filename(&empty, None), "0.bin");
    }

    #[rstest]
    // The recorder's own filename knows the real container.
    #[case(Some("call.m4a"), Some("audio/x-wav"), "m4a")]
    #[case(Some("CALL.WAV"), None, "wav")]
    #[case(Some("a.b.opus"), None, "opus")]
    // Unusable names fall through to the MIME type...
    #[case(Some("noextension"), Some("audio/mpeg"), "mp3")]
    #[case(Some("call."), Some("audio/mp4"), "m4a")]
    #[case(Some("call.tar.gz!"), Some("audio/aac"), "aac")]
    #[case(Some("call.waaaaaaaaaay"), Some("audio/ogg"), "ogg")]
    #[case(None, Some("audio/flac"), "flac")]
    #[case(None, Some("audio/mp4; codecs=\"mp4a.40.2\""), "m4a")]
    #[case(None, Some("AUDIO/WAV"), "wav")]
    // ...and an unknown MIME type to a neutral last resort.
    #[case(None, Some("application/octet-stream"), "bin")]
    #[case(None, None, "bin")]
    fn extension_prefers_the_recorder_then_the_mime_type(
        #[case] name: Option<&str>,
        #[case] mime: Option<&str>,
        #[case] expected: &str,
    ) {
        assert_eq!(download_extension(name, mime), expected);
    }

    #[rstest]
    #[case("Butler County", "Butler-County")]
    #[case("Fire/EMS", "Fire-EMS")]
    #[case("a//b", "a-b")] // runs collapse to one dash
    #[case("../../etc/passwd", "etc-passwd")] // no traversal survives
    #[case("  spaced  ", "spaced")]
    #[case("Pompiers Sûreté", "Pompiers-S-ret")] // non-ASCII is replaced
    #[case("...", "call")] // nothing usable -> a name, never ""
    #[case("", "call")]
    #[case("_under_", "under")]
    fn slug_cases(#[case] raw: &str, #[case] expected: &str) {
        assert_eq!(slug(raw), expected);
    }

    /// A pathological label can't grow a filename past what a filesystem takes.
    #[test]
    fn slug_truncates_a_very_long_label() {
        assert_eq!(slug(&"a".repeat(500)).len(), MAX_STEM);
    }

    proptest! {
        /// Whatever a recorder or an operator put in a label, the slug is always
        /// a safe, non-empty, bounded filename component.
        #[test]
        fn slug_is_always_a_safe_filename_component(raw in ".{0,300}") {
            let slug = slug(&raw);
            prop_assert!(!slug.is_empty());
            prop_assert!(slug.len() <= MAX_STEM);
            prop_assert!(
                slug.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')),
                "unsafe characters in {slug:?}"
            );
            // A dot inside the stem is harmless ("Engine.1"); what must never
            // happen is a leading one — that's both a hidden file and the only
            // way `.` or `..` could become the whole component.
            prop_assert!(!slug.starts_with('.'), "hidden or traversal name {slug:?}");
            prop_assert!(!slug.ends_with('.'), "trailing dot in {slug:?}");
        }

        /// A download filename is always header-safe: printable ASCII with no
        /// quote to break out of `filename="…"`, and always extended.
        #[test]
        fn download_filename_is_always_header_safe(
            system in ".{0,40}",
            talkgroup in ".{0,40}",
            name in ".{0,40}",
            at_ms in any::<i64>(),
        ) {
            let filename = download_filename(
                &StoredCall {
                    system_label: Some(system),
                    talkgroup_label: Some(talkgroup),
                    timestamp: Some(at_ms),
                    ..call()
                },
                Some(&name),
            );
            let header = format!("attachment; filename=\"{filename}\"");
            prop_assert!(HeaderValue::from_str(&header).is_ok());
            prop_assert!(!filename.contains('"'));
            prop_assert!(!filename.contains('/') && !filename.contains('\\'));
            prop_assert!(filename.contains('.'), "no extension in {filename:?}");
        }
    }
}
