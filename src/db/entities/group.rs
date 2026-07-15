//! A **Group** — a cross-system category clustering talkgroups by purpose
//! (e.g. "Fire", "Law"). A talkgroup may belong to several groups (CONTEXT.md),
//! so Group <-> Talkgroup is many-to-many via `talkgroup_groups`.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "groups")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(unique)]
    pub name: String,
    pub created_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::talkgroup_group::Entity")]
    TalkgroupGroup,
}

impl Related<super::talkgroup_group::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::TalkgroupGroup.def()
    }
}

// Many-to-many bridge to Talkgroup through the join table.
impl Related<super::talkgroup::Entity> for Entity {
    fn to() -> RelationDef {
        super::talkgroup_group::Relation::Talkgroup.def()
    }
    fn via() -> Option<RelationDef> {
        Some(super::talkgroup_group::Relation::Group.def().rev())
    }
}

impl ActiveModelBehavior for ActiveModel {}
