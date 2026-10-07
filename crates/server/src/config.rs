//! Configuration.
//!
//! Flag names track upstream SubQuery's `packages/query/src/yargs.ts` wherever
//! the concept survives the port, so an existing deployment's command line
//! mostly carries over. Two deliberate differences:
//!
//! - **`--schema`** is new. Upstream reads the schema through PostGraphile's
//!   database introspection; we parse the project's `schema.graphql` instead
//!   (introspection alone cannot distinguish `BigInt` from `BigDecimal` — both
//!   are `numeric` — nor recover entity casing from a pluralized table name).
//! - **`--indexer`** is dropped. Upstream calls the node's HTTP endpoint for
//!   metadata; we read `_metadata` straight from Postgres, so the query service
//!   has no dependency on the node being reachable.

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use superquery_graphql::Limits;
use superquery_postgres::DbConfig;

/// SuperQuery GraphQL query service.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "superquery-query",
    version,
    about = "SuperQuery GraphQL query service — serves node-indexed PostgreSQL data"
)]
pub struct Config {
    /// Project name — the Postgres schema the node writes to.
    ///
    /// Must match the node's `--db-schema`. This is the project's identity;
    /// there is no separate project id.
    #[arg(short = 'n', long, env = "SUPERQUERY_NAME")]
    pub name: String,

    /// Path to the project's `schema.graphql`.
    #[arg(long, env = "SUPERQUERY_SCHEMA")]
    pub schema: PathBuf,

    /// Port to bind.
    #[arg(short = 'p', long, default_value_t = 3000, env = "SUPERQUERY_PORT")]
    pub port: u16,

    /// Address to bind.
    #[arg(long, default_value = "0.0.0.0", env = "SUPERQUERY_HOST")]
    pub host: String,

    /// Serve the GraphiQL playground at `GET /`.
    #[arg(long, env = "SUPERQUERY_PLAYGROUND")]
    pub playground: bool,

    /// Maximum pooled Postgres connections.
    #[arg(long, default_value_t = 10, env = "SUPERQUERY_MAX_CONNECTION")]
    pub max_connection: usize,

    /// Maximum rows one connection field may return.
    #[arg(long, default_value_t = 100, env = "SUPERQUERY_QUERY_LIMIT")]
    pub query_limit: u32,

    /// Maximum query nesting depth.
    #[arg(long, default_value_t = 10, env = "SUPERQUERY_QUERY_DEPTH_LIMIT")]
    pub query_depth_limit: usize,

    /// Maximum query complexity score.
    #[arg(long, default_value_t = 1000, env = "SUPERQUERY_QUERY_COMPLEXITY")]
    pub query_complexity: usize,

    /// Maximum aliased fields in one query document.
    #[arg(
        long,
        default_value_t = superquery_graphql::limits::DEFAULT_MAX_ALIASES,
        env = "SUPERQUERY_QUERY_ALIAS_LIMIT"
    )]
    pub query_alias_limit: usize,

    /// Per-request time budget, in milliseconds.
    #[arg(long, default_value_t = 10_000, env = "SUPERQUERY_QUERY_TIMEOUT")]
    pub query_timeout: u64,

    /// Maximum request body size, in bytes.
    #[arg(long, default_value_t = 1024 * 1024, env = "SUPERQUERY_MAX_BODY_SIZE")]
    pub max_body_size: usize,

    /// Disable depth, complexity and page-size limits.
    ///
    /// Only for a private deployment behind a trusted gateway. The service logs
    /// a warning at startup when this is set.
    #[arg(long = "unsafe", env = "SUPERQUERY_UNSAFE")]
    pub unsafe_mode: bool,

    /// Log level (`trace`, `debug`, `info`, `warn`, `error`).
    #[arg(long, default_value = "info", env = "SUPERQUERY_LOG_LEVEL")]
    pub log_level: String,
}

impl Config {
    /// Database settings, from the same `DB_*` env vars the node reads.
    pub fn db(&self) -> DbConfig {
        DbConfig {
            max_connections: self.max_connection,
            ..DbConfig::from_env()
        }
    }

    /// Query protection limits.
    pub fn limits(&self) -> Limits {
        if self.unsafe_mode {
            return Limits::unsafe_unlimited();
        }
        Limits {
            max_depth: Some(self.query_depth_limit),
            max_complexity: Some(self.query_complexity),
            max_aliases: Some(self.query_alias_limit),
            max_page_size: self.query_limit,
            default_page_size: superquery_query_core::DEFAULT_PAGE_SIZE.min(self.query_limit),
            timeout: Duration::from_millis(self.query_timeout),
        }
    }

    pub fn bind_address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Config {
        Config::try_parse_from([&["superquery-query"], args].concat()).unwrap()
    }

    #[test]
    fn minimal_invocation_uses_documented_defaults() {
        let c = parse(&["--name", "app", "--schema", "./schema.graphql"]);
        assert_eq!(c.name, "app");
        assert_eq!(c.port, 3000);
        assert_eq!(c.query_limit, 100);
        assert_eq!(c.query_timeout, 10_000);
        assert_eq!(c.query_alias_limit, 50);
        assert!(!c.playground);
        assert!(!c.unsafe_mode);
    }

    #[test]
    fn name_and_schema_are_required() {
        assert!(Config::try_parse_from(["superquery-query"]).is_err());
        assert!(Config::try_parse_from(["superquery-query", "--name", "app"]).is_err());
    }

    #[test]
    fn limits_reflect_flags() {
        let c = parse(&[
            "--name",
            "app",
            "--schema",
            "s.graphql",
            "--query-depth-limit",
            "5",
            "--query-limit",
            "50",
            "--query-alias-limit",
            "7",
        ]);
        let l = c.limits();
        assert_eq!(l.max_depth, Some(5));
        assert_eq!(l.max_page_size, 50);
        assert_eq!(l.max_aliases, Some(7));
    }

    #[test]
    fn default_page_size_never_exceeds_the_max() {
        // --query-limit 5 must not leave a default page size of 20.
        let c = parse(&[
            "--name",
            "app",
            "--schema",
            "s.graphql",
            "--query-limit",
            "5",
        ]);
        assert_eq!(c.limits().default_page_size, 5);
    }

    #[test]
    fn unsafe_mode_removes_limits() {
        let c = parse(&["--name", "app", "--schema", "s.graphql", "--unsafe"]);
        assert!(c.limits().is_unrestricted());
    }

    #[test]
    fn bind_address_composes() {
        let c = parse(&["--name", "app", "--schema", "s.graphql", "--port", "8080"]);
        assert_eq!(c.bind_address(), "0.0.0.0:8080");
    }
}
