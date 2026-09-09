//! Connection pool.
//!
//! Uses `tokio-postgres` + `deadpool-postgres`, matching `superquery-node`'s
//! stack so store-layer logic stays shareable between the repos. (The guide
//! suggests `sqlx`; its headline feature is compile-time-checked queries, which
//! buys nothing here because every statement we issue is generated at runtime
//! from the schema IR — and it would force a live database at build time.)
//!
//! # Read-only by construction
//!
//! This service must never write project entities; the node owns those. Rather
//! than rely on that as a convention, every connection sets
//! `default_transaction_read_only=on` at the session level, so an accidental
//! `INSERT` anywhere in this codebase fails against the server, not in review.
//! Operators should *also* run the service as a Postgres role with only
//! `SELECT`; this is defence in depth, not a substitute.

use std::collections::BTreeMap;

use deadpool_postgres::{Config as PoolConfig, Object, Pool, Runtime};
use tokio_postgres::NoTls;

use crate::error::{PgError, PgResult};

/// Connection settings. Env-var names match the node's `DB_*` so a single
/// docker-compose environment block configures both services.
#[derive(Debug, Clone)]
pub struct DbConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
    /// Upper bound on pooled connections. Upstream SubQuery's `--max-connection`.
    pub max_connections: usize,
}

impl Default for DbConfig {
    fn default() -> Self {
        // Defaults mirror the node's db config so the pair works out of the box.
        Self {
            host: "127.0.0.1".to_string(),
            port: 5432,
            user: "postgres".to_string(),
            password: "postgres".to_string(),
            database: "postgres".to_string(),
            max_connections: 10,
        }
    }
}

impl DbConfig {
    /// Read `DB_HOST`/`DB_PORT`/`DB_USER`/`DB_PASS`/`DB_DATABASE`, falling back
    /// to the same defaults the node uses.
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            host: std::env::var("DB_HOST").unwrap_or(d.host),
            port: std::env::var("DB_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d.port),
            user: std::env::var("DB_USER").unwrap_or(d.user),
            password: std::env::var("DB_PASS").unwrap_or(d.password),
            database: std::env::var("DB_DATABASE").unwrap_or(d.database),
            max_connections: d.max_connections,
        }
    }
}

/// A pooled, read-only Postgres handle.
#[derive(Clone)]
pub struct Database {
    pool: Pool,
}

impl Database {
    /// Build the pool. Deadpool is lazy, so this does not open a connection —
    /// call [`Database::ping`] to actually prove reachability.
    pub fn connect(cfg: &DbConfig) -> PgResult<Self> {
        let mut pool_cfg = PoolConfig::new();
        pool_cfg.host = Some(cfg.host.clone());
        pool_cfg.port = Some(cfg.port);
        pool_cfg.user = Some(cfg.user.clone());
        pool_cfg.password = Some(cfg.password.clone());
        pool_cfg.dbname = Some(cfg.database.clone());
        // Enforced server-side for every session this pool hands out.
        pool_cfg.options = Some("-c default_transaction_read_only=on".to_string());

        let pool = pool_cfg
            .create_pool(Some(Runtime::Tokio1), NoTls)
            .map_err(|e| PgError::Pool(e.to_string()))?;
        pool.resize(cfg.max_connections);

        Ok(Self { pool })
    }

    /// Borrow a pooled client.
    pub async fn conn(&self) -> PgResult<Object> {
        self.pool
            .get()
            .await
            .map_err(|e| PgError::Connection(e.to_string()))
    }

    /// Round-trip check. This is what `/ready` depends on, so it must actually
    /// touch the server rather than inspect pool state.
    pub async fn ping(&self) -> PgResult<()> {
        self.conn().await?.query_one("SELECT 1", &[]).await?;
        Ok(())
    }

    /// Run a parameterized query.
    pub async fn query(
        &self,
        sql: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> PgResult<Vec<tokio_postgres::Row>> {
        Ok(self.conn().await?.query(sql, params).await?)
    }

    /// Run a parameterized query expecting at most one row.
    pub async fn query_opt(
        &self,
        sql: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> PgResult<Option<tokio_postgres::Row>> {
        Ok(self.conn().await?.query_opt(sql, params).await?)
    }

    /// Whether a schema exists, and if not, what does — so the error can tell
    /// the operator which `--name` values are actually available.
    pub async fn schema_exists(&self, schema: &str) -> PgResult<bool> {
        let row = self
            .query_opt(
                "SELECT 1 FROM information_schema.schemata WHERE schema_name = $1",
                &[&schema],
            )
            .await?;
        Ok(row.is_some())
    }

    /// User-visible schemas, for the "did you mean" half of a startup error.
    pub async fn list_project_schemas(&self) -> PgResult<Vec<String>> {
        let rows = self
            .query(
                "SELECT schema_name FROM information_schema.schemata \
                 WHERE schema_name NOT IN ('information_schema', 'public') \
                   AND schema_name NOT LIKE 'pg\\_%' \
                 ORDER BY schema_name",
                &[],
            )
            .await?;
        Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
    }

    /// Read the node's `_metadata` key/value table for a project schema.
    pub async fn read_metadata(&self, schema: &str) -> PgResult<BTreeMap<String, String>> {
        validate_ident(schema)?;
        let rows = self
            .query(
                &format!(
                    "SELECT key, value FROM \"{schema}\".\"{}\"",
                    superquery_query_core::METADATA_TABLE
                ),
                &[],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<_, String>(0), r.get::<_, String>(1)))
            .collect())
    }
}

/// Postgres identifiers cannot be bound parameters, so any identifier that
/// reaches SQL text is restricted to a safe charset first.
///
/// This is the *only* place a non-generated identifier (the operator-supplied
/// `--name`) enters SQL. Entity and column names come from the schema IR, which
/// derives them from parsed SDL rather than from client input.
pub fn validate_ident(ident: &str) -> PgResult<()> {
    let ok = !ident.is_empty()
        && ident.len() <= 63
        && ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(PgError::InvalidIdentifier(ident.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_identifiers() {
        assert!(validate_ident("app").is_ok());
        assert!(validate_ident("app_1").is_ok());
        assert!(validate_ident("bad-name").is_err());
        assert!(validate_ident("drop\";--").is_err());
        assert!(validate_ident("").is_err());
        assert!(validate_ident(&"x".repeat(64)).is_err());
    }

    #[test]
    fn env_config_falls_back_to_node_defaults() {
        let c = DbConfig::default();
        assert_eq!(c.host, "127.0.0.1");
        assert_eq!(c.port, 5432);
        assert_eq!(c.max_connections, 10);
    }
}
