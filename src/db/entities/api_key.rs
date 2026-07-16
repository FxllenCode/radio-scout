//! An **API key** — a recorder-facing secret that authorizes ingesting calls
//! (CONTEXT.md, ADR-0008). Stored **hashed** (SHA-256 hex), never in plaintext.
//! `system_ref` scopes the key to one System's Ref; `NULL` grants all Systems.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "api_keys")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(unique)]
    pub key_hash: String,
    pub label: Option<String>,
    /// The System Ref this key may ingest to; `None` = every System.
    pub system_ref: Option<i64>,
    pub disabled: bool,
    pub created_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
