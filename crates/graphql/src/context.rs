//! Shared state every resolver reaches through `ctx.data()`.

use std::sync::Arc;

use async_graphql::dataloader::DataLoader;
use superquery_postgres::Database;
use superquery_query_core::SchemaIr;

use crate::limits::Limits;
use crate::loader::{DerivedLoader, EntityLoader};

/// Everything a resolver needs: the connection pool, the schema IR the API was
/// generated from, and which Postgres schema to read.
///
/// Cheap to clone — `Database` wraps a pooled handle and `SchemaIr` is behind an
/// `Arc`, so hot reload (Milestone 11) can swap in a new schema without
/// disturbing in-flight requests holding the old one.
#[derive(Clone)]
pub struct QueryContext {
    pub db: Database,
    pub ir: Arc<SchemaIr>,
    /// The project's Postgres schema — its identity. See `query-core::project`.
    pub db_schema: String,
    pub limits: Limits,
    /// Batches forward-relation lookups across a layer. Shared, not per
    /// request; see `loader` for why that is safe without a cache.
    pub entities: Arc<DataLoader<EntityLoader>>,
    /// Batches `@derivedFrom` reverse relations by parent id.
    pub derived: Arc<DataLoader<DerivedLoader>>,
}

impl QueryContext {
    pub fn new(
        db: Database,
        ir: Arc<SchemaIr>,
        db_schema: impl Into<String>,
        limits: Limits,
    ) -> Self {
        let db_schema = db_schema.into();
        let entities = DataLoader::new(
            EntityLoader::new(db.clone(), Arc::clone(&ir), db_schema.clone()),
            tokio::spawn,
        );
        // Matches what the unbatched resolver returned: one default-sized page
        // per parent, never more than the configured maximum.
        let per_parent_limit = limits.default_page_size.min(limits.max_page_size);
        let derived = DataLoader::new(
            DerivedLoader::new(
                db.clone(),
                Arc::clone(&ir),
                db_schema.clone(),
                per_parent_limit,
            ),
            tokio::spawn,
        );
        Self {
            db,
            ir,
            db_schema,
            limits,
            entities: Arc::new(entities),
            derived: Arc::new(derived),
        }
    }
}
