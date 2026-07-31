//! Per-call unit detail — the rdio `sources[]`/`units[]` array (which unit was
//! transmitting at which offset).

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "call_units")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub call_id: i64,
    pub unit_ref: i64,
    /// The alias the recorder resolved from its own configuration.
    pub label: Option<String>,
    pub offset_ms: Option<i64>,
    /// The alias the **radio itself** transmitted — TR's `tag_ota` (#42, spec
    /// US 12). Kept apart from `label` on purpose: one is what an operator
    /// configured and the other is what the air said, and when they disagree
    /// the disagreement is the interesting part.
    pub tag_ota: Option<String>,
    /// This Unit held the emergency bit — which radio pressed the button,
    /// where a Call-level flag can only say that somebody did.
    pub emergency: bool,
    /// The signalling the recorder decoded this Unit under, verbatim from TR
    /// (`P25`, `MDC1200`, …). Free text: it is the recorder's vocabulary.
    pub signal_system: Option<String>,
    /// Wall-clock time this Unit started transmitting, unix milliseconds —
    /// TR's `time`, where `offset_ms` is the same instant relative to the Call.
    pub at_ms: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::call::Entity",
        from = "Column::CallId",
        to = "super::call::Column::Id"
    )]
    Call,
}

impl Related<super::call::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Call.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
