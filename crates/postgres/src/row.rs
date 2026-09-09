//! Row decoding: Postgres text output → JSON values for the GraphQL layer.
//!
//! Every column is read back as `text` (see `sql::select_list`), so decoding is
//! a pure string→JSON step with no driver type-inference in the middle. That is
//! what keeps `numeric` exact: Postgres renders it losslessly to text, and we
//! hand it to GraphQL as a string.

use serde_json::Value;
use superquery_query_core::ScalarType;

use crate::error::PgResult;
use crate::sql::ProjectedField;

/// One decoded row: GraphQL field name → JSON value.
pub type EntityRow = serde_json::Map<String, Value>;

/// Decode a row using the projection that produced it.
///
/// Columns are read positionally, matching the order of `fields`, which is the
/// order `select_list` emitted them in.
pub fn decode_row(row: &tokio_postgres::Row, fields: &[ProjectedField]) -> PgResult<EntityRow> {
    let mut out = EntityRow::new();
    for (i, field) in fields.iter().enumerate() {
        let raw: Option<String> = row.get(i);
        out.insert(field.name.clone(), decode_value(raw, field)?);
    }
    Ok(out)
}

/// Convert one text column into its wire representation.
fn decode_value(raw: Option<String>, field: &ProjectedField) -> PgResult<Value> {
    let Some(text) = raw else {
        return Ok(Value::Null);
    };

    // Lists are stored as jsonb and rendered as a JSON array by ::text.
    if field.is_list {
        return Ok(serde_json::from_str(&text).unwrap_or(Value::Null));
    }

    Ok(match field.scalar {
        // Exact integers small enough for i32 go over as JSON numbers.
        ScalarType::Int => text
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or(Value::String(text)),
        ScalarType::Float => text
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        ScalarType::Boolean => Value::Bool(text == "t" || text == "true"),

        // The precision-critical case: a 256-bit value has no exact f64, and
        // JSON numbers are f64 everywhere that matters. String, always.
        ScalarType::BigInt | ScalarType::BigDecimal => Value::String(text),

        // encode() gave bare hex; the ecosystem expects an 0x prefix.
        ScalarType::Bytes => Value::String(format!("0x{text}")),

        ScalarType::Json => serde_json::from_str(&text).unwrap_or(Value::Null),
        ScalarType::Id | ScalarType::String | ScalarType::Date => Value::String(text),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, scalar: ScalarType, is_list: bool) -> ProjectedField {
        ProjectedField {
            name: name.to_string(),
            column: name.to_string(),
            scalar,
            is_list,
        }
    }

    #[test]
    fn bigint_stays_a_string_at_full_precision() {
        // 2^256 - 1: would become 1.157...e77 as an f64.
        let huge = "115792089237316195423570985008687907853269984665640564039457584007913129639935";
        let v = decode_value(
            Some(huge.to_string()),
            &field("v", ScalarType::BigInt, false),
        )
        .unwrap();
        assert_eq!(v, Value::String(huge.to_string()));
    }

    #[test]
    fn bytes_gets_an_0x_prefix() {
        let v = decode_value(
            Some("deadbeef".into()),
            &field("d", ScalarType::Bytes, false),
        )
        .unwrap();
        assert_eq!(v, Value::String("0xdeadbeef".into()));
    }

    #[test]
    fn postgres_boolean_text_is_understood() {
        // Postgres renders booleans as `t`/`f`, not `true`/`false`.
        let f = field("b", ScalarType::Boolean, false);
        assert_eq!(
            decode_value(Some("t".into()), &f).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            decode_value(Some("f".into()), &f).unwrap(),
            Value::Bool(false)
        );
    }

    #[test]
    fn null_columns_decode_to_null() {
        assert_eq!(
            decode_value(None, &field("x", ScalarType::String, false)).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn lists_parse_back_from_jsonb() {
        let v = decode_value(
            Some(r#"["a", "b"]"#.into()),
            &field("tags", ScalarType::String, true),
        )
        .unwrap();
        assert_eq!(v, serde_json::json!(["a", "b"]));
    }

    #[test]
    fn int_becomes_a_json_number() {
        let v = decode_value(Some("42".into()), &field("n", ScalarType::Int, false)).unwrap();
        assert_eq!(v, Value::from(42));
    }
}
