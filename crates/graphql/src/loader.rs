//! DataLoaders for relation fields (Milestone 8).
//!
//! # The N+1 this removes
//!
//! ```graphql
//! transfers(first: 100) { nodes { fromAccount { balance } } }
//! ```
//!
//! Resolved naively, that is one statement for the page and then one per
//! transfer for its account — 101 round-trips. Every relation resolver in a
//! layer runs concurrently, so a DataLoader can collect their keys for a moment
//! and issue a single `WHERE id = ANY($1)` instead: 2 statements, regardless of
//! page size.
//!
//! # No cache, on purpose
//!
//! The loaders live as long as the schema, not the request, so they are built
//! without a result cache. A cache there would serve a row the node has since
//! updated; batching alone gives the win that matters.

use std::collections::HashMap;
use std::sync::Arc;

use async_graphql::dataloader::Loader;
use superquery_postgres::row::EntityRow;
use superquery_postgres::{decode_row, sql, Database};
use superquery_query_core::SchemaIr;

/// One entity row, by primary key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntityKey {
    /// Entity name as written in the schema, e.g. `Account`.
    pub entity: String,
    pub id: String,
}

impl EntityKey {
    pub fn new(entity: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            entity: entity.into(),
            id: id.into(),
        }
    }
}

/// Loads rows by id for forward (foreign-key) relations.
pub struct EntityLoader {
    db: Database,
    ir: Arc<SchemaIr>,
    db_schema: String,
}

impl EntityLoader {
    pub fn new(db: Database, ir: Arc<SchemaIr>, db_schema: impl Into<String>) -> Self {
        Self {
            db,
            ir,
            db_schema: db_schema.into(),
        }
    }
}

impl Loader<EntityKey> for EntityLoader {
    type Value = EntityRow;
    // Must be `Clone`: one failed batch is reported to every waiting resolver.
    type Error = String;

    async fn load(&self, keys: &[EntityKey]) -> Result<HashMap<EntityKey, EntityRow>, String> {
        // One loader serves every entity type, so a layer that reaches several
        // targets becomes one statement per target, not one per key.
        let mut by_entity: HashMap<&str, Vec<String>> = HashMap::new();
        for key in keys {
            by_entity
                .entry(key.entity.as_str())
                .or_default()
                .push(key.id.clone());
        }

        let mut out = HashMap::with_capacity(keys.len());
        for (entity_name, ids) in by_entity {
            let entity = self
                .ir
                .entity(entity_name)
                .ok_or_else(|| format!("entity `{entity_name}` is not in the active schema"))?;

            let (query, fields) =
                sql::select_by_ids(&self.db_schema, entity, &ids).map_err(|e| e.to_string())?;
            let rows = self
                .db
                .query(&query.sql, &query.params_as_refs())
                .await
                .map_err(|e| e.to_string())?;

            for row in &rows {
                let decoded = decode_row(row, &fields).map_err(|e| e.to_string())?;
                // Keyed by the id the row actually carries; an id with no row is
                // simply absent, which `load_one` turns into `None`.
                if let Some(id) = decoded.get("id").and_then(|v| v.as_str()) {
                    out.insert(EntityKey::new(entity_name, id), decoded);
                }
            }
        }

        Ok(out)
    }
}
