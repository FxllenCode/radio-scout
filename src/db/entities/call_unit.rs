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
    pub label: Option<String>,
    pub offset_ms: Option<i64>,
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
