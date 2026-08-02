//! The Call view types shared by the live feed, the audio-serve path, and the
//! archive.
//!
//! `CONTEXT.md` is the ubiquitous language: a **Call** is a single recorded
//! transmission (audio + metadata). **Ref** is the external, recorder-supplied
//! numeric id (`systemRef`, `talkgroupRef`, …); **Id** is Radio-Scout's internal
//! primary key, never sent by recorders. `StoredCall` is the denormalized view
//! built from the SeaORM rows (`crate::db::repo::stored_calls`, which builds a
//! whole page of them in a fixed number of queries — there is no per-Call form).
//!
//! These live here rather than beside their handlers so the data layer can build
//! them without depending on the HTTP layer.

use serde::Serialize;

/// Radio-Scout's internal primary key for a stored Call (matches the DB `i64`).
pub type CallId = i64;

/// A Call's place in the **emission** sequence — the order Calls went out on the
/// live feed (#94).
///
/// Deliberately its own name rather than a second `CallId`, because they are two
/// different orderings of the same Calls and the whole point is that they
/// diverge: a **Delay** (#73) stores a Call early and emits it late. A
/// **Backfill** is read in this order and a Listener's cursor is a value of this
/// type; `0` is the cursor of a Listener who has heard nothing.
pub type Emission = i64;

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
    /// The Talkgroup's curated LED color, set by CSV import (#18) and drawn
    /// from the client palette. Absent until an operator curates it, and the
    /// client then falls back to its deterministic per-Talkgroup color — so an
    /// uncurated archive still reads at a glance, and a curated one reads the
    /// way its operator meant it to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub led: Option<String>,
    /// Talkgroup Refs this Call is patched to (rdio `patches[]`). Carried on the
    /// wire so the client can display cross-patched traffic; also drives live-feed
    /// patch fanout (a subscriber of any patched Talkgroup receives the Call).
    /// Omitted from the payload when empty to keep the socket compact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patches: Vec<i64>,
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
    /// How long the transmission is, in milliseconds (#42, spec US 8) — from
    /// the recorder's own metadata when it sent any, else read from the audio's
    /// container header at ingest. Absent when neither could say, which is what
    /// every Call stored before this existed looks like.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// The emergency bit the radio set (#42, spec US 5) — omitted when it
    /// wasn't, because it almost never is and every live frame pays for a key
    /// that is present.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub emergency: bool,
    /// The talkgroup was encrypted, so this Call is metadata and nothing else
    /// (spec US 9). Its `audio_url` is absent, which is the same fact said in
    /// the way a player can act on.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub encrypted: bool,
    /// The Site Ref this Call was heard on, for multi-site Systems (spec
    /// US 11). Absent unless a recorder named one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site_ref: Option<i64>,
    /// Where the audio lives in the object store. Internal; not serialized.
    #[serde(skip)]
    pub object_key: String,
    /// The URL a client fetches the audio from (audio never rides the socket).
    ///
    /// Absent when there is nothing to fetch — an encrypted Call. A client
    /// therefore cannot queue an unplayable Call by forgetting to check a flag:
    /// the thing it would need in order to play it is not there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_url: Option<String>,
}

impl StoredCall {
    /// Every Talkgroup this Call reaches: its own, then the ones it is patched
    /// to. What "does this listener hear it?" is asked over, on the live feed
    /// (#9) and in Web Push (#16) alike.
    pub fn talkgroups(&self) -> impl Iterator<Item = i64> + '_ {
        std::iter::once(self.talkgroup_ref).chain(self.patches.iter().copied())
    }
}

/// One Call, with everything the recorder said about it — the contract of
/// `GET /api/call/{id}` (#42, spec US 5).
///
/// [`StoredCall`] plus the parts nobody wants fifty of: the per-frequency and
/// per-source detail, and the scalars an archive reader asks for one Call at a
/// time. Kept off `StoredCall` deliberately — that type is one live-feed frame
/// per Call *and* fifty rows per search page, and Trunk Recorder sends a
/// `freqList` and a `srcList` on every single upload, so carrying them there
/// would put arrays on every frame a Pi pushes to every listener for data a
/// screen shows only when somebody opens one Call.
///
/// rdio-scanner has no equivalent at all: it answers a search with bare ids and
/// makes the client re-fetch each Call, and even then never surfaces the signal
/// detail its own parser collected.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallDetail {
    /// Everything a search row already carries, flattened so one Call has one
    /// shape whichever endpoint served it.
    #[serde(flatten)]
    pub call: StoredCall,
    /// When the transmission ended, unix milliseconds (the recorder's own
    /// `stop_time`). Absent unless a recorder said.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_ms: Option<i64>,
    /// The **recorder's** priority for this call, not the listener's per-
    /// Talkgroup Priority (#58).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    /// What the recorder was demodulating — `digital`, `digital tdma`,
    /// `analog`. The recorder's vocabulary, passed through.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_type: Option<String>,
    /// How the signal behaved, per frequency the call moved across. Always
    /// present, possibly empty — a caller that asked for the detail is handling
    /// a list either way, and an absent key would make it handle two things.
    pub frequencies: Vec<CallFrequencyDetail>,
    /// Which radios were heard, and when.
    pub units: Vec<CallUnitDetail>,
}

// How one Call, and the cascading filter options beside it, reach a client —
// decided beside the types rather than at the handlers (#92).
crate::answers_json!(CallDetail, FilterOptions);

/// One frequency segment of a Call: where it sat and how badly it decoded.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallFrequencyDetail {
    pub freq: i64,
    /// Offset into the Call, milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pos_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub len_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dbm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spike_count: Option<i32>,
    /// Wall-clock start, unix milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<i64>,
}

/// One radio heard within a Call.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallUnitDetail {
    /// The radio's external id (CONTEXT.md: **Ref**).
    pub r#ref: i64,
    /// The alias the recorder had configured for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The alias the radio itself put over the air (#42, spec US 12). Separate
    /// from `label` because when the two disagree, that *is* the information.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_ota: Option<String>,
    /// Offset into the Call, milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset_ms: Option<i64>,
    /// This Unit held the emergency bit — which radio pressed the button,
    /// where the Call's own flag can only say that somebody did.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub emergency: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal_system: Option<String>,
    /// Wall-clock start, unix milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<i64>,
}

/// One System that has Calls matching the current archive-search filters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemOption {
    /// The recorder-supplied external id (CONTEXT.md: **Ref**).
    pub r#ref: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// One Talkgroup that has Calls matching the current archive-search filters. A
/// Talkgroup Ref is unique only within its System, so both are carried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TalkgroupOption {
    pub system_ref: i64,
    pub r#ref: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

/// The values each archive-search filter can usefully take, given the *other*
/// filters already chosen — the cascading-filter contract of
/// `GET /api/calls/filters` (#13, spec US 24).
///
/// Two properties make this better than rdio-scanner's equivalent, which builds
/// its dropdowns from the whole talkgroup config:
/// 1. **Only values with Calls are offered** — no dead options that search to
///    nothing.
/// 2. **Each dimension excludes its own filter** — picking System 100 narrows
///    the Talkgroup list but leaves the System list complete, so a choice is
///    always reversible in one click.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterOptions {
    pub systems: Vec<SystemOption>,
    pub talkgroups: Vec<TalkgroupOption>,
    pub groups: Vec<String>,
    pub tags: Vec<String>,
    /// Oldest / newest Call time (unix ms) the non-date filters can reach —
    /// the bounds a date picker should offer. Computed with the date filter
    /// itself excluded, so narrowing the range never collapses the picker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_start_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_stop_ms: Option<i64>,
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
            led: Some("red".into()),
            patches: vec![54001, 54002],
            frequency: Some(774_031_250),
            source: Some(1_610_092),
            date_time: Some("2022-11-29T18:05:38Z".into()),
            timestamp: Some(1_669_740_338_000),
            audio_mime: Some("audio/mp4".into()),
            duration_ms: Some(8250),
            emergency: true,
            encrypted: false,
            site_ref: Some(3),
            object_key: "ab/secret-internal-key.m4a".into(),
            audio_url: Some("/api/call/42/audio".into()),
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

    /// Patches serialize as a compact array when present, and are dropped
    /// entirely when empty (the common, un-patched case).
    #[test]
    fn patches_serialize_when_present_and_omit_when_empty() {
        let with = serde_json::to_value(full()).unwrap();
        assert_eq!(with["patches"], serde_json::json!([54001, 54002]));

        let mut without = full();
        without.patches.clear();
        let json = serde_json::to_value(without).unwrap();
        assert!(json.get("patches").is_none(), "empty patches omitted");
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
            led: None,
            patches: vec![],
            frequency: None,
            source: None,
            date_time: None,
            timestamp: None,
            audio_mime: None,
            duration_ms: None,
            emergency: false,
            encrypted: false,
            site_ref: None,
            object_key: "internal".into(),
            audio_url: Some("/api/call/1/audio".into()),
        };
        insta::assert_json_snapshot!("stored_call_minimal", minimal);
    }
}
