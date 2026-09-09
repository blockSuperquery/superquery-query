//! Canonical scalar mapping — GraphQL type ⇄ Postgres column type.
//!
//! This table is the exact inverse of the node's `ddl.rs` type map. The node
//! writes columns using the left-to-middle direction; we read them using
//! middle-to-right. Any disagreement shows up as a decode error at query time,
//! so `validate` (in the postgres crate) checks it at startup instead.
//!
//! | GraphQL      | Postgres           | Wire representation           |
//! |--------------|--------------------|-------------------------------|
//! | `ID`         | `text`             | String                        |
//! | `String`     | `text`             | String                        |
//! | `Int`        | `integer`          | i32                           |
//! | `Float`      | `double precision` | f64                           |
//! | `Boolean`    | `boolean`          | bool                          |
//! | `BigInt`     | `numeric`          | **String** (never a JSON num) |
//! | `BigDecimal` | `numeric`          | **String**                    |
//! | `Bytes`      | `bytea`            | `0x`-prefixed hex string      |
//! | `Date`       | `timestamp`        | RFC3339 string                |
//! | `Json`       | `jsonb`            | arbitrary JSON                |
//! | any list     | `jsonb`            | JSON array                    |
//!
//! # Why BigInt is a string
//!
//! A 256-bit EVM value does not survive `f64`. JSON numbers are `f64` in every
//! mainstream client, so serializing `BigInt` as a JSON number silently corrupts
//! any balance above 2^53. It goes over the wire as a string, always.

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// A scalar type as written in a project's `schema.graphql`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScalarType {
    Id,
    String,
    Int,
    Float,
    Boolean,
    BigInt,
    BigDecimal,
    Bytes,
    Date,
    Json,
}

impl ScalarType {
    /// Parse a base type name from SDL. `None` for names that are not scalars
    /// (those are entity references or enums, resolved by the schema pass).
    pub fn from_sdl_name(name: &str) -> Option<Self> {
        Some(match name {
            "ID" => Self::Id,
            "String" => Self::String,
            "Int" => Self::Int,
            "Float" => Self::Float,
            "Boolean" => Self::Boolean,
            "BigInt" => Self::BigInt,
            "BigDecimal" => Self::BigDecimal,
            "Bytes" => Self::Bytes,
            "Date" => Self::Date,
            "Json" => Self::Json,
            _ => return None,
        })
    }

    /// The name this scalar is exposed under in the generated GraphQL schema.
    pub fn graphql_name(self) -> &'static str {
        match self {
            Self::Id => "ID",
            Self::String => "String",
            Self::Int => "Int",
            Self::Float => "Float",
            Self::Boolean => "Boolean",
            Self::BigInt => "BigInt",
            Self::BigDecimal => "BigDecimal",
            Self::Bytes => "Bytes",
            Self::Date => "Date",
            Self::Json => "JSON",
        }
    }

    /// The Postgres type the node's DDL creates for this scalar.
    ///
    /// Used by startup validation to detect a node/query version mismatch
    /// before any query runs.
    pub fn postgres_type(self, is_list: bool) -> &'static str {
        if is_list {
            return "jsonb";
        }
        match self {
            Self::Id | Self::String => "text",
            Self::Int => "integer",
            Self::Float => "double precision",
            Self::Boolean => "boolean",
            Self::BigInt | Self::BigDecimal => "numeric",
            Self::Bytes => "bytea",
            Self::Date => "timestamp",
            Self::Json => "jsonb",
        }
    }

    /// Whether this scalar is ordered, i.e. usable in `orderBy` and as a cursor
    /// component. `Json` is not — Postgres `jsonb` ordering is not meaningful
    /// to a user and would produce stable-but-surprising pagination.
    pub fn is_orderable(self) -> bool {
        !matches!(self, Self::Json)
    }

    /// Whether range comparisons (`greaterThan`, …) apply.
    pub fn supports_ordering_filters(self) -> bool {
        matches!(
            self,
            Self::Int | Self::Float | Self::BigInt | Self::BigDecimal | Self::Date | Self::String
        )
    }

    /// The SQL expression that projects this column onto the wire.
    ///
    /// Everything is read back as `text` and converted in Rust. That keeps
    /// numeric precision intact (Postgres renders `numeric` losslessly to text,
    /// whereas binary-decoding it through `f64` would not) and matches how the
    /// node canonicalizes rows for its own parity dumps.
    pub fn select_expr(self, quoted_col: &str) -> String {
        match self {
            // bytea → hex without the leading `\x`; the GraphQL layer re-adds `0x`.
            Self::Bytes => format!("encode({quoted_col}, 'hex')"),
            _ => format!("{quoted_col}::text"),
        }
    }

    /// The cast applied to a bound `$n` parameter so Postgres compares it
    /// against this column's type. Parameters are always bound as text, so the
    /// cast is `::text::<target>` — binding text and letting Postgres infer the
    /// target type from context is rejected for non-text columns.
    pub fn param_cast(self, placeholder: &str, is_list: bool) -> String {
        if is_list {
            return format!("{placeholder}::text::jsonb");
        }
        match self {
            Self::Id | Self::String => placeholder.to_string(),
            Self::Int => format!("{placeholder}::text::integer"),
            Self::Float => format!("{placeholder}::text::double precision"),
            Self::Boolean => format!("{placeholder}::text::boolean"),
            Self::BigInt | Self::BigDecimal => format!("{placeholder}::text::numeric"),
            Self::Date => format!("{placeholder}::text::timestamp"),
            Self::Bytes => format!("decode({placeholder}::text, 'hex')"),
            Self::Json => format!("{placeholder}::text::jsonb"),
        }
    }

    /// Normalize a client-supplied value into the text form bound to a parameter.
    ///
    /// `Bytes` accepts `0x`-prefixed or bare hex and is stored bare, matching the
    /// node's encode step.
    pub fn encode_param(self, value: &serde_json::Value) -> Result<Option<String>, CoreError> {
        use serde_json::Value;
        Ok(match value {
            Value::Null => None,
            Value::String(s) => Some(match self {
                Self::Bytes => s.strip_prefix("0x").unwrap_or(s).to_string(),
                _ => s.clone(),
            }),
            Value::Bool(b) => Some(b.to_string()),
            Value::Number(n) => Some(n.to_string()),
            v @ (Value::Array(_) | Value::Object(_)) => Some(v.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PARITY: mirrors the node's `ddl.rs` `column_type` mapping. If the node
    /// changes a column type, this test is where it should first be noticed.
    #[test]
    fn postgres_types_match_node_ddl() {
        assert_eq!(ScalarType::Id.postgres_type(false), "text");
        assert_eq!(ScalarType::String.postgres_type(false), "text");
        assert_eq!(ScalarType::Int.postgres_type(false), "integer");
        assert_eq!(ScalarType::BigInt.postgres_type(false), "numeric");
        assert_eq!(ScalarType::Float.postgres_type(false), "double precision");
        assert_eq!(ScalarType::Boolean.postgres_type(false), "boolean");
        assert_eq!(ScalarType::Bytes.postgres_type(false), "bytea");
        assert_eq!(ScalarType::Date.postgres_type(false), "timestamp");
        assert_eq!(ScalarType::Json.postgres_type(false), "jsonb");
    }

    #[test]
    fn lists_are_always_jsonb() {
        for s in [
            ScalarType::String,
            ScalarType::BigInt,
            ScalarType::Int,
            ScalarType::Bytes,
        ] {
            assert_eq!(s.postgres_type(true), "jsonb", "{s:?} as list");
        }
    }

    #[test]
    fn bytes_projects_as_hex_not_text() {
        // `bytea::text` renders as `\x616263`, which is not what we want on the
        // wire; encode() gives bare hex that the GraphQL layer prefixes with 0x.
        assert_eq!(
            ScalarType::Bytes.select_expr("\"data\""),
            "encode(\"data\", 'hex')"
        );
        assert_eq!(ScalarType::BigInt.select_expr("\"v\""), "\"v\"::text");
    }

    #[test]
    fn bytes_param_accepts_prefixed_hex() {
        let v = serde_json::json!("0xdeadbeef");
        assert_eq!(
            ScalarType::Bytes.encode_param(&v).unwrap(),
            Some("deadbeef".to_string())
        );
    }

    #[test]
    fn json_is_not_orderable() {
        assert!(!ScalarType::Json.is_orderable());
        assert!(ScalarType::BigInt.is_orderable());
    }
}
