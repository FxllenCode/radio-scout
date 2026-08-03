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
use sea_orm::{ConnectionTrait, DbErr, EntityTrait};
use serde::Serialize;

use crate::AppState;
use crate::db::entities::{group, system, tag, talkgroup, talkgroup_group};
use crate::failure::{Failure, Stage};

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
}

// How a catalog reaches a listener, decided beside the type rather than at the
// handler (#92).
crate::answers_json!(Catalog);

/// `GET /api/catalog` — the Systems and Talkgroups a listener can select.
pub async fn catalog(State(state): State<AppState>) -> Result<Catalog, Failure> {
    read(&state.db).await.map_err(Stage::LoadCatalog.failed())
}

/// Every System and Talkgroup a listener can select (#12, spec US 19–21).
///
/// Three flat queries — systems, talkgroups (with their Tag), and the
/// Talkgroup→Group links — assembled in Rust. Constant in the number of
/// round trips rather than one per Talkgroup, which is what makes this
/// affordable on a Pi with a few hundred Talkgroups, and it keeps the
/// dialect-divergent list aggregation out of SQL (ADR-0003), like the archive
/// search.
pub async fn read<C: ConnectionTrait>(db: &C) -> Result<Catalog, DbErr> {
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
            });
    }

    let mut systems: Vec<CatalogSystem> = system::Entity::find()
        .all(db)
        .await?
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

    Ok(Catalog { systems })
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
