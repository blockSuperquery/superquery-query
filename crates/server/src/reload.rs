//! Schema hot reload (Milestone 11).
//!
//! ```text
//!   every interval:
//!     fingerprint = (_metadata.schemaMigrationCount, hash(schema.graphql))
//!          │
//!          ├─ unchanged ──▶ nothing
//!          │
//!          └─ changed ──▶ load_schema (same checks as startup)
//!                            ├─ ok   ──▶ atomic swap; new requests see it
//!                            └─ fail ──▶ old schema stays live, error logged
//! ```
//!
//! # Why polling
//!
//! The node bumps `schemaMigrationCount` when it migrates, but does not yet
//! `NOTIFY` (see `postgres::notify::schema_channel`). Polling one small
//! key/value table is cheap, and swapping to `LISTEN` later changes only how
//! this loop wakes up, not what it does.
//!
//! The SDL file is part of the fingerprint because the operator ships it: a
//! migration and a new `schema.graphql` usually arrive together, and either
//! one alone is still a reason to rebuild.
//!
//! # Guarantees
//!
//! - In-flight requests are untouched: each already holds its own schema clone.
//! - A failed rebuild leaves the previous schema serving. A database that no
//!   longer matches *any* loadable schema is an operator problem; serving the
//!   last good schema and logging loudly is better than serving nothing.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use anyhow::{Context, Result};
use superquery_query_core::ProjectMeta;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use crate::app::AppState;
use crate::config::Config;
use crate::schema_loader::load_schema;

/// What has to change for a rebuild to be worth attempting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub migration_count: Option<i64>,
    /// Change detection only, not integrity: a collision costs one missed
    /// reload, which the next migration bump recovers.
    pub sdl_hash: u64,
}

impl Fingerprint {
    pub fn new(migration_count: Option<i64>, sdl: &[u8]) -> Self {
        let mut hasher = DefaultHasher::new();
        sdl.hash(&mut hasher);
        Self {
            migration_count,
            sdl_hash: hasher.finish(),
        }
    }
}

/// Read the current fingerprint from the database and the schema file.
pub async fn fingerprint(state: &AppState, config: &Config) -> Result<Fingerprint> {
    let rows = state
        .db
        .read_metadata(&config.name)
        .await
        .context("reading _metadata")?;
    let meta = ProjectMeta::from_rows(&config.name, rows);
    let sdl = tokio::fs::read(&config.schema)
        .await
        .with_context(|| format!("reading {}", config.schema.display()))?;
    Ok(Fingerprint::new(meta.schema_migration_count, &sdl))
}

/// Whether `next` warrants a rebuild, given what is serving now.
///
/// An unknown current fingerprint (the first read failed) always reloads: the
/// rebuild is idempotent, and it is the only way to learn a baseline.
pub fn should_reload(current: Option<&Fingerprint>, next: &Fingerprint) -> bool {
    current != Some(next)
}

/// Start the watcher in the background.
pub fn spawn(state: AppState, config: Config, interval: Duration) -> JoinHandle<()> {
    tokio::spawn(watch(state, config, interval))
}

async fn watch(state: AppState, config: Config, interval: Duration) {
    let mut current = match fingerprint(&state, &config).await {
        Ok(f) => Some(f),
        Err(err) => {
            tracing::warn!("hot reload: no baseline fingerprint yet: {err:#}");
            None
        }
    };
    // Remembered so a persistently broken schema is logged once, not on every
    // tick, while still being retried in case the cause was transient.
    let mut last_failed: Option<Fingerprint> = None;

    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // The first tick fires immediately; the baseline above already covers it.
    ticker.tick().await;

    tracing::info!(
        interval_ms = interval.as_millis() as u64,
        "schema hot reload enabled"
    );

    loop {
        ticker.tick().await;

        let next = match fingerprint(&state, &config).await {
            Ok(f) => f,
            Err(err) => {
                // Readiness already reports a database outage; nothing to add.
                tracing::debug!("hot reload: fingerprint unavailable: {err:#}");
                continue;
            }
        };

        if !should_reload(current.as_ref(), &next) {
            continue;
        }

        match load_schema(&state.db, &config, &state.limits).await {
            Ok(loaded) => {
                tracing::info!(
                    entities = loaded.ir.len(),
                    migration_count = ?next.migration_count,
                    "schema change detected; new schema is live"
                );
                state.replace_schema(loaded);
                current = Some(next);
                last_failed = None;
            }
            Err(err) => {
                if last_failed.as_ref() != Some(&next) {
                    tracing::error!(
                        "schema change detected but the new schema failed to load; \
                         the previous schema stays live: {err:#}"
                    );
                    last_failed = Some(next);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_inputs_do_not_reload() {
        let a = Fingerprint::new(Some(3), b"type A @entity { id: ID! }");
        let b = Fingerprint::new(Some(3), b"type A @entity { id: ID! }");
        assert!(!should_reload(Some(&a), &b));
    }

    #[test]
    fn a_migration_bump_reloads() {
        let a = Fingerprint::new(Some(3), b"sdl");
        let b = Fingerprint::new(Some(4), b"sdl");
        assert!(should_reload(Some(&a), &b));
    }

    #[test]
    fn a_new_schema_file_reloads() {
        let a = Fingerprint::new(Some(3), b"type A @entity { id: ID! }");
        let b = Fingerprint::new(Some(3), b"type A @entity { id: ID! n: Int }");
        assert!(should_reload(Some(&a), &b));
    }

    #[test]
    fn no_baseline_always_reloads() {
        assert!(should_reload(None, &Fingerprint::new(None, b"")));
    }
}
