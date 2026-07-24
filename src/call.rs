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

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> StoredCall {
        StoredCall {
            id: 42,
            system_ref: 11,
            system_label: Some("butco".into()),
            talkgroup_ref: 54241,
            talkgroup_label: Some("TDB A1".into()),
            talkgroup_group: Some("Fire".into()),
            talkgroup_tag: Some("Fire Dispatch".into()),
            frequency: Some(774_031_250),
            source: Some(1_610_092),
            date_time: Some("2022-11-29T18:05:38Z".into()),
            timestamp: Some(1_669_740_338_000),
            audio_mime: Some("audio/mp4".into()),
            object_key: "ab/secret-internal-key.m4a".into(),
            audio_url: "/api/call/42/audio".into(),
        }
    }

    #[test]
    fn object_key_is_never_serialized() {
        // `object_key` is an internal storage detail; leaking it to live-feed
        // clients would be a contract break (ADR-0004).
        let json = serde_json::to_value(full()).unwrap();
        assert!(json.get("objectKey").is_none());
        assert!(json.get("object_key").is_none());
    }

    /// Pin the exact live-feed wire shape: camelCase keys, `object_key` hidden,
    /// all fields present.
    #[test]
    fn full_call_wire_shape() {
        insta::assert_json_snapshot!("stored_call_full", full());
    }

    /// `None` fields are omitted entirely, keeping the socket payload compact.
    #[test]
    fn none_fields_are_omitted() {
        let minimal = StoredCall {
            id: 1,
            system_ref: 11,
            system_label: None,
            talkgroup_ref: 5,
            talkgroup_label: None,
            talkgroup_group: None,
            talkgroup_tag: None,
            frequency: None,
            source: None,
            date_time: None,
            timestamp: None,
            audio_mime: None,
            object_key: "internal".into(),
            audio_url: "/api/call/1/audio".into(),
        };
        insta::assert_json_snapshot!("stored_call_minimal", minimal);
    }
}
