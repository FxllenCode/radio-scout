//! Per-call patch detail — the rdio `patches[]` array. A patched talkgroup ref
//! that this call should also reach (full patch resolution is the live feed's
//! job, #9).

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "call_patches")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub call_id: i64,
    pub talkgroup_ref: i64,
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
