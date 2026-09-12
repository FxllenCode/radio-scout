//! The selection catalog (#12, spec US 19–21) — every System and Talkgroup the
//! Talkgroups panel can offer, so a listener can choose what the live feed
//! plays.
//!
//! # Improving on rdio-scanner
//!
//! rdio pushes its whole config down the live-feed WebSocket on connect, so its
//! select panel exists only while a socket is up (`config` event →
//! `select.component`). Ours is a plain `GET /api/catalog`: the panel renders
//! with the feed off (playback mode), survives a dropped socket, is cacheable
//! and refetchable by the client's normal data layer, and is reachable by
//! anything that speaks HTTP.
//!
//! It is also deliberately **not** derived from Calls the way
//! `GET /api/calls/filters` is (#13): a Talkgroup whose Calls have aged out
//! (#10) is still something a listener wants selected, because the selection is
//! about what *will* arrive.
//!
//! The wire contract is [`Catalog`], and [`read`] is the query behind it. The
//! read lives here rather than in [`crate::db::repo`] because these types do
//! (#98): a data-layer module that has to import a handler module's view types
//! in order to build them has the dependency backwards, and this was the last
//! place it did.

use axum::extract::State;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, FromQueryResult, QueryFilter, QuerySelect,
};
use serde::Serialize;

use crate::AppState;
use crate::db::entities::{call, group, system, tag, talkgroup, talkgroup_group};
use crate::failure::{Failure, Stage};

/// How far back "recently" reaches, for the activity a panel row shows and the
/// most-active sort orders by (#57, spec US 29).
///
/// **Bounded, and that is the design.** Last-heard over the whole Archive would
/// be `MAX(call_at_ms) GROUP BY talkgroup_id` — a covering-index scan of every
/// Call ever stored, on every app open, on a Pi with an SD card. A day is what
/// a Listener means by "is this channel busy"; a Talkgroup quieter than that
/// carries no activity at all, and its row is drawn quiet.
///
/// It rides on the wire ([`Catalog::activity_window_ms`]) rather than being
/// spelled a second time in the client, so "12 calls in the last 24 hours" is
/// true by construction if this number ever moves.
pub const ACTIVITY_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;

/// One Talkgroup a listener can select. Nested under its System, so the Ref
/// pair the selection is keyed by is the row's position in the document rather
/// than a field repeated on every entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogTalkgroup {
    /// The recorder-supplied external id (CONTEXT.md: **Ref**), unique within
    /// its System.
    pub r#ref: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The Talkgroup's single Tag (CONTEXT.md), one half of the panel's
    /// category toggles.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Every Group the Talkgroup belongs to, sorted — the other half. A
    /// Talkgroup may belong to several, so this is a list, not a field.
    pub groups: Vec<String>,
    /// The curated LED color (#18), when an operator has set one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub led: Option<String>,
    /// How many Calls this Talkgroup took in the last [`ACTIVITY_WINDOW_MS`] —
    /// what the panel's most-active sort orders by. Absent rather than `0` for
    /// a Talkgroup that took none, so a quiet row is drawn quiet and the
    /// document a fresh Instance serves is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent_calls: Option<i64>,
    /// The newest Call inside that window — what a last-heard age is a
    /// subtraction from. Absent for the same reason, and never older than the
    /// window: it is the window's answer, not the Archive's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_call_at_ms: Option<i64>,
    /// This channel is **restricted** and this Listener cannot hear it (#68,
    /// spec US 52) — so the panel draws the row with a lock rather than a
    /// switch, and offers the unlock.
    ///
    /// **The row is still here, and its activity is not.** Two decisions, and
    /// they pull in opposite directions on purpose: the Operator gated the
    /// channel, not the fact that it exists, and a Listener holding a code has
    /// to be able to see what the code is *for* — but "PD Tac took 94 Calls in
    /// the last hour" is traffic analysis of exactly the channel that was
    /// gated, so [`CatalogTalkgroup::recent_calls`] and `last_call_at_ms` are
    /// absent on a locked row however busy it was.
    ///
    /// Omitted when it isn't, the [`crate::call::StoredCall::emergency`] rule:
    /// an Instance that gates nothing serves the document it served before this
    /// field existed, byte for byte.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
}

/// One System and the Talkgroups under it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSystem {
    pub r#ref: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub talkgroups: Vec<CatalogTalkgroup>,
}

/// Everything the Talkgroups panel offers.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub systems: Vec<CatalogSystem>,
    /// How long "recently" is, for every [`CatalogTalkgroup::recent_calls`]
    /// above — see [`ACTIVITY_WINDOW_MS`].
    pub activity_window_ms: i64,
    /// Whether this Instance mints **Share links** (#64, spec US 32) — `[share]
    /// enabled`.
    ///
    /// Here rather than on a surface of its own because it is the same question
    /// the window above answers: *what does this Instance offer?*, asked once on
    /// app open. A control that is offered and then refused is a control that
    /// lies, and an Operator who closed sharing closed it for a reason.
    ///
    /// Set by the **handler**, not by [`read`]: this is configuration rather
    /// than something in the database, and `read` is what a test drives against
    /// rows.
    pub sharing: bool,
    /// Whether a range of the Archive can be taken away, and how much of one at
    /// a time (#65, spec US 33).
    ///
    /// On the wire for `sharing`'s reason: a control offered and then refused
    /// is a control that lies — and here the *number* matters too, because a
    /// Listener has to be told "that is 4,312 Calls" **before** they wait for a
    /// download that was never going to arrive.
    pub export: ExportOffer,
    /// What a **Star** is worth on this Instance (#66, spec US 37).
    ///
    /// On the wire for `sharing`'s reason, one step on: a control that is
    /// offered and then *refused* is a control that lies, and one that quietly
    /// means less than a Listener thinks it does is the same lie told more
    /// slowly. Only the server knows `[retention] starred_days`.
    pub starred: StarOffer,
    /// Where this browser stands with **Access codes** (#68, spec US 52).
    ///
    /// `sharing`'s reason a third time, plus one this feature owes on its own:
    /// a browser remembers its grant, and a grant can stop working while the
    /// browser is not looking — the Operator revoked the code, or it reached
    /// its expiry overnight. Every HTTP read **degrades** rather than refusing
    /// in that case, so without this the app would quietly go back to open
    /// listening and never say why. This is where it is said, and it is what
    /// tells the client to clear what it is holding.
    ///
    /// **Absent entirely on an Instance that gates nothing**, which is every
    /// Instance until an Operator marks a channel — so the document a fresh
    /// install serves is byte-for-byte the one it served before this feature
    /// existed, and `tests/catalog.rs` can assert that rather than describe it.
    /// It is the [`crate::call::StoredCall::emergency`] rule applied to a whole
    /// object instead of a field.
    #[serde(skip_serializing_if = "AccessOffer::is_quiet")]
    pub access: AccessOffer,
}

/// Where the browser reading this catalog stands with **Access codes** (#68).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessOffer {
    /// Whether this Instance restricts any channel at all. `false` — which is
    /// what ships and what every Instance is until an Operator marks something
    /// — means there is nothing to unlock and no control to draw.
    pub gating: bool,
    /// The **label** of the code this request arrived with, if it arrived with a
    /// live one. Never the code and never the grant (ADR-0011 rule 2): a label
    /// is the most that may be said out loud about one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// When that code runs out, if it does — so a browser can say "your access
    /// ends at 18:00" rather than discovering it mid-transmission.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    /// Why the grant this request presented is not being honoured, when it
    /// presented one that is not. Absent for a browser holding nothing and for
    /// one holding a live code — the two ordinary states.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale: Option<crate::access::Stale>,
}

impl AccessOffer {
    /// Whether there is nothing here worth a key on the wire: an Instance that
    /// gates nothing, read by a browser holding nothing.
    ///
    /// Every field, not just `gating`, because the three that follow it are the
    /// answer to *"what happened to the grant I sent?"* — and a browser that
    /// sent one is owed that answer whatever the rest of the Instance looks like.
    fn is_quiet(&self) -> bool {
        !self.gating && self.label.is_none() && self.expires_at_ms.is_none() && self.stale.is_none()
    }
}

impl From<&crate::access::Viewer> for AccessOffer {
    fn from(viewer: &crate::access::Viewer) -> Self {
        match &viewer.held {
            crate::access::Held::Nothing => AccessOffer::default(),
            crate::access::Held::Code(code) => AccessOffer {
                gating: false,
                label: code.label.clone(),
                expires_at_ms: code.expires_at_ms,
                stale: None,
            },
            crate::access::Held::Stale(why) => AccessOffer {
                gating: false,
                label: None,
                expires_at_ms: None,
                stale: Some(*why),
            },
        }
    }
}

/// What starring a Call does here, beyond marking it (#66).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StarOffer {
    /// Whether a Star holds a Call back from **Retention** at all. `false` is
    /// what ships: the exemption is a disk commitment, and only an Operator can
    /// make one.
    pub kept: bool,
    /// For how many days past the transmission — `0` being "for good", the
    /// reading every window in `[retention]` already has. Meaningless when
    /// `kept` is false, which is the [`ExportOffer`] shape beside it.
    pub kept_days: u32,
}

/// The one place `[retention] starred_days`' three readings become the two
/// fields on the wire.
///
/// Here rather than at the handler, which used to ask [`crate::star::Stars`]
/// twice and derive `kept` from the answer's shape — a fact the option already
/// held, re-read on the far side of an accessor.
impl From<Option<u32>> for StarOffer {
    fn from(kept_days: Option<u32>) -> Self {
        StarOffer {
            kept: kept_days.is_some(),
            kept_days: kept_days.unwrap_or_default(),
        }
    }
}

/// What this Instance will let a Listener take away (#65).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOffer {
    pub enabled: bool,
    /// The most Calls one export may carry — `[export] max_calls`.
    pub max_calls: u64,
}

// How a catalog reaches a listener, decided beside the type rather than at the
// handler (#92).
crate::answers_json!(Catalog);

/// `GET /api/catalog` — the Systems and Talkgroups a listener can select.
pub async fn catalog(
    State(state): State<AppState>,
    viewer: crate::access::Viewer,
) -> Result<Catalog, Failure> {
    let catalog = read(
        &state.db,
        state.clock.now_ms() - ACTIVITY_WINDOW_MS,
        &viewer.scope,
    )
    .await
    .map_err(Stage::LoadCatalog.failed())?;
    Ok(Catalog {
        sharing: state.shares.enabled(),
        export: ExportOffer {
            enabled: state.exports.enabled(),
            max_calls: state.exports.max_calls(),
        },
        starred: state.stars.kept_days().into(),
        access: AccessOffer {
            // The bit is the Instance's and the rest is this request's, which is
            // why they are assembled from two places and not one.
            gating: state.access.is_gating(),
            ..AccessOffer::from(&viewer)
        },
        ..catalog
    })
}

/// One Talkgroup's traffic since a cutoff, as the grouped query answers it.
#[derive(Debug, FromQueryResult)]
struct Activity {
    talkgroup_id: i64,
    recent_calls: i64,
    last_call_at_ms: i64,
}

/// Every System and Talkgroup a listener can select (#12, spec US 19–21).
///
/// Three flat queries — systems, talkgroups (with their Tag), and the
/// Talkgroup→Group links — assembled in Rust. Constant in the number of
/// round trips rather than one per Talkgroup, which is what makes this
/// affordable on a Pi with a few hundred Talkgroups, and it keeps the
/// dialect-divergent list aggregation out of SQL (ADR-0003), like the archive
/// search.
pub async fn read<C: ConnectionTrait>(
    db: &C,
    since_ms: i64,
    scope: &crate::access::AccessScope,
) -> Result<Catalog, DbErr> {
    // One grouped query for the whole panel, not one per row (#86): a county
    // catalog is 400+ Talkgroups, and an N+1 here would be invisible from
    // outside because every answer it gave would be correct.
    let mut activity: std::collections::HashMap<i64, Activity> = call::Entity::find()
        .select_only()
        .column(call::Column::TalkgroupId)
        .column_as(call::Column::Id.count(), "recent_calls")
        .column_as(call::Column::CallAtMs.max(), "last_call_at_ms")
        .filter(call::Column::CallAtMs.gte(since_ms))
        .group_by(call::Column::TalkgroupId)
        .into_model::<Activity>()
        .all(db)
        .await?
        .into_iter()
        .map(|row| (row.talkgroup_id, row))
        .collect();

    let systems: Vec<system::Model> = system::Entity::find().all(db).await?;

    let mut groups_by_talkgroup: std::collections::HashMap<i64, Vec<String>> =
        std::collections::HashMap::new();
    for (link, group) in talkgroup_group::Entity::find()
        .find_also_related(group::Entity)
        .all(db)
        .await?
    {
        if let Some(group) = group {
            groups_by_talkgroup
                .entry(link.talkgroup_id)
                .or_default()
                .push(group.name);
        }
    }

    // Read **before** the Talkgroups, because whether a channel is gated is its
    // System's answer whenever its own column is `NULL` (#68) — and read once,
    // because this is also what the listing below is built from.
    let systems_by_id: std::collections::HashMap<i64, (i64, bool)> = systems
        .iter()
        .map(|system| (system.id, (system.r#ref, system.restricted)))
        .collect();

    let mut talkgroups_by_system: std::collections::HashMap<i64, Vec<CatalogTalkgroup>> =
        std::collections::HashMap::new();
    for (talkgroup, tag) in talkgroup::Entity::find()
        .find_also_related(tag::Entity)
        .all(db)
        .await?
    {
        let mut groups = groups_by_talkgroup
            .remove(&talkgroup.id)
            .unwrap_or_default();
        groups.sort();
        let heard = activity.remove(&talkgroup.id);
        // `NULL` on the channel inherits its System, which is the reading the
        // Archive's SQL and the live feed's wire both take (#68).
        let (system_ref, system_restricted) = systems_by_id
            .get(&talkgroup.system_id)
            .copied()
            .unwrap_or_default();
        let restricted = talkgroup.restricted.unwrap_or(system_restricted);
        let locked = !scope.permits(system_ref, talkgroup.r#ref, restricted);
        talkgroups_by_system
            .entry(talkgroup.system_id)
            .or_default()
            .push(CatalogTalkgroup {
                r#ref: talkgroup.r#ref,
                label: talkgroup.label,
                name: talkgroup.name,
                tag: tag.map(|tag| tag.name),
                groups,
                led: talkgroup.led,
                // Stripped on a locked row rather than merely not drawn: how busy
                // a gated channel has been is traffic analysis of exactly what
                // was gated, and the client is not where that is decided.
                recent_calls: heard
                    .as_ref()
                    .filter(|_| !locked)
                    .map(|heard| heard.recent_calls),
                last_call_at_ms: heard.filter(|_| !locked).map(|heard| heard.last_call_at_ms),
                locked,
            });
    }

    let mut systems: Vec<CatalogSystem> = systems
        .into_iter()
        .map(|system| {
            let mut talkgroups = talkgroups_by_system.remove(&system.id).unwrap_or_default();
            talkgroups.sort_by_key(|tg| display_order(tg.label.as_deref(), tg.r#ref));
            CatalogSystem {
                r#ref: system.r#ref,
                label: system.label,
                talkgroups,
            }
        })
        .collect();
    systems.sort_by_key(|system| display_order(system.label.as_deref(), system.r#ref));

    Ok(Catalog {
        systems,
        activity_window_ms: ACTIVITY_WINDOW_MS,
        // Configuration, which this function does not read — see the field.
        ..Catalog::default()
    })
}

/// How the panel is ordered: by what the listener reads, then by Ref.
///
/// The sort happens in Rust rather than in `ORDER BY` on purpose — text
/// collation is exactly where SQLite and Postgres disagree (ADR-0003), and a
/// panel whose rows reshuffle when an operator moves to Postgres is a bug the
/// dual-dialect suite would have to catch instead of the design preventing it.
/// An unlabeled row sorts last: it is the one a listener can't recognise, so it
/// belongs at the bottom rather than silently taking the top slot.
pub(crate) fn display_order(label: Option<&str>, r#ref: i64) -> (bool, String, i64) {
    (
        label.is_none(),
        label.unwrap_or_default().to_lowercase(),
        r#ref,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_sort_case_insensitively_before_unlabeled_rows() {
        let mut rows = vec![
            display_order(None, 1),
            display_order(Some("zulu"), 2),
            display_order(Some("Alpha"), 3),
            display_order(Some("alpha"), 1),
        ];
        rows.sort();

        assert_eq!(
            rows,
            vec![
                display_order(Some("alpha"), 1),
                display_order(Some("Alpha"), 3),
                display_order(Some("zulu"), 2),
                display_order(None, 1),
            ],
            "case-insensitive by label, Ref breaking ties, unlabeled last"
        );
    }
}
