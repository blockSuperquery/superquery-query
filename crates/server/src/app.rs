//! Application assembly: startup sequence, shared state, router.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use async_graphql::dynamic::Schema;
use axum::routing::{get, post};
use axum::Router;
use superquery_graphql::Limits;
use superquery_postgres::{Database, PgError};
use superquery_query_core::SchemaIr;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::config::Config;
use crate::schema_loader::{load_schema, LoadedSchema};
use crate::{graphql_http, health, metrics};

/// State shared by every handler.
#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub db_schema: String,
    /// Checked against the raw document before execution (aliases today).
    pub limits: Limits,
    /// Queries allowed in one batched request; `None` when unlimited.
    pub batch_limit: Option<usize>,
    /// The served schema and its IR, behind one lock so hot reload replaces
    /// both at once. `Schema` is internally reference-counted, so cloning it
    /// out per request is cheap and keeps the lock uncontended.
    active: Arc<RwLock<LoadedSchema>>,
}

impl AppState {
    pub fn new(
        db: Database,
        db_schema: impl Into<String>,
        limits: Limits,
        batch_limit: Option<usize>,
        loaded: LoadedSchema,
    ) -> Self {
        Self {
            db,
            db_schema: db_schema.into(),
            limits,
            batch_limit,
            active: Arc::new(RwLock::new(loaded)),
        }
    }

    /// Take a snapshot of the current schema to execute against.
    pub fn schema(&self) -> Schema {
        self.active
            .read()
            .expect("schema lock is never held across a panic")
            .schema
            .clone()
    }

    /// The IR the current schema was generated from.
    pub fn ir(&self) -> Arc<SchemaIr> {
        Arc::clone(
            &self
                .active
                .read()
                .expect("schema lock is never held across a panic")
                .ir,
        )
    }

    /// Replace the active schema (Milestone 11). In-flight requests are
    /// unaffected — they already hold their own clone.
    pub fn replace_schema(&self, loaded: LoadedSchema) {
        *self
            .active
            .write()
            .expect("schema lock is never held across a panic") = loaded;
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

    let limits = config.limits();
    if limits.is_unrestricted() {
        tracing::warn!(
            "running with --unsafe: query depth, complexity and page-size limits are disabled. \
             Do not expose this instance to untrusted clients."
        );
    }

    let loaded = load_schema(&db, config, &limits).await?;

    tracing::info!(
        project = %config.name,
        entities = loaded.ir.len(),
        "schema ready"
    );

    Ok(AppState::new(
        db,
        config.name.clone(),
        limits,
        config.batch_limit(),
        loaded,
    ))
}

/// Build the HTTP router.
pub fn router(state: AppState, config: &Config) -> Router {
    let mut app = Router::new()
        .route("/health", get(health::health))
        .route("/ready", get(health::ready))
        .route("/meta", get(health::meta))
        .route("/metrics", get(metrics::metrics))
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
