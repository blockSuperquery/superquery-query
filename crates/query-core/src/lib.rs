//! # superquery-query-core
//!
//! Pure query logic with no I/O: the schema IR, the naming rules that bind us to
//! the node's table layout, and the filter/sort/cursor intermediate
//! representations that client input is funnelled through.
//!
//! Keeping this crate I/O-free is deliberate. Everything here is exhaustively
//! unit-testable without a database, and the security-critical boundary — client
//! input never becoming SQL syntax — is enforced by types that only this crate
//! can construct.
//!
//! ```text
//!   schema.graphql ──parse_sdl──▶ SchemaIr ──┬──▶ graphql: build dynamic schema
//!                                            └──▶ postgres: build SQL
//!
//!   client input ──▶ FilterExpr / Ordering / PageRequest ──▶ parameterized SQL
//! ```

pub mod error;
pub mod filter;
pub mod naming;
pub mod pagination;
pub mod project;
pub mod scalar;
pub mod schema;
pub mod sort;

pub use error::{CoreError, CoreResult};
pub use filter::{CmpOp, Field, FilterExpr};
pub use pagination::{
    Cursor, PageDirection, PageInfo, PageRequest, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE,
};
pub use project::{ProjectMeta, METADATA_TABLE};
pub use scalar::ScalarType;
pub use schema::{parse_sdl, Entity, Field as SchemaField, FieldKind, SchemaIr};
pub use sort::{Ordering, SortDirection, SortKey};
