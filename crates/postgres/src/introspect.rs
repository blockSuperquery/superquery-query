//! Schema introspection — reads what Postgres actually has.
//!
//! Used only to *validate* the project schema at startup, not to generate the
//! GraphQL API. (Deriving GraphQL from introspection alone is lossy: `numeric`
//! cannot tell you whether the field was `BigInt` or `BigDecimal`, and
//! un-pluralizing `transfers` back to `Transfer` is ambiguous. The SDL is the
//! source of truth; this is the cross-check.)

use std::collections::{BTreeMap, BTreeSet};

use crate::error::PgResult;
use crate::pool::Database;

/// The columns of one table, keyed by column name.
pub type TableInfo = BTreeMap<String, ColumnInfo>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    /// Canonical Postgres type name, e.g. `text`, `numeric`, `int8range`.
    pub data_type: String,
    pub is_nullable: bool,
}

/// Every table in a schema, keyed by table name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchemaInfo {
    pub tables: BTreeMap<String, TableInfo>,
}

impl SchemaInfo {
    pub fn table_names(&self) -> BTreeSet<&str> {
        self.tables.keys().map(String::as_str).collect()
    }
}

/// Introspect one Postgres schema.
pub async fn introspect(db: &Database, schema: &str) -> PgResult<SchemaInfo> {
    // `udt_name` gives the canonical type (`int4`), `data_type` the SQL spelling
    // (`integer`). We normalize to the SQL spelling for readable diagnostics and
    // because that is what the scalar map is written in.
    let rows = db
        .query(
            "SELECT table_name, column_name, data_type, udt_name, is_nullable \
             FROM information_schema.columns \
             WHERE table_schema = $1 \
             ORDER BY table_name, column_name",
            &[&schema],
        )
        .await?;

    let mut tables: BTreeMap<String, TableInfo> = BTreeMap::new();
    for row in rows {
        let table: String = row.get("table_name");
        let name: String = row.get("column_name");
        let data_type: String = row.get("data_type");
        let udt: String = row.get("udt_name");
        let is_nullable: String = row.get("is_nullable");

        tables.entry(table).or_default().insert(
            name.clone(),
            ColumnInfo {
                name,
                data_type: normalize_type(&data_type, &udt),
                is_nullable: is_nullable == "YES",
            },
        );
    }

    Ok(SchemaInfo { tables })
}

/// Normalize a Postgres type to the spelling used in the scalar map.
///
/// `information_schema` reports arrays as `ARRAY` with the element type in
/// `udt_name`, and user-defined ranges as `USER-DEFINED` — both need the udt.
fn normalize_type(data_type: &str, udt: &str) -> String {
    match data_type {
        "ARRAY" | "USER-DEFINED" => udt.trim_start_matches('_').to_string(),
        "timestamp without time zone" => "timestamp".to_string(),
        "timestamp with time zone" => "timestamptz".to_string(),
        "character varying" => "text".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_postgres_type_spellings() {
        assert_eq!(
            normalize_type("timestamp without time zone", "timestamp"),
            "timestamp"
        );
        assert_eq!(normalize_type("character varying", "varchar"), "text");
        assert_eq!(normalize_type("USER-DEFINED", "int8range"), "int8range");
        assert_eq!(normalize_type("integer", "int4"), "integer");
    }
}
