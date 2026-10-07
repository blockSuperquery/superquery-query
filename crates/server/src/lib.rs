//! # superquery-server
//!
//! Axum wiring: configuration, startup validation, health endpoints and the
//! GraphQL transports.
//!
//! ```text
//!   Config ──▶ build_state ──▶ AppState ──▶ router ──▶ serve
//!                  │
//!                  ├─ Postgres pool + ping
//!                  ├─ project schema exists?
//!                  ├─ parse schema.graphql
//!                  ├─ validate IR against the database
//!                  └─ build the dynamic GraphQL schema
//!
//!   reload::spawn ──▶ poll fingerprint ──▶ load_schema ──▶ AppState::replace_schema
//! ```

pub mod app;
pub mod config;
pub mod graphql_http;
pub mod graphql_ws;
pub mod health;
pub mod metrics;
pub mod reload;
pub mod schema_loader;

pub use app::{build_state, router, AppState};
pub use config::Config;

use anyhow::{Context, Result};

/// Start the service and serve until shutdown is signalled.
pub async fn serve(config: Config) -> Result<()> {
    let state = build_state(&config).await?;

    // Detached on purpose: the watcher only ever swaps the schema, so it has
    // nothing to flush, and it ends with the runtime at shutdown.
    if let Some(interval) = config.hot_schema() {
        reload::spawn(state.clone(), config.clone(), interval);
    }

    let app = router(state, &config);

    let address = config.bind_address();
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .with_context(|| format!("binding {address}"))?;

    tracing::info!(
        address = %address,
        playground = config.playground,
        "SuperQuery query service listening"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")
}

/// Wait for Ctrl-C or SIGTERM.
///
/// Graceful shutdown matters here beyond tidiness: without it, a rolling deploy
/// severs in-flight GraphQL requests mid-query, and clients see connection
/// resets rather than results.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl-C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("received Ctrl-C, shutting down"),
        () = terminate => tracing::info!("received SIGTERM, shutting down"),
    }
}
