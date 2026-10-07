//! # superquery-graphql
//!
//! Builds the public GraphQL API at runtime from a project's schema IR, using
//! `async_graphql::dynamic`.
//!
//! ```text
//!   SchemaIr ──▶ build_schema ──▶ async_graphql::dynamic::Schema
//!                                        │
//!                    resolvers ──────────┘
//!                        │
//!                        ├─ filters::parse_filter  ─┐
//!                        ├─ ordering::parse_ordering├─▶ IR ─▶ postgres::sql
//!                        └─ PageRequest::resolve   ─┘
//! ```
//!
//! No resolver builds SQL text itself; each converts arguments into IR and hands
//! them to `superquery-postgres`.

pub mod context;
pub mod filters;
pub mod limits;
pub mod loader;
pub mod naming;
pub mod ordering;
pub mod scalars;
pub mod schema;
pub mod subscriptions;

pub use context::QueryContext;
pub use limits::Limits;
pub use schema::build_schema;
