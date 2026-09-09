//! Health, readiness and metadata endpoints.
//!
//! # Liveness vs readiness
//!
//! These answer different questions and must not be conflated:
//!
//! - `/health` — *is the process alive?* Always 200 while we can serve a
//!   request. An orchestrator restarts the pod when this fails, so it must not
//!   depend on Postgres: a database blip would otherwise trigger a restart loop
//!   that fixes nothing.
//! - `/ready` — *can this instance serve queries right now?* Requires a live
//!   database round-trip. Failing here removes the instance from the load
//!   balancer without killing it, which is the correct response to a DB outage.
//!
//! This is the acceptance criterion from the implementation guide's Milestone 1:
//! "DB disconnect changes readiness state appropriately."

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::app::AppState;

/// `GET /health` — liveness. Deliberately does not touch the database.
pub async fn health() -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") })),
    )
}

/// `GET /ready` — readiness. 200 only when the database is reachable.
pub async fn ready(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    match state.db.ping().await {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({ "status": "ready", "project": state.db_schema })),
        ),
        Err(err) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "not_ready",
                "reason": "database unreachable",
                "detail": err.to_string(),
            })),
        ),
    }
}

/// `GET /meta` — index state.
///
/// Reports only what is safe to publish: heights, chain and schema shape. No
/// connection strings, credentials or RPC endpoints — this endpoint is
/// frequently exposed publicly alongside `/graphql`.
pub async fn meta(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let rows = match state.db.read_metadata(&state.db_schema).await {
        Ok(rows) => rows,
        Err(err) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": err.to_string() })),
            )
        }
    };

    let meta = superquery_query_core::ProjectMeta::from_rows(&state.db_schema, rows);

    (
        StatusCode::OK,
        Json(json!({
            "project": meta.db_schema,
            "chain": meta.chain,
            "indexedHeight": meta.last_processed_height,
            "finalizedHeight": meta.last_finalized_verified_height,
            "targetHeight": meta.target_height,
            "blocksBehind": meta.blocks_behind(),
            "historicalStateEnabled": meta.historical_state_enabled,
            "indexerNodeVersion": meta.indexer_node_version,
            "entities": state.ir.entities().map(|e| e.name.clone()).collect::<Vec<_>>(),
            "queryVersion": env!("CARGO_PKG_VERSION"),
        })),
    )
}
