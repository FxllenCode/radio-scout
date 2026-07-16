//! The Call view type shared by the live feed and the audio-serve path.
//!
//! `CONTEXT.md` is the ubiquitous language: a **Call** is a single recorded
//! transmission (audio + metadata). **Ref** is the external, recorder-supplied
//! numeric id (`systemRef`, `talkgroupRef`, …); **Id** is Radio-Scout's internal
//! primary key, never sent by recorders. `StoredCall` is the denormalized view
//! built from the SeaORM rows (`crate::db::repo::stored_call`).

use serde::Serialize;

/// Radio-Scout's internal primary key for a stored Call (matches the DB `i64`).
pub type CallId = i64;

/// A stored Call as delivered over the live feed and referenced by the audio
/// endpoint. Serializes with the compact camelCase keys the live-feed protocol
/// uses (ADR-0004). `object_key` is internal and never sent to clients.
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
