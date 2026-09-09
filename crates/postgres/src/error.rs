//! Postgres-layer errors.
//!
//! Startup failures are separated by *cause* so the operator is told which of the
//! three independent things is wrong. "Connection refused" and "that project
//! isn't in this database" and "the node indexed a different schema version" have
//! nothing to do with each other, and collapsing them into one message is the
//! single most common way a service like this wastes someone's afternoon.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PgError {
    /// Could not reach Postgres at all — wrong host/port/credentials, or it's down.
    #[error("cannot reach PostgreSQL: {0}")]
    Connection(String),

    /// Reached Postgres, but the named project schema does not exist.
    #[error(
        "project schema `{schema}` does not exist in this database. \
         Check --name matches the node's --db-schema. Schemas present: {available}"
    )]
    ProjectNotFound { schema: String, available: String },

    /// The schema exists but has no `_metadata` table — it was not created by a node.
    #[error(
        "schema `{0}` exists but has no `_metadata` table; \
         it does not look like a SuperQuery project schema"
    )]
    NotAProjectSchema(String),

    /// The project schema and the supplied `schema.graphql` disagree.
    #[error("project schema does not match the database:\n{0}")]
    SchemaMismatch(String),

    /// An identifier failed validation before interpolation into DDL/DML.
    #[error("invalid identifier `{0}`")]
    InvalidIdentifier(String),

    /// A query failed at execution time.
    #[error("query failed: {0}")]
    Query(#[from] tokio_postgres::Error),

    /// Connection-pool exhaustion or configuration failure.
    #[error("connection pool error: {0}")]
    Pool(String),

    /// Bubbled up from pure logic (bad cursor, unknown field, …).
    #[error(transparent)]
    Core(#[from] superquery_query_core::CoreError),
}

pub type PgResult<T> = Result<T, PgError>;
