//! A **Talkgroup** — a logical channel within a system that calls are addressed
//! to. Listeners subscribe at talkgroup granularity (CONTEXT.md). `ref` is the
//! recorder-supplied id, unique within its system.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "talkgroups")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub system_id: i64,
    pub r#ref: i64,
    pub label: Option<String>,
    pub name: Option<String>,
    pub tag_id: Option<i64>,
    /// LED color name (see the client LED palette); assigned via curation (#18).
    pub led: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::system::Entity",
        from = "Column::SystemId",
        to = "super::system::Column::Id"
    )]
    System,
    #[sea_orm(
        belongs_to = "super::tag::Entity",
        from = "Column::TagId",
        to = "super::tag::Column::Id"
    )]
    Tag,
    #[sea_orm(has_many = "super::call::Entity")]
    Call,
    #[sea_orm(has_many = "super::talkgroup_group::Entity")]
    TalkgroupGroup,
}

impl Related<super::system::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::System.def()
    }
}

impl Related<super::tag::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Tag.def()
    }
}

impl Related<super::call::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Call.def()
    }
}

impl Related<super::talkgroup_group::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::TalkgroupGroup.def()
    }
}

// Many-to-many to Group through the join table.
impl Related<super::group::Entity> for Entity {
    fn to() -> RelationDef {
        super::talkgroup_group::Relation::Group.def()
    }
    fn via() -> Option<RelationDef> {
        Some(super::talkgroup_group::Relation::Talkgroup.def().rev())
    }
}

impl ActiveModelBehavior for ActiveModel {}
