//! Startup validation: does the database match the project schema we were given?
//!
//! # Why this exists
//!
//! The query service is handed a `schema.graphql` and a database that some node
//! populated. Nothing guarantees they are the same version. If they are not, the
//! failure mode without this check is terrible: the service starts happily,
//! serves a GraphQL schema that looks right, and then returns
//! `column "foo" does not exist` from deep inside a resolver, at 3am, to a dApp.
//!
//! So the whole schema is checked once, at startup, and every problem is
//! reported together rather than one-per-restart.
//!
//! # What is deliberately *not* an error
//!
//! - **Extra tables/columns in the DB.** The node writes bookkeeping the query
//!   layer neither knows nor cares about (`_metadata`, `_poi`, historical
//!   columns). Requiring an exact match would break on every node feature.
//! - **Nullability mismatches.** Reported as warnings. Historical mode and
//!   mid-migration states legitimately relax NOT NULL, and refusing to start
//!   over it would be worse than serving a null the schema says is non-null.
//!
//! A missing table or a type mismatch *is* an error: neither can produce correct
//! results.

use superquery_query_core::schema::{Entity, SchemaIr};

use crate::error::{PgError, PgResult};
use crate::introspect::SchemaInfo;

/// Outcome of validating the IR against the live schema.
#[derive(Debug, Default)]
pub struct ValidationReport {
    /// Problems that make correct results impossible.
    pub errors: Vec<String>,
    /// Divergences worth logging that we can still serve through.
    pub warnings: Vec<String>,
}

impl ValidationReport {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    /// Turn a failed report into the startup error.
    pub fn into_result(self) -> PgResult<Vec<String>> {
        if self.is_ok() {
            Ok(self.warnings)
        } else {
            Err(PgError::SchemaMismatch(
                self.errors
                    .iter()
                    .map(|e| format!("  - {e}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ))
        }
    }
}

/// Check every entity in the IR against the introspected schema.
pub fn validate(ir: &SchemaIr, db: &SchemaInfo) -> ValidationReport {
    let mut report = ValidationReport::default();

    for entity in ir.entities() {
        let Some(table) = db.tables.get(&entity.table) else {
            report.errors.push(format!(
                "entity `{}` expects table `{}`, which does not exist{}",
                entity.name,
                entity.table,
                nearest_table_hint(&entity.table, db)
            ));
            continue;
        };
        validate_entity(entity, table, &mut report);
    }

    report
}

fn validate_entity(
    entity: &Entity,
    table: &crate::introspect::TableInfo,
    report: &mut ValidationReport,
) {
    for field in entity.stored_fields() {
        let Some(column_name) = &field.column else {
            continue;
        };
        let Some(scalar) = field.scalar() else {
            continue;
        };

        let Some(column) = table.get(column_name) else {
            report.errors.push(format!(
                "`{}.{}` expects column `{}.{}`, which does not exist",
                entity.name, field.name, entity.table, column_name
            ));
            continue;
        };

        let expected = scalar.postgres_type(field.is_list);
        if column.data_type != expected {
            report.errors.push(format!(
                "`{}.{}` is `{}` (Postgres `{expected}`) but column `{}.{}` is `{}`",
                entity.name,
                field.name,
                scalar.graphql_name(),
                entity.table,
                column_name,
                column.data_type,
            ));
        }

        // Non-fatal: see module docs.
        if !field.nullable && column.is_nullable {
            report.warnings.push(format!(
                "`{}.{}` is non-null in the schema but nullable in the database; \
                 nulls will be returned as errors if present",
                entity.name, field.name
            ));
        }
    }
}

/// If a table is missing, suggest a similarly-named one. The overwhelmingly
/// common cause is pointing `--name` at the wrong project schema, and seeing a
/// near-miss makes that obvious immediately.
fn nearest_table_hint(wanted: &str, db: &SchemaInfo) -> String {
    let candidate = db
        .tables
        .keys()
        .filter(|t| !t.starts_with('_'))
        .min_by_key(|t| edit_distance(wanted, t));

    match candidate {
        Some(c) if edit_distance(wanted, c) <= 3 => format!(" (did you mean `{c}`?)"),
        _ => String::new(),
    }
}

/// Levenshtein distance, two-row variant.
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];

    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::introspect::{ColumnInfo, TableInfo};
    use std::collections::BTreeMap;
    use superquery_query_core::parse_sdl;

    const SDL: &str = r#"
        type Transfer @entity {
          id: ID!
          value: BigInt!
          blockNumber: Int!
        }
    "#;

    fn col(name: &str, ty: &str, nullable: bool) -> (String, ColumnInfo) {
        (
            name.to_string(),
            ColumnInfo {
                name: name.to_string(),
                data_type: ty.to_string(),
                is_nullable: nullable,
            },
        )
    }

    fn db_with(cols: Vec<(String, ColumnInfo)>) -> SchemaInfo {
        let table: TableInfo = cols.into_iter().collect();
        SchemaInfo {
            tables: BTreeMap::from([("transfers".to_string(), table)]),
        }
    }

    fn good_db() -> SchemaInfo {
        db_with(vec![
            col("id", "text", false),
            col("value", "numeric", false),
            col("block_number", "integer", false),
        ])
    }

    #[test]
    fn matching_schema_passes() {
        let r = validate(&parse_sdl(SDL).unwrap(), &good_db());
        assert!(r.is_ok(), "{:?}", r.errors);
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn extra_db_columns_are_ignored() {
        // The node adds bookkeeping columns; they must not fail startup.
        let mut db = good_db();
        db.tables.get_mut("transfers").unwrap().extend([
            col("_id", "uuid", false),
            col("_block_range", "int8range", false),
        ]);
        assert!(validate(&parse_sdl(SDL).unwrap(), &db).is_ok());
    }

    #[test]
    fn missing_table_is_an_error_with_a_hint() {
        let db = db_with(vec![col("id", "text", false)]);
        let db = SchemaInfo {
            tables: BTreeMap::from([("transfer".to_string(), db.tables["transfers"].clone())]),
        };
        let r = validate(&parse_sdl(SDL).unwrap(), &db);
        assert!(!r.is_ok());
        assert!(
            r.errors[0].contains("did you mean `transfer`?"),
            "{:?}",
            r.errors
        );
    }

    #[test]
    fn type_mismatch_is_an_error() {
        // BigInt must be numeric; text would silently mis-sort.
        let db = db_with(vec![
            col("id", "text", false),
            col("value", "text", false),
            col("block_number", "integer", false),
        ]);
        let r = validate(&parse_sdl(SDL).unwrap(), &db);
        assert!(!r.is_ok());
        assert!(r.errors[0].contains("`BigInt`") && r.errors[0].contains("is `text`"));
    }

    #[test]
    fn missing_column_is_an_error() {
        let db = db_with(vec![
            col("id", "text", false),
            col("value", "numeric", false),
        ]);
        let r = validate(&parse_sdl(SDL).unwrap(), &db);
        assert!(r.errors.iter().any(|e| e.contains("block_number")));
    }

    #[test]
    fn nullability_drift_warns_but_does_not_fail() {
        let db = db_with(vec![
            col("id", "text", false),
            col("value", "numeric", true),
            col("block_number", "integer", false),
        ]);
        let r = validate(&parse_sdl(SDL).unwrap(), &db);
        assert!(r.is_ok());
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn all_errors_reported_together() {
        // One restart should surface every problem, not the first.
        let db = db_with(vec![col("id", "integer", false)]);
        let r = validate(&parse_sdl(SDL).unwrap(), &db);
        assert!(r.errors.len() >= 3, "{:?}", r.errors);
    }
}
