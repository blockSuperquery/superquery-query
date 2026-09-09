//! Error taxonomy for the query service.
//!
//! Startup errors must say *which* of the three things is wrong — the database,
//! the project (db-schema) or the schema compatibility — because those have
//! completely different fixes. That distinction is encoded in the variants here
//! rather than in log strings.

use thiserror::Error;

/// Errors from pure query logic (schema IR, naming, filters, cursors).
#[derive(Debug, Error)]
pub enum CoreError {
    /// The SDL could not be parsed at all.
    #[error("failed to parse GraphQL schema: {0}")]
    SchemaParse(String),

    /// The SDL parsed, but contains something we cannot map to storage.
    #[error("unsupported type `{ty}` on field `{entity}.{field}`")]
    UnsupportedType {
        entity: String,
        field: String,
        ty: String,
    },

    /// An entity has no `id: ID!` field, so it has no primary key.
    #[error("entity `{0}` has no `id: ID!` field")]
    MissingId(String),

    /// A `@derivedFrom` pointed at something that does not exist.
    #[error("`{entity}.{field}` is @derivedFrom(field: \"{target}\") but {target_entity}.{target} does not exist")]
    BadDerivedFrom {
        entity: String,
        field: String,
        target_entity: String,
        target: String,
    },

    /// A client sent a cursor we cannot decode.
    #[error("invalid cursor: {0}")]
    InvalidCursor(String),

    /// A client referenced a field that is not on the entity.
    #[error("unknown field `{field}` on `{entity}`")]
    UnknownField { entity: String, field: String },

    /// A value could not be coerced to the field's type.
    #[error("invalid value for `{field}`: {reason}")]
    InvalidValue { field: String, reason: String },
}

/// Convenience alias.
pub type CoreResult<T> = Result<T, CoreError>;
