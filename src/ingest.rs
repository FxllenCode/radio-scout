//! Ingest: the rdio-scanner-compatible `POST /api/call-upload` endpoint.
//!
//! Byte-compatibility is load-bearing (ADR-0001): recorders branch on the exact
//! response strings and status codes, verified against the rdio-scanner server
//! source and the SDRTrunk client:
//!   - success  -> HTTP 200 `Call imported successfully.\n`
//!   - no tg    -> HTTP 417 `Incomplete call data: no talkgroup\n`
//!
//! Skeleton scope: the success and no-talkgroup paths, plus the serialized
//! pipeline (write audio object, then insert the row, then emit to the live
//! feed). Ticket #5 adds hashed API-key auth + dedup; #8 adds auto-populate.

use std::sync::Arc;

use axum::extract::{Multipart, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::AppState;
use crate::call::IngestedCall;

/// Exact rdio-scanner success body (period + trailing newline). SDRTrunk matches
/// with `.contains("Call imported successfully.")`.
const CALL_IMPORTED: &str = "Call imported successfully.\n";

/// Fields collected from the multipart body before we build the call.
#[derive(Default)]
struct RawUpload {
    key: Option<String>,
    system: Option<String>,
    system_label: Option<String>,
    talkgroup: Option<String>,
    talkgroup_label: Option<String>,
    talkgroup_group: Option<String>,
    talkgroup_tag: Option<String>,
    frequency: Option<String>,
    source: Option<String>,
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

        // Borrow-then-consume: capture the metadata off the field before its
        // body is read (which consumes it).
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
            "talkgroupTag" => upload.talkgroup_tag = Some(value),
            "frequency" => upload.frequency = Some(value),
            "source" => upload.source = Some(value),
            "dateTime" => upload.date_time = Some(value),
            "timestamp" => upload.timestamp = Some(value),
            _ => {} // ignore fields the skeleton doesn't model yet
        }
    }

    // Validate the load-bearing requirement: a talkgroup is mandatory.
    let talkgroup_ref = match upload.talkgroup.as_deref().and_then(parse_i64) {
        Some(tg) => tg,
        None => return incomplete("no talkgroup"),
    };

    let audio = match upload.audio {
        Some(audio) if !audio.is_empty() => audio,
        _ => return incomplete("no audio"),
    };

    let ingested = IngestedCall {
        system_ref: upload.system.as_deref().and_then(parse_i64).unwrap_or(0),
        system_label: upload.system_label,
        talkgroup_ref,
        talkgroup_label: upload.talkgroup_label,
        talkgroup_group: upload.talkgroup_group,
        talkgroup_tag: upload.talkgroup_tag,
        frequency: upload.frequency.as_deref().and_then(parse_i64),
        source: upload.source.as_deref().and_then(parse_i64),
        date_time: upload.date_time,
        timestamp: upload.timestamp.as_deref().and_then(parse_i64),
        audio_name: upload.audio_name,
        audio_mime: upload.audio_mime,
        audio,
    };

    // Serialized pipeline (ADR-0001): write the audio object first, then insert
    // the metadata row (which references it), then emit to the live feed.
    let object_key = format!(
        "{}.{}",
        uuid::Uuid::new_v4().simple(),
        ingested.audio_extension()
    );
    if let Err(err) = state.audio.put(&object_key, &ingested.audio) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not store audio: {err}\n"),
        )
            .into_response();
    }

    let stored = state.calls.insert(&ingested, object_key);
    state.live.publish(Arc::new(stored));

    (StatusCode::OK, CALL_IMPORTED).into_response()
}

/// The rdio-scanner incomplete-data response: HTTP 417 + `Incomplete call data: <reason>\n`.
fn incomplete(reason: &str) -> Response {
    (
        StatusCode::EXPECTATION_FAILED,
        format!("Incomplete call data: {reason}\n"),
    )
        .into_response()
}

/// Parse a decimal integer field, tolerating surrounding whitespace.
fn parse_i64(value: &str) -> Option<i64> {
    value.trim().parse().ok()
}
