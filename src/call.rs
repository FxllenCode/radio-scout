//! The Call domain type for the walking skeleton.
//!
//! `CONTEXT.md` is the ubiquitous language: a **Call** is a single recorded
//! transmission (audio + metadata). **Ref** is the external, recorder-supplied
//! numeric id (`systemRef`, `talkgroupRef`, …); **Id** is Radio-Scout's internal
//! primary key, never sent by recorders. They are kept distinct here.
//!
//! This is intentionally thin — ticket #3 replaces it with the full SeaORM
//! domain model (Systems/Talkgroups/Units + child tables).

use serde::Serialize;

/// Radio-Scout's internal primary key for a stored Call.
pub type CallId = u64;

/// A Call parsed from an ingest request, before it is assigned an Id or stored.
#[derive(Debug, Clone)]
pub struct IngestedCall {
    pub system_ref: i64,
    pub system_label: Option<String>,
    pub talkgroup_ref: i64,
    pub talkgroup_label: Option<String>,
    pub talkgroup_group: Option<String>,
    pub talkgroup_tag: Option<String>,
    pub frequency: Option<i64>,
    pub source: Option<i64>,
    pub date_time: Option<String>,
    pub timestamp: Option<i64>,
    pub audio_name: Option<String>,
    pub audio_mime: Option<String>,
    pub audio: Vec<u8>,
}

impl IngestedCall {
    /// The object-store key extension, derived from the uploaded filename
    /// (falling back to `wav`). Used to name the stored audio object.
    pub fn audio_extension(&self) -> String {
        self.audio_name
            .as_deref()
            .and_then(|name| name.rsplit_once('.'))
            .map(|(_, ext)| ext.to_ascii_lowercase())
            .filter(|ext| !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or_else(|| "wav".to_string())
    }
}

/// A Call that has been assigned an internal Id and had its audio stored.
///
/// Serializes with the compact camelCase keys the live-feed protocol uses
/// (ADR-0004). `object_key` is internal and never sent to clients.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredCall {
    pub id: CallId,
    pub system_ref: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_label: Option<String>,
    pub talkgroup_ref: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub talkgroup_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub talkgroup_group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub talkgroup_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_mime: Option<String>,
    /// Where the audio lives in the object store. Internal; not serialized.
    #[serde(skip)]
    pub object_key: String,
    /// The URL a client fetches the audio from (audio never rides the socket).
    pub audio_url: String,
}

impl StoredCall {
    /// Build a stored Call from an ingested one, its assigned Id, and the
    /// object-store key its audio was written to.
    pub fn from_ingested(id: CallId, ingested: &IngestedCall, object_key: String) -> Self {
        StoredCall {
            id,
            system_ref: ingested.system_ref,
            system_label: ingested.system_label.clone(),
            talkgroup_ref: ingested.talkgroup_ref,
            talkgroup_label: ingested.talkgroup_label.clone(),
            talkgroup_group: ingested.talkgroup_group.clone(),
            talkgroup_tag: ingested.talkgroup_tag.clone(),
            frequency: ingested.frequency,
            source: ingested.source,
            date_time: ingested.date_time.clone(),
            timestamp: ingested.timestamp,
            audio_mime: ingested.audio_mime.clone(),
            object_key,
            audio_url: format!("/api/call/{id}/audio"),
        }
    }
}
