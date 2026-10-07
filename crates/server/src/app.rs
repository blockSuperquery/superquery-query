//! Application assembly: startup sequence, shared state, router.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use async_graphql::dynamic::Schema;
use axum::routing::{get, post};
use axum::Router;
use superquery_graphql::{build_schema, Limits};
use superquery_postgres::{introspect, validate, Database, PgError};
use superquery_query_core::{parse_sdl, SchemaIr};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::config::Config;
use crate::{graphql_http, health};

/// State shared by every handler.
#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub ir: Arc<SchemaIr>,
    pub db_schema: String,
    /// Checked against the raw document before execution (aliases today).
    pub limits: Limits,
    /// Behind a lock so a future hot reload can replace it without a restart.
    /// `Schema` is internally reference-counted, so cloning it out per request
    /// is cheap and keeps the lock uncontended.
    schema: Arc<RwLock<Schema>>,
}

impl AppState {
    /// Take a snapshot of the current schema to execute against.
    pub fn schema(&self) -> Schema {
        self.schema
            .read()
            .expect("schema lock is never held across a panic")
            .clone()
    }

    /// Replace the active schema (Milestone 11). In-flight requests are
    /// unaffected — they already hold their own clone.
    pub fn replace_schema(&self, schema: Schema) {
        *self
            .schema
            .write()
            .expect("schema lock is never held across a panic") = schema;
    }
}

/// Bring the service up.
///
/// The order is load-bearing, and each step's failure is reported distinctly —
/// "cannot reach Postgres", "no such project", and "your schema does not match
/// the database" have completely different fixes:
///
/// ```text
///   read config
///        │
///   connect + ping Postgres        ─── fails ──▶ "cannot reach PostgreSQL"
///        │
///   check project schema exists    ─── fails ──▶ "no such project; available: …"
///        │
///   read _metadata                 ─── fails ──▶ "not a SuperQuery schema"
///        │
///   parse schema.graphql           ─── fails ──▶ parse error with location
///        │
///   validate IR against database   ─── fails ──▶ per-field mismatch list
///        │
///   build GraphQL schema
///        │
///   serve
/// ```
pub async fn build_state(config: &Config) -> Result<AppState> {
    let db = Database::connect(&config.db()).context("building the Postgres connection pool")?;

    db.ping()
        .await
        .context("connecting to PostgreSQL — check DB_HOST/DB_PORT/DB_USER/DB_PASS")?;

    // Project identity is the Postgres schema name. Getting this wrong is the
    // single most common misconfiguration, so the error lists what does exist.
    if !db.schema_exists(&config.name).await? {
        let available = db.list_project_schemas().await.unwrap_or_default();
        return Err(PgError::ProjectNotFound {
            schema: config.name.clone(),
            available: if available.is_empty() {
                "(none)".to_string()
            } else {
                available.join(", ")
            },
        }
        .into());
    }

    // Presence of `_metadata` is what distinguishes a project schema from any
    // other schema that happens to share the name.
    db.read_metadata(&config.name)
        .await
        .map_err(|_| PgError::NotAProjectSchema(config.name.clone()))?;

    let sdl = std::fs::read_to_string(&config.schema)
        .with_context(|| format!("reading project schema at {}", config.schema.display()))?;
    let ir = Arc::new(parse_sdl(&sdl).context("parsing the project schema")?);

    if ir.is_empty() {
        tracing::warn!(
            path = %config.schema.display(),
            "project schema declares no @entity types — the API will expose no queryable data"
        );
    }

    // The guarantee the guide asks for: fail loudly at startup if the database
    // and the project schema have diverged, rather than deep inside a resolver.
    let db_schema_info = introspect(&db, &config.name).await?;
    let warnings = validate(&ir, &db_schema_info).into_result()?;
    for warning in warnings {
        tracing::warn!("schema: {warning}");
    }

    let limits = config.limits();
    if limits.is_unrestricted() {
        tracing::warn!(
            "running with --unsafe: query depth, complexity and page-size limits are disabled. \
             Do not expose this instance to untrusted clients."
        );
    }

    let schema = build_schema(
        Arc::clone(&ir),
        db.clone(),
        config.name.clone(),
        limits.clone(),
    )
    .context("building the GraphQL schema")?;

    tracing::info!(
        project = %config.name,
        entities = ir.len(),
        "schema ready"
    );

    Ok(AppState {
        db,
        ir,
        db_schema: config.name.clone(),
        limits,
        schema: Arc::new(RwLock::new(schema)),
    })
}

/// Build the HTTP router.
pub fn router(state: AppState, config: &Config) -> Router {
    let mut app = Router::new()
        .route("/health", get(health::health))
        .route("/ready", get(health::ready))
        .route("/meta", get(health::meta))
        .route("/graphql", post(graphql_http::graphql_handler));

    if config.playground {
        app = app.route("/", get(graphql_http::playground));
    }

    app.layer(TraceLayer::new_for_http())
        // Applied before execution: a huge body is rejected without being parsed.
        .layer(RequestBodyLimitLayer::new(config.max_body_size))
        // 504 rather than the default 408: a timeout here means the database
        // took too long, not that the client was slow to send its request.
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::GATEWAY_TIMEOUT,
            Duration::from_millis(config.query_timeout),
        ))
        .with_state(state)
}
