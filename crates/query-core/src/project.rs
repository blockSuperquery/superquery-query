//! Project metadata — what the node tells us about the state of the index.
//!
//! # The real contract
//!
//! The implementation guide describes a `_superquery_metadata` table with columns
//! `project_id`, `schema_version`, `query_schema_ir`, and so on. That table does
//! not exist. What the node actually writes is:
//!
//! ```text
//!   <db-schema>._metadata
//!   ┌─────────────────────────────┬──────────────┐
//!   │ key                    text │ value   text │  ← PRIMARY KEY (key)
//!   ├─────────────────────────────┼──────────────┤
//!   │ lastProcessedHeight         │ 21012345     │
//!   │ lastFinalizedVerifiedHeight │ 21012330     │
//!   │ targetHeight                │ 21012400     │
//!   │ chain                       │ stellar      │
//!   │ historicalStateEnabled      │ true         │
//!   └─────────────────────────────┴──────────────┘
//! ```
//!
//! It is a **key/value table inside a per-project Postgres schema**. There is no
//! `project_id` column, because the Postgres schema name *is* the project
//! identity: the node is launched with `--db-schema=app` and the query service
//! with `--name=app` (upstream SubQuery uses exactly this pairing).
//!
//! Values are all `text` and are parsed defensively here — a key that is absent,
//! empty, or unparseable yields `None` rather than failing startup, because the
//! node writes them incrementally as it indexes.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Metadata keys written by the node. Names are the node's, verbatim — they are
/// camelCase strings in a text column, not identifiers, so they are not subject
/// to the `underscored` naming rules.
pub mod keys {
    pub const LAST_PROCESSED_HEIGHT: &str = "lastProcessedHeight";
    pub const LAST_PROCESSED_BLOCK_TIMESTAMP: &str = "lastProcessedBlockTimestamp";
    pub const LAST_FINALIZED_VERIFIED_HEIGHT: &str = "lastFinalizedVerifiedHeight";
    pub const TARGET_HEIGHT: &str = "targetHeight";
    pub const START_HEIGHT: &str = "startHeight";
    pub const CHAIN: &str = "chain";
    pub const GENESIS_HASH: &str = "genesisHash";
    pub const SPEC_NAME: &str = "specName";
    pub const INDEXER_NODE_VERSION: &str = "indexerNodeVersion";
    pub const PROCESSED_BLOCK_COUNT: &str = "processedBlockCount";
    pub const SCHEMA_MIGRATION_COUNT: &str = "schemaMigrationCount";
    pub const HISTORICAL_STATE_ENABLED: &str = "historicalStateEnabled";
    pub const DEPLOYMENTS: &str = "deployments";
    pub const DYNAMIC_DATASOURCES: &str = "dynamicDatasources";
    pub const BLOCK_OFFSET: &str = "blockOffset";
    pub const LATEST_SYNCED_POI_HEIGHT: &str = "latestSyncedPoiHeight";
    pub const LAST_CREATED_POI_HEIGHT: &str = "lastCreatedPoiHeight";
}

/// The name of the node's metadata table within a project schema.
pub const METADATA_TABLE: &str = "_metadata";

/// A typed view over the node's `_metadata` key/value rows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectMeta {
    /// The Postgres schema this project lives in — its identity.
    pub db_schema: String,
    /// Highest block the node has fully indexed.
    pub last_processed_height: Option<i64>,
    /// Highest block confirmed final (below this, no reorg is expected).
    pub last_finalized_verified_height: Option<i64>,
    /// Chain tip the node is working toward.
    pub target_height: Option<i64>,
    /// First block the project indexes.
    pub start_height: Option<i64>,
    pub chain: Option<String>,
    pub genesis_hash: Option<String>,
    pub indexer_node_version: Option<String>,
    /// Whether the node is storing entity history. Gates the `block:` selector.
    pub historical_state_enabled: bool,
    /// Bumped by the node on each schema migration; a change means our generated
    /// GraphQL schema may be stale (hot reload, Milestone 11).
    pub schema_migration_count: Option<i64>,
    /// Every other key, unparsed, so `/meta` can surface new node keys without
    /// a query-service release.
    pub extra: BTreeMap<String, String>,
}

impl ProjectMeta {
    /// Build from raw key/value rows.
    pub fn from_rows(db_schema: impl Into<String>, rows: BTreeMap<String, String>) -> Self {
        let num = |k: &str| rows.get(k).and_then(|v| parse_height(v));
        let text = |k: &str| rows.get(k).filter(|v| !v.is_empty()).cloned();

        let known = [
            keys::LAST_PROCESSED_HEIGHT,
            keys::LAST_FINALIZED_VERIFIED_HEIGHT,
            keys::TARGET_HEIGHT,
            keys::START_HEIGHT,
            keys::CHAIN,
            keys::GENESIS_HASH,
            keys::INDEXER_NODE_VERSION,
            keys::HISTORICAL_STATE_ENABLED,
            keys::SCHEMA_MIGRATION_COUNT,
        ];

        Self {
            db_schema: db_schema.into(),
            last_processed_height: num(keys::LAST_PROCESSED_HEIGHT),
            last_finalized_verified_height: num(keys::LAST_FINALIZED_VERIFIED_HEIGHT),
            target_height: num(keys::TARGET_HEIGHT),
            start_height: num(keys::START_HEIGHT),
            chain: text(keys::CHAIN),
            genesis_hash: text(keys::GENESIS_HASH),
            indexer_node_version: text(keys::INDEXER_NODE_VERSION),
            historical_state_enabled: rows
                .get(keys::HISTORICAL_STATE_ENABLED)
                .is_some_and(|v| v == "true"),
            schema_migration_count: num(keys::SCHEMA_MIGRATION_COUNT),
            extra: rows
                .iter()
                .filter(|(k, _)| !known.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    /// Whether the node has indexed anything at all. Readiness depends on this:
    /// a schema with tables but no processed height is still warming up.
    pub fn has_indexed_data(&self) -> bool {
        self.last_processed_height.is_some()
    }

    /// How far behind the chain tip the index is, when both heights are known.
    pub fn blocks_behind(&self) -> Option<i64> {
        match (self.target_height, self.last_processed_height) {
            (Some(t), Some(l)) => Some((t - l).max(0)),
            _ => None,
        }
    }
}

/// Parse a height value. The node writes plain integers, but values have been
/// JSON-quoted by some versions, so tolerate surrounding quotes.
fn parse_height(raw: &str) -> Option<i64> {
    raw.trim().trim_matches('"').parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parses_known_keys() {
        let m = ProjectMeta::from_rows(
            "app",
            rows(&[
                (keys::LAST_PROCESSED_HEIGHT, "21012345"),
                (keys::TARGET_HEIGHT, "21012400"),
                (keys::CHAIN, "stellar"),
                (keys::HISTORICAL_STATE_ENABLED, "true"),
            ]),
        );
        assert_eq!(m.last_processed_height, Some(21012345));
        assert_eq!(m.target_height, Some(21012400));
        assert_eq!(m.chain.as_deref(), Some("stellar"));
        assert!(m.historical_state_enabled);
        assert_eq!(m.blocks_behind(), Some(55));
    }

    #[test]
    fn unknown_keys_are_preserved_not_dropped() {
        // A newer node writing a key we don't model should still show in /meta.
        let m = ProjectMeta::from_rows("app", rows(&[("somethingNew", "42")]));
        assert_eq!(m.extra.get("somethingNew").map(String::as_str), Some("42"));
    }

    #[test]
    fn missing_metadata_is_not_an_error() {
        let m = ProjectMeta::from_rows("app", BTreeMap::new());
        assert!(!m.has_indexed_data());
        assert!(!m.historical_state_enabled);
        assert_eq!(m.blocks_behind(), None);
    }

    #[test]
    fn tolerates_quoted_and_padded_numbers() {
        let m = ProjectMeta::from_rows("app", rows(&[(keys::LAST_PROCESSED_HEIGHT, "\"123\" ")]));
        assert_eq!(m.last_processed_height, Some(123));
    }

    #[test]
    fn garbage_height_does_not_panic() {
        let m = ProjectMeta::from_rows("app", rows(&[(keys::TARGET_HEIGHT, "not-a-number")]));
        assert_eq!(m.target_height, None);
    }

    #[test]
    fn blocks_behind_never_negative() {
        // Node can briefly report a processed height above a stale target.
        let m = ProjectMeta::from_rows(
            "app",
            rows(&[
                (keys::LAST_PROCESSED_HEIGHT, "100"),
                (keys::TARGET_HEIGHT, "90"),
            ]),
        );
        assert_eq!(m.blocks_behind(), Some(0));
    }
}
