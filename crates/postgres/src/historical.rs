//! Historical reads — **not implemented**, and deliberately so.
//!
//! # Why this is a stub
//!
//! Historical queries cannot be implemented correctly in the query service
//! alone. They require the node to store *versions* of each entity with a
//! validity range, conventionally an `int8range` column `_block_range`, and to
//! make `id` non-unique so multiple versions coexist:
//!
//! ```text
//!   id    │ value │ _block_range
//!   ──────┼───────┼──────────────
//!   0xab  │ 100   │ [10, 20)      ← superseded at height 20
//!   0xab  │ 250   │ [20, )        ← current
//! ```
//!
//! A query at height 15 then adds `WHERE _block_range @> 15::bigint`.
//!
//! As of this writing `superquery-node`'s store writes **no** `_block_range`
//! column — its `PlainModel` upserts in place, and the historical slice is
//! explicitly marked as later work. Exposing a `block:` argument now would
//! return present-day values for every height, which is worse than not offering
//! the feature: it looks like it works.
//!
//! # Enabling this (Milestone 9)
//!
//! 1. Node lands historical storage and sets `historicalStateEnabled=true` in
//!    `_metadata`.
//! 2. [`is_supported`] starts returning true for that project.
//! 3. The GraphQL layer adds the `block: {height: …}` argument **only** when it
//!    does, so a non-historical project's schema never advertises it.
//! 4. [`block_range_predicate`] joins the collection query's WHERE clause.

use superquery_query_core::ProjectMeta;

/// The column the node uses for entity validity ranges, once it has one.
pub const BLOCK_RANGE_COLUMN: &str = "_block_range";

/// Whether this project can answer historical queries.
///
/// Gated on the node's own `historicalStateEnabled` flag rather than on the
/// presence of the column, so the node stays the authority on its own semantics.
pub fn is_supported(meta: &ProjectMeta) -> bool {
    meta.historical_state_enabled
}

/// The predicate selecting the version of each row live at `height`.
///
/// Returns the SQL fragment and the parameter to bind. Not wired into
/// `select_collection` yet — see the module docs.
pub fn block_range_predicate(height: i64, placeholder: &str) -> (String, Option<String>) {
    (
        format!("\"{BLOCK_RANGE_COLUMN}\" @> {placeholder}::text::bigint"),
        Some(height.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn gated_on_node_flag() {
        let off = ProjectMeta::from_rows("app", BTreeMap::new());
        assert!(!is_supported(&off));

        let on = ProjectMeta::from_rows(
            "app",
            BTreeMap::from([("historicalStateEnabled".to_string(), "true".to_string())]),
        );
        assert!(is_supported(&on));
    }

    #[test]
    fn predicate_uses_range_containment() {
        let (sql, param) = block_range_predicate(21012345, "$1");
        assert_eq!(sql, r#""_block_range" @> $1::text::bigint"#);
        assert_eq!(param, Some("21012345".to_string()));
    }
}
