//! SeaORM entities for the Radio-Scout domain model (CONTEXT.md, ADR-0003).
//!
//! Every network-facing entity carries both an internal **Id** (primary key)
//! and an external **Ref** (the recorder-supplied id) — conflating them breaks
//! joins. Timestamps are stored as unix-milliseconds `i64` to stay
//! dialect-agnostic across SQLite and Postgres.

pub mod api_key;
pub mod call;
pub mod call_frequency;
pub mod call_patch;
pub mod call_unit;
pub mod group;
pub mod site;
pub mod system;
pub mod tag;
pub mod talkgroup;
pub mod talkgroup_group;
pub mod unit;
