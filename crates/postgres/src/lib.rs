//! # superquery-postgres
//!
//! Read-only Postgres access for the query service.
//!
//! ```text
//!   SchemaIr ─┬─▶ validate ──▶ startup: does the DB match the project schema?
//!             └─▶ sql      ──▶ parameterized statements ──▶ row ──▶ JSON
//!
//!   _metadata ──▶ ProjectMeta ──▶ /meta, /ready, historical gating
//! ```
//!
//! Every connection this crate hands out is `default_transaction_read_only`;
//! the node owns all writes.

pub mod error;
pub mod historical;
pub mod introspect;
pub mod notify;
pub mod pool;
pub mod row;
pub mod sql;
pub mod validate;

pub use error::{PgError, PgResult};
pub use introspect::{introspect, ColumnInfo, SchemaInfo, TableInfo};
pub use pool::{Database, DbConfig};
pub use row::{decode_row, EntityRow};
pub use sql::{ProjectedField, SqlQuery};
pub use validate::{validate, ValidationReport};
