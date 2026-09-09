//! SuperQuery GraphQL query service.
//!
//! Reads entities indexed by `superquery-node` from PostgreSQL and serves them
//! through a GraphQL API generated from the project's `schema.graphql`.
//!
//! ```bash
//! superquery-query \
//!   --name app \
//!   --schema ./project/schema.graphql \
//!   --port 3000 \
//!   --playground
//! ```
//!
//! `--name` is the Postgres schema the node writes to (its `--db-schema`).

use anyhow::Result;
use clap::Parser;
use superquery_server::Config;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::parse();
    init_tracing(&config.log_level);

    // Startup errors are reported with their full context chain, so the operator
    // sees "connecting to PostgreSQL: connection refused" rather than either
    // half on its own.
    if let Err(err) = superquery_server::serve(config).await {
        tracing::error!("{err:#}");
        std::process::exit(1);
    }

    Ok(())
}

/// `RUST_LOG` wins if set, so an operator can get targeted debug output without
/// changing the deployment's `--log-level`.
fn init_tracing(level: &str) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("superquery={level},tower_http={level}")));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
