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

/// The rows on the owning side of a `@derivedFrom` relation, for one parent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DerivedKey {
    /// The owning entity, e.g. `Transfer`.
    pub entity: String,
    /// Its foreign-key field pointing back at the parent, e.g. `fromAccount`.
    pub fk_field: String,
    pub parent_id: String,
}

/// Loads reverse relations, batched by parent id.
pub struct DerivedLoader {
    db: Database,
    ir: Arc<SchemaIr>,
    db_schema: String,
    /// Rows returned per parent. Reverse relations are plain lists, not
    /// connections, so this cap is what keeps one popular parent bounded.
    per_parent_limit: u32,
}

impl DerivedLoader {
    pub fn new(
        db: Database,
        ir: Arc<SchemaIr>,
        db_schema: impl Into<String>,
        per_parent_limit: u32,
    ) -> Self {
        Self {
            db,
            ir,
            db_schema: db_schema.into(),
            per_parent_limit,
        }
    }
}

impl Loader<DerivedKey> for DerivedLoader {
    type Value = Vec<EntityRow>;
    type Error = String;

    async fn load(
        &self,
        keys: &[DerivedKey],
    ) -> Result<HashMap<DerivedKey, Vec<EntityRow>>, String> {
        // Two different @derivedFrom fields in one layer target different
        // tables or columns, so each (entity, fk) pair is its own statement.
        let mut groups: HashMap<(&str, &str), Vec<String>> = HashMap::new();
        for key in keys {
            groups
                .entry((key.entity.as_str(), key.fk_field.as_str()))
                .or_default()
                .push(key.parent_id.clone());
        }

        let mut out = HashMap::with_capacity(keys.len());
        for ((entity_name, fk_field), parent_ids) in groups {
            let entity = self
                .ir
                .entity(entity_name)
                .ok_or_else(|| format!("entity `{entity_name}` is not in the active schema"))?;

            let (query, fields) = sql::select_by_parent_ids(
                &self.db_schema,
                entity,
                fk_field,
                &parent_ids,
                self.per_parent_limit,
            )
            .map_err(|e| e.to_string())?;
            let rows = self
                .db
                .query(&query.sql, &query.params_as_refs())
                .await
                .map_err(|e| e.to_string())?;

            let decoded = rows
                .iter()
                .map(|r| decode_row(r, &fields).map_err(|e| e.to_string()))
                .collect::<Result<Vec<_>, _>>()?;
            let mut grouped = group_by_parent(decoded, fk_field);

            // Every requested parent gets an entry. A parent with no children
            // is an empty list — the field is `[T!]!`, so null would be wrong.
            for parent_id in parent_ids {
                let rows = grouped.remove(&parent_id).unwrap_or_default();
                out.insert(
                    DerivedKey {
                        entity: entity_name.to_string(),
                        fk_field: fk_field.to_string(),
                        parent_id,
                    },
                    rows,
                );
            }
        }

        Ok(out)
    }
}

/// Split a batched result back out by the parent id each row points at.
///
/// Row order within a parent is preserved, so the `id` ordering the SQL
/// applied survives the regrouping.
pub(crate) fn group_by_parent(
    rows: Vec<EntityRow>,
    fk_field: &str,
) -> HashMap<String, Vec<EntityRow>> {
    let mut grouped: HashMap<String, Vec<EntityRow>> = HashMap::new();
    for row in rows {
        let Some(parent) = row.get(fk_field).and_then(|v| v.as_str()) else {
            continue;
        };
        grouped.entry(parent.to_string()).or_default().push(row);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(id: &str, parent: Option<&str>) -> EntityRow {
        let mut r = EntityRow::new();
        r.insert("id".into(), json!(id));
        r.insert(
            "fromAccount".into(),
            parent.map_or(serde_json::Value::Null, |p| json!(p)),
        );
        r
    }

    fn ids(rows: &[EntityRow]) -> Vec<&str> {
        rows.iter()
            .map(|r| r.get("id").and_then(|v| v.as_str()).unwrap())
            .collect()
    }

    #[test]
    fn batched_rows_go_back_to_their_own_parent() {
        let rows = vec![
            row("t1", Some("a")),
            row("t2", Some("b")),
            row("t3", Some("a")),
        ];
        let grouped = group_by_parent(rows, "fromAccount");
        assert_eq!(ids(&grouped["a"]), ["t1", "t3"]);
        assert_eq!(ids(&grouped["b"]), ["t2"]);
    }

    #[test]
    fn grouping_preserves_sql_order_within_a_parent() {
        // The statement orders by (fk, id); regrouping must not reshuffle it.
        let rows = vec![
            row("t1", Some("a")),
            row("t2", Some("a")),
            row("t9", Some("a")),
        ];
        let grouped = group_by_parent(rows, "fromAccount");
        assert_eq!(ids(&grouped["a"]), ["t1", "t2", "t9"]);
    }

    #[test]
    fn rows_without_a_parent_are_dropped() {
        let grouped = group_by_parent(vec![row("t1", None)], "fromAccount");
        assert!(grouped.is_empty());
    }

    #[test]
    fn keys_for_the_same_row_are_equal() {
        // DataLoader de-duplicates on key equality: two parents pointing at one
        // account must collapse to a single id in the batch.
        assert_eq!(
            EntityKey::new("Account", "a"),
            EntityKey::new("Account", "a")
        );
        assert_ne!(EntityKey::new("Account", "a"), EntityKey::new("Pool", "a"));
    }
}
