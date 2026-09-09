//! Postgres LISTEN/NOTIFY — the transport under subscriptions and hot reload.
//!
//! # Design
//!
//! ```text
//!   node commits a block
//!         │
//!         ▼
//!   NOTIFY <channel>, '{"entity":"Transfer","operation":"SET","id":"0x…"}'
//!         │                    (only after COMMIT — an aborted
//!         ▼                     transaction emits nothing)
//!   PgListener (this module, one dedicated connection)
//!         │
//!         ▼
//!   broadcast::Sender  ──▶ per-subscriber GraphQL streams
//! ```
//!
//! A dedicated connection is required: `LISTEN` is session-scoped, so it cannot
//! share the pooled connections that serve queries.
//!
//! The payload is a *hint*, not data. Subscribers re-read the row through the
//! normal query path, so subscription results obey the same limits and shape as
//! everything else, and the notification stays under Postgres' 8000-byte payload
//! cap regardless of entity size.
//!
//! **Status: not wired up.** The node does not emit these notifications yet
//! (Milestone 10). This module fixes the payload contract both sides will use.

use serde::{Deserialize, Serialize};

/// Channel a project's entity changes are published on.
///
/// Namespaced by db schema so several projects can share one database without
/// cross-talk.
pub fn entity_channel(db_schema: &str) -> String {
    format!("superquery_{db_schema}_entities")
}

/// Channel announcing a schema migration, for hot reload (Milestone 11).
pub fn schema_channel(db_schema: &str) -> String {
    format!("superquery_{db_schema}_schema")
}

/// What the node did to an entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Operation {
    /// Created or updated.
    Set,
    /// Deleted.
    Remove,
}

/// The notification payload. Kept small and stable — this is a cross-repo
/// contract, so adding a field must stay backward compatible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityNotification {
    /// Entity name as written in the schema, e.g. `Transfer`.
    pub entity: String,
    pub operation: Operation,
    /// Primary key of the affected row.
    pub id: String,
    /// Height the change was committed at, for ordering and replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_height: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_are_namespaced_per_project() {
        assert_eq!(entity_channel("app"), "superquery_app_entities");
        assert_ne!(entity_channel("app"), entity_channel("other"));
    }

    #[test]
    fn payload_round_trips() {
        let n = EntityNotification {
            entity: "Transfer".into(),
            operation: Operation::Set,
            id: "0xabc".into(),
            block_height: Some(21012345),
        };
        let json = serde_json::to_string(&n).unwrap();
        assert_eq!(
            serde_json::from_str::<EntityNotification>(&json).unwrap(),
            n
        );
    }

    #[test]
    fn block_height_is_optional_for_forward_compat() {
        // An older node omitting the field must still parse.
        let n: EntityNotification =
            serde_json::from_str(r#"{"entity":"Transfer","operation":"REMOVE","id":"1"}"#).unwrap();
        assert_eq!(n.operation, Operation::Remove);
        assert_eq!(n.block_height, None);
    }
}
