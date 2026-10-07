//! Loading the project schema: parse → validate against the database → build.
//!
//! One function for both startup and hot reload, so a reload is held to exactly
//! the checks startup is. A reload that skipped validation would be the easiest
//! way to serve a schema whose columns no longer exist.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_graphql::dynamic::Schema;
use superquery_graphql::{build_schema, Limits};
use superquery_postgres::{introspect, validate, Database};
use superquery_query_core::{parse_sdl, SchemaIr};

use crate::config::Config;

/// A schema ready to serve, and the IR it was generated from.
///
/// Kept together because they must be swapped together: `/meta` reporting one
/// entity set while `/graphql` serves another is its own kind of outage.
#[derive(Clone)]
pub struct LoadedSchema {
    pub ir: Arc<SchemaIr>,
    pub schema: Schema,
}

/// Read `schema.graphql`, check it against the live database, and build the
/// GraphQL schema.
///
/// Every failure names its stage — reading the file, parsing it, or the
/// database disagreeing with it — because each has a different fix.
pub async fn load_schema(db: &Database, config: &Config, limits: &Limits) -> Result<LoadedSchema> {
    let sdl = std::fs::read_to_string(&config.schema)
        .with_context(|| format!("reading project schema at {}", config.schema.display()))?;
    let ir = Arc::new(parse_sdl(&sdl).context("parsing the project schema")?);

    if ir.is_empty() {
        tracing::warn!(
            path = %config.schema.display(),
            "project schema declares no @entity types — the API will expose no queryable data"
        );
    }

    // The guarantee the guide asks for: fail loudly if the database and the
    // project schema have diverged, rather than deep inside a resolver.
    let db_schema_info = introspect(db, &config.name).await?;
    let warnings = validate(&ir, &db_schema_info).into_result()?;
    for warning in warnings {
        tracing::warn!("schema: {warning}");
    }

    let schema = build_schema(
        Arc::clone(&ir),
        db.clone(),
        config.name.clone(),
        limits.clone(),
    )
    .context("building the GraphQL schema")?;

    Ok(LoadedSchema { ir, schema })
}
