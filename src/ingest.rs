//! Ingest: the rdio-scanner-compatible `POST /api/call-upload` endpoint.
//!
//! Byte-compatibility is load-bearing (ADR-0001): recorders branch on the exact
//! response strings and status codes, verified against the rdio-scanner server
//! source (`api.go`, `parsers.go`) and the SDRTrunk client:
//! - success: HTTP 200 `Call imported successfully.\n`
//! - duplicate: HTTP 200 `duplicate call rejected\n` (SDRTrunk reads the body only on 200, then drops without retry)
//! - no talkgroup: HTTP 417 `Incomplete call data: no talkgroup\n`
//! - bad key: HTTP 401 `Invalid API key for system <s> talkgroup <t>.\n`
//!
//! The serialized pipeline (ADR-0001): authorize -> validate -> dedup -> write
//! audio object -> insert DB row (in a transaction) -> emit to the live feed.
//! Auto-populate enrichment (blacklist, default Group/Tag) is #8.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Multipart, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use sea_orm::TransactionTrait;
use serde::Deserialize;

use crate::AppState;
use crate::db::repo::{self, NewCall, NewCallFrequency, NewCallUnit};

const CALL_IMPORTED: &str = "Call imported successfully.\n";
const DUPLICATE_REJECTED: &str = "duplicate call rejected\n";

/// Ingest tuning. Ticket #17 populates this from TOML/CLI.
#[derive(Debug, Clone)]
pub struct IngestConfig {
    /// Duplicate-detection window in milliseconds (rdio's default is ~500ms).
    pub dedup_window_ms: i64,
}

impl Default for IngestConfig {
    fn default() -> Self {
        IngestConfig {
            dedup_window_ms: 500,
        }
    }
}

/// Raw multipart fields, collected before validation. Arrays stay as raw JSON
/// text until we build the call.
#[derive(Default)]
struct RawUpload {
    key: Option<String>,
    system: Option<String>,
    system_label: Option<String>,
    talkgroup: Option<String>,
    talkgroup_label: Option<String>,
    talkgroup_group: Option<String>,
    talkgroup_groups: Option<String>,
    talkgroup_tag: Option<String>,
    frequency: Option<String>,
    frequencies: Option<String>,
    source: Option<String>,
    sources: Option<String>,
    unit: Option<String>,
    units: Option<String>,
    patches: Option<String>,
    date_time: Option<String>,
    timestamp: Option<String>,
    audio: Option<Vec<u8>>,
    audio_name: Option<String>,
    audio_mime: Option<String>,
}

/// `POST /api/call-upload` — accept a call from a recorder.
pub async fn call_upload(State(state): State<AppState>, mut multipart: Multipart) -> Response {
    let mut upload = RawUpload::default();

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(_) => return incomplete("malformed multipart body"),
        };

        // Borrow-then-consume: capture metadata off the field before its body is
        // read (which consumes it).
        let name = field.name().unwrap_or("").to_string();
        if name == "audio" {
            upload.audio_name = field.file_name().map(str::to_string);
            upload.audio_mime = field.content_type().map(str::to_string);
            match field.bytes().await {
                Ok(bytes) => upload.audio = Some(bytes.to_vec()),
                Err(_) => return incomplete("could not read audio"),
            }
            continue;
        }

        let value = match field.text().await {
            Ok(value) => value,
            Err(_) => return incomplete("could not read field"),
        };
        match name.as_str() {
            "key" => upload.key = Some(value),
            "system" => upload.system = Some(value),
            "systemLabel" => upload.system_label = Some(value),
            "talkgroup" => upload.talkgroup = Some(value),
            "talkgroupLabel" => upload.talkgroup_label = Some(value),
            "talkgroupGroup" => upload.talkgroup_group = Some(value),
            "talkgroupGroups" => upload.talkgroup_groups = Some(value),
            "talkgroupTag" => upload.talkgroup_tag = Some(value),
            "frequency" => upload.frequency = Some(value),
            "frequencies" => upload.frequencies = Some(value),
            "source" => upload.source = Some(value),
            "sources" => upload.sources = Some(value),
            "unit" => upload.unit = Some(value),
            "units" => upload.units = Some(value),
            "patches" | "patched_talkgroups" => upload.patches = Some(value),
            "dateTime" => upload.date_time = Some(value),
            "timestamp" => upload.timestamp = Some(value),
            // audioName/audioFilename and audioMime/audioType if sent as fields.
            "audioName" | "audioFilename" => upload.audio_name = Some(value),
            "audioMime" | "audioType" => upload.audio_mime = Some(value),
            _ => {} // ignore fields we don't model in v1 (e.g. site)
        }
    }

    // A talkgroup is mandatory (the load-bearing health-check string).
    let Some(talkgroup_ref) = upload.talkgroup.as_deref().and_then(parse_i64) else {
        return incomplete("no talkgroup");
    };
    let audio = match upload.audio.take() {
        Some(audio) if !audio.is_empty() => audio,
        _ => return incomplete("no audio"),
    };
    let system_ref = upload.system.as_deref().and_then(parse_i64).unwrap_or(0);

    // Auth (ADR-0008): recorders always require a valid, in-scope API key.
    let key = upload.key.as_deref().unwrap_or("");
    match repo::authorize_ingest(&state.db, key, system_ref).await {
        Ok(true) => {}
        Ok(false) => {
            return (
                StatusCode::UNAUTHORIZED,
                format!("Invalid API key for system {system_ref} talkgroup {talkgroup_ref}.\n"),
            )
                .into_response();
        }
        Err(err) => return server_error("auth", err),
    }

    let call_at_ms = parse_call_time(upload.timestamp.as_deref(), upload.date_time.as_deref())
        .unwrap_or_else(now_ms);

    // Dedup (ADR-0001): same System + Talkgroup within the window.
    match repo::is_duplicate_call(
        &state.db,
        system_ref,
        talkgroup_ref,
        call_at_ms,
        state.ingest.dedup_window_ms,
    )
    .await
    {
        Ok(true) => return (StatusCode::OK, DUPLICATE_REJECTED).into_response(),
        Ok(false) => {}
        Err(err) => return server_error("dedup", err),
    }

    // Key is sharded by a two-char prefix so no directory grows unbounded.
    let uuid = uuid::Uuid::new_v4().simple().to_string();
    let object_key = format!(
        "{}/{}.{}",
        &uuid[0..2],
        uuid,
        audio_extension(&upload.audio_name)
    );

    let new_call = NewCall {
        system_ref,
        system_label: upload.system_label,
        talkgroup_ref,
        talkgroup_label: upload.talkgroup_label,
        talkgroup_tag: upload.talkgroup_tag,
        talkgroup_groups: parse_groups(upload.talkgroup_group, upload.talkgroup_groups),
        call_at_ms,
        frequency: upload.frequency.as_deref().and_then(parse_i64),
        source_ref: upload.source.as_deref().and_then(parse_i64),
        object_key: object_key.clone(),
        audio_mime: upload.audio_mime,
        audio_name: upload.audio_name,
        duration_ms: None,
        patches: parse_patches(upload.patches.as_deref()),
        units: parse_units(
            upload.units.as_deref(),
            upload.sources.as_deref(),
            upload.unit.as_deref(),
        ),
        frequencies: parse_frequencies(upload.frequencies.as_deref()),
    };

    // Write the audio object first (ADR-0001); a failed DB insert afterward leaves
    // an orphan the GC sweep reclaims (#10).
    if let Err(err) = state
        .audio
        .put(&object_key, bytes::Bytes::from(audio))
        .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not store audio: {err}\n"),
        )
            .into_response();
    }

    // Insert the row (+ children) atomically.
    let now = now_ms();
    let call = match insert_in_txn(&state.db, &new_call, now).await {
        Ok(call) => call,
        Err(err) => return server_error("store call", err),
    };

    // Emit to the live feed.
    match repo::stored_call(&state.db, call.id).await {
        Ok(Some(view)) => state.live.publish(Arc::new(view)),
        Ok(None) => {}
        Err(err) => return server_error("build call view", err),
    }

    (StatusCode::OK, CALL_IMPORTED).into_response()
}

async fn insert_in_txn(
    db: &sea_orm::DatabaseConnection,
    new_call: &NewCall,
    now_ms: i64,
) -> Result<crate::db::entities::call::Model, sea_orm::DbErr> {
    let txn = db.begin().await?;
    let call = repo::insert_call(&txn, new_call, now_ms).await?;
    txn.commit().await?;
    Ok(call)
}

/// The rdio-scanner incomplete-data response: HTTP 417 + `Incomplete call data: <reason>\n`.
fn incomplete(reason: &str) -> Response {
    (
        StatusCode::EXPECTATION_FAILED,
        format!("Incomplete call data: {reason}\n"),
    )
        .into_response()
}

fn server_error(stage: &str, err: sea_orm::DbErr) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("ingest {stage} failed: {err}\n"),
    )
        .into_response()
}

/// Current unix time in milliseconds.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Parse a decimal integer field, tolerating surrounding whitespace.
fn parse_i64(value: &str) -> Option<i64> {
    value.trim().parse().ok()
}

/// The audio object-key extension, from the uploaded filename (default `wav`).
fn audio_extension(name: &Option<String>) -> String {
    name.as_deref()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .filter(|ext| !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "wav".to_string())
}

/// Resolve the call time: `timestamp` is unix **milliseconds**; `dateTime` is
/// RFC3339 or unix **seconds** (per rdio's `api.md`).
fn parse_call_time(timestamp: Option<&str>, date_time: Option<&str>) -> Option<i64> {
    if let Some(ms) = timestamp.and_then(parse_i64) {
        return Some(ms);
    }
    let date_time = date_time?.trim();
    if let Some(seconds) = parse_i64(date_time) {
        return Some(seconds * 1000);
    }
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;
    OffsetDateTime::parse(date_time, &Rfc3339)
        .ok()
        .map(|dt| (dt.unix_timestamp_nanos() / 1_000_000) as i64)
}

/// Combine the single `talkgroupGroup` and comma-separated `talkgroupGroups`.
fn parse_groups(single: Option<String>, multiple: Option<String>) -> Vec<String> {
    let mut groups = Vec::new();
    if let Some(g) = single {
        let g = g.trim();
        if !g.is_empty() {
            groups.push(g.to_string());
        }
    }
    if let Some(list) = multiple {
        for g in list.split(',') {
            let g = g.trim();
            if !g.is_empty() && !groups.iter().any(|existing| existing == g) {
                groups.push(g.to_string());
            }
        }
    }
    groups
}

/// Parse the `patches` / `patched_talkgroups` array (numbers or numeric strings).
fn parse_patches(raw: Option<&str>) -> Vec<i64> {
    let Some(raw) = raw else { return Vec::new() };
    let Ok(values) = serde_json::from_str::<Vec<serde_json::Value>>(raw) else {
        return Vec::new();
    };
    values
        .into_iter()
        .filter_map(|v| v.as_i64().or_else(|| v.as_str().and_then(parse_i64)))
        .collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FreqJson {
    #[serde(default)]
    freq: f64,
    #[serde(default)]
    pos: f64,
    #[serde(default)]
    len: f64,
    #[serde(default)]
    dbm: Option<f64>,
    #[serde(default)]
    error_count: Option<i64>,
    #[serde(default)]
    spike_count: Option<i64>,
}

fn parse_frequencies(raw: Option<&str>) -> Vec<NewCallFrequency> {
    let Some(raw) = raw else { return Vec::new() };
    serde_json::from_str::<Vec<FreqJson>>(raw)
        .unwrap_or_default()
        .into_iter()
        .map(|f| NewCallFrequency {
            freq: f.freq as i64,
            pos_ms: Some((f.pos * 1000.0) as i64),
            len_ms: Some((f.len * 1000.0) as i64),
            dbm: f.dbm,
            error_count: f.error_count.map(|n| n as i32),
            spike_count: f.spike_count.map(|n| n as i32),
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnitJson {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    offset: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceJson {
    #[serde(default)]
    src: i64,
    #[serde(default)]
    pos: f64,
    #[serde(default)]
    tag: Option<String>,
}

/// Units heard, from `units[]` (rdio-native), `sources[]` (Trunk Recorder), or a
/// singular `unit`.
fn parse_units(units: Option<&str>, sources: Option<&str>, unit: Option<&str>) -> Vec<NewCallUnit> {
    if let Some(raw) = units
        && let Ok(list) = serde_json::from_str::<Vec<UnitJson>>(raw)
    {
        return list
            .into_iter()
            .map(|u| NewCallUnit {
                unit_ref: u.id,
                label: u.label,
                offset_ms: Some((u.offset * 1000.0) as i64),
            })
            .collect();
    }
    if let Some(raw) = sources
        && let Ok(list) = serde_json::from_str::<Vec<SourceJson>>(raw)
    {
        return list
            .into_iter()
            .map(|s| NewCallUnit {
                unit_ref: s.src,
                label: s.tag,
                offset_ms: Some((s.pos * 1000.0) as i64),
            })
            .collect();
    }
    if let Some(unit_ref) = unit.and_then(parse_i64) {
        return vec![NewCallUnit {
            unit_ref,
            label: None,
            offset_ms: None,
        }];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_time_prefers_timestamp_millis() {
        assert_eq!(
            parse_call_time(Some("1669740338000"), Some("2022-11-29T18:05:38Z")),
            Some(1669740338000)
        );
    }

    #[test]
    fn call_time_parses_unix_seconds_datetime() {
        assert_eq!(
            parse_call_time(None, Some("1669740338")),
            Some(1669740338000)
        );
    }

    #[test]
    fn call_time_parses_rfc3339_with_millis() {
        let a = parse_call_time(None, Some("2022-11-29T18:05:38.000Z")).unwrap();
        let b = parse_call_time(None, Some("2022-11-29T18:05:38.500Z")).unwrap();
        assert_eq!(b - a, 500, "millisecond precision preserved");
        assert!(a > 1_600_000_000_000, "plausible 2022 timestamp");
    }

    #[test]
    fn frequencies_parse_from_json() {
        let f = parse_frequencies(Some(
            r#"[{"freq":774031250,"pos":0.0,"len":1.5,"dbm":-50,"errorCount":2,"spikeCount":1}]"#,
        ));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].freq, 774031250);
        assert_eq!(f[0].len_ms, Some(1500));
        assert_eq!(f[0].error_count, Some(2));
        assert_eq!(f[0].dbm, Some(-50.0));
    }

    #[test]
    fn units_from_units_sources_or_singular() {
        let from_units = parse_units(
            Some(r#"[{"id":4424000,"label":"Engine 1","offset":0.5}]"#),
            None,
            None,
        );
        assert_eq!(from_units[0].unit_ref, 4424000);
        assert_eq!(from_units[0].label.as_deref(), Some("Engine 1"));
        assert_eq!(from_units[0].offset_ms, Some(500));

        let from_sources =
            parse_units(None, Some(r#"[{"src":123,"pos":1.0,"tag":"Medic"}]"#), None);
        assert_eq!(from_sources[0].unit_ref, 123);
        assert_eq!(from_sources[0].label.as_deref(), Some("Medic"));

        let from_singular = parse_units(None, None, Some("999"));
        assert_eq!(from_singular.len(), 1);
        assert_eq!(from_singular[0].unit_ref, 999);
    }

    #[test]
    fn patches_parse_numbers_and_strings() {
        assert_eq!(parse_patches(Some("[100, 200]")), vec![100, 200]);
        assert_eq!(parse_patches(Some(r#"["300","400"]"#)), vec![300, 400]);
        assert_eq!(parse_patches(None), Vec::<i64>::new());
    }

    #[test]
    fn groups_combine_single_and_comma_list_without_dupes() {
        assert_eq!(
            parse_groups(Some("Fire".into()), Some("Fire, Law".into())),
            vec!["Fire".to_string(), "Law".to_string()]
        );
    }

    #[test]
    fn audio_extension_derives_or_defaults() {
        assert_eq!(audio_extension(&Some("call.m4a".into())), "m4a");
        assert_eq!(audio_extension(&Some("weird".into())), "wav");
        assert_eq!(audio_extension(&None), "wav");
    }
}
