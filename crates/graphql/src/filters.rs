//! Filter input types, and the parser that turns them into [`FilterExpr`].
//!
//! The builder and the parser live in the same module on purpose: they are two
//! views of one grammar, and splitting them is how a schema ends up advertising
//! an operator the parser silently ignores.
//!
//! Shape:
//!
//! ```graphql
//! transfers(filter: {
//!   value: { greaterThan: "1000" }
//!   from:  { equalTo: "0xabc" }
//!   or: [ { blockNumber: { lessThan: 100 } } ]
//! })
//! ```
//!
//! Top-level entries are ANDed, matching SubQuery.

use async_graphql::dynamic::{InputObject, InputValue, TypeRef};
use serde_json::Value;
use superquery_query_core::filter::{CmpOp, Field as FilterField};
use superquery_query_core::schema::{Entity, FieldKind};
use superquery_query_core::{CoreError, CoreResult, FilterExpr, ScalarType};

use crate::naming;

/// Scalars that get a generated `<Scalar>Filter` input object.
const FILTERABLE: &[ScalarType] = &[
    ScalarType::Id,
    ScalarType::String,
    ScalarType::Int,
    ScalarType::Float,
    ScalarType::Boolean,
    ScalarType::BigInt,
    ScalarType::BigDecimal,
    ScalarType::Bytes,
    ScalarType::Date,
];

/// Build the shared per-scalar filter inputs (`StringFilter`, `BigIntFilter`, …).
///
/// One per scalar rather than one per field keeps the schema small: an entity
/// with 20 `String` fields reuses a single `StringFilter`.
pub fn scalar_filter_inputs() -> Vec<InputObject> {
    FILTERABLE
        .iter()
        .map(|scalar| {
            let gql = scalar.graphql_name();
            let mut input = InputObject::new(naming::scalar_filter_input(gql))
                .description(format!("Filter conditions for `{gql}` fields."));

            for op in CmpOp::EQUALITY {
                input = input.field(InputValue::new(op.graphql_name(), TypeRef::named(gql)));
            }
            if scalar.supports_ordering_filters() {
                for op in CmpOp::ORDERING {
                    input = input.field(InputValue::new(op.graphql_name(), TypeRef::named(gql)));
                }
            }
            input = input.field(InputValue::new("in", TypeRef::named_nn_list(gql)));
            input = input.field(
                InputValue::new("isNull", TypeRef::named(TypeRef::BOOLEAN))
                    .description("Match rows where this field is (or is not) null."),
            );
            input
        })
        .collect()
}

/// Build `<Entity>Filter`.
pub fn entity_filter_input(entity: &Entity) -> InputObject {
    let type_name = naming::filter_input(&entity.name);
    let mut input = InputObject::new(&type_name).description(format!(
        "Filter conditions for `{}`. Top-level fields are combined with AND.",
        entity.name
    ));

    for field in entity.stored_fields() {
        // Relations filter on the stored foreign-key id.
        let Some(scalar) = field.scalar() else {
            continue;
        };
        // Lists live in jsonb; containment operators are a later milestone.
        if field.is_list || !FILTERABLE.contains(&scalar) {
            continue;
        }
        input = input.field(InputValue::new(
            &field.name,
            TypeRef::named(naming::scalar_filter_input(scalar.graphql_name())),
        ));
    }

    input
        .field(InputValue::new("and", TypeRef::named_nn_list(&type_name)))
        .field(InputValue::new("or", TypeRef::named_nn_list(&type_name)))
        .field(InputValue::new("not", TypeRef::named(&type_name)))
}

/// Parse a client filter argument into the IR.
///
/// Every field name is resolved against `entity`; an unknown one is an error,
/// which is what keeps client text out of the SQL builder.
pub fn parse_filter(entity: &Entity, value: &Value) -> CoreResult<Option<FilterExpr>> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };

    let mut parts = Vec::new();

    for (key, sub) in object {
        if sub.is_null() {
            continue;
        }
        match key.as_str() {
            "and" => parts.push(FilterExpr::And(parse_list(entity, sub)?)),
            "or" => parts.push(FilterExpr::Or(parse_list(entity, sub)?)),
            "not" => {
                if let Some(inner) = parse_filter(entity, sub)? {
                    parts.push(FilterExpr::Not(Box::new(inner)));
                }
            }
            field_name => parts.extend(parse_field_conditions(entity, field_name, sub)?),
        }
    }

    Ok(FilterExpr::all(parts))
}

fn parse_list(entity: &Entity, value: &Value) -> CoreResult<Vec<FilterExpr>> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|i| parse_filter(entity, i).transpose())
                .collect::<CoreResult<Vec<_>>>()
        })
        .unwrap_or_else(|| Ok(Vec::new()))
}

/// Parse `{ equalTo: …, greaterThan: … }` for one field. Multiple operators on
/// the same field are ANDed, which is what makes range queries readable:
/// `blockNumber: { greaterThan: 100, lessThan: 200 }`.
fn parse_field_conditions(
    entity: &Entity,
    field_name: &str,
    conditions: &Value,
) -> CoreResult<Vec<FilterExpr>> {
    // The resolution step that makes the rest safe.
    let field = entity
        .field(field_name)
        .ok_or_else(|| CoreError::UnknownField {
            entity: entity.name.clone(),
            field: field_name.to_string(),
        })?;

    if matches!(field.kind, FieldKind::Derived { .. }) {
        return Err(CoreError::UnknownField {
            entity: entity.name.clone(),
            field: format!("{field_name} (derived relations cannot be filtered directly)"),
        });
    }

    let column = field
        .column
        .clone()
        .ok_or_else(|| CoreError::UnknownField {
            entity: entity.name.clone(),
            field: field_name.to_string(),
        })?;
    let reference = FilterField::resolved(field_name, column);

    let Some(object) = conditions.as_object() else {
        return Ok(Vec::new());
    };

    let mut out = Vec::new();
    for (op_name, value) in object {
        if value.is_null() && op_name != "isNull" {
            continue;
        }
        let expr = match op_name.as_str() {
            "in" => FilterExpr::In {
                field: reference.clone(),
                values: value.as_array().cloned().unwrap_or_default(),
            },
            "isNull" => FilterExpr::IsNull {
                field: reference.clone(),
                negated: value.as_bool() == Some(false),
            },
            other => {
                let op = cmp_op_from_name(other).ok_or_else(|| CoreError::InvalidValue {
                    field: field_name.to_string(),
                    reason: format!("unknown filter operator `{other}`"),
                })?;
                FilterExpr::Compare {
                    field: reference.clone(),
                    op,
                    value: value.clone(),
                }
            }
        };
        out.push(expr);
    }
    Ok(out)
}

/// Reverse of [`CmpOp::graphql_name`]. Exhaustive over the enum so a new
/// operator cannot be added to one direction only.
fn cmp_op_from_name(name: &str) -> Option<CmpOp> {
    [
        CmpOp::Eq,
        CmpOp::Ne,
        CmpOp::Gt,
        CmpOp::Gte,
        CmpOp::Lt,
        CmpOp::Lte,
    ]
    .into_iter()
    .find(|op| op.graphql_name() == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use superquery_query_core::parse_sdl;

    const SDL: &str = r#"
        type Transfer @entity {
          id: ID!
          from: String!
          value: BigInt!
          blockNumber: Int!
          acct: Account!
        }
        type Account @entity {
          id: ID!
          transfers: [Transfer!]! @derivedFrom(field: "acct")
        }
    "#;

    fn entity(name: &str) -> Entity {
        parse_sdl(SDL).unwrap().entity(name).unwrap().clone()
    }

    #[test]
    fn parses_simple_equality() {
        let e = entity("Transfer");
        let f = parse_filter(&e, &json!({"from": {"equalTo": "0xabc"}}))
            .unwrap()
            .unwrap();
        assert_eq!(
            f,
            FilterExpr::Compare {
                field: FilterField::resolved("from", "from"),
                op: CmpOp::Eq,
                value: json!("0xabc"),
            }
        );
    }

    #[test]
    fn multiple_operators_on_one_field_are_anded() {
        let e = entity("Transfer");
        let f = parse_filter(
            &e,
            &json!({"blockNumber": {"greaterThan": 100, "lessThan": 200}}),
        )
        .unwrap()
        .unwrap();
        match f {
            FilterExpr::And(parts) => assert_eq!(parts.len(), 2),
            other => panic!("expected AND, got {other:?}"),
        }
    }

    #[test]
    fn nested_and_or_parse() {
        let e = entity("Transfer");
        let f = parse_filter(
            &e,
            &json!({
                "from": {"equalTo": "0xa"},
                "or": [{"blockNumber": {"lessThan": 10}}, {"value": {"equalTo": "5"}}]
            }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(f.depth(), 3); // And → Or → Compare
    }

    #[test]
    fn foreign_key_is_filterable_by_id() {
        let e = entity("Transfer");
        let f = parse_filter(&e, &json!({"acct": {"equalTo": "acct-1"}}))
            .unwrap()
            .unwrap();
        match f {
            FilterExpr::Compare { field, .. } => assert_eq!(field.column(), "acct"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn unknown_field_is_rejected() {
        let e = entity("Transfer");
        let err = parse_filter(&e, &json!({"nope": {"equalTo": 1}})).unwrap_err();
        assert!(matches!(err, CoreError::UnknownField { .. }));
    }

    #[test]
    fn injection_shaped_field_name_is_rejected_not_passed_through() {
        let e = entity("Transfer");
        let evil = r#"id" = '' OR 1=1 --"#;
        assert!(parse_filter(&e, &json!({ evil: {"equalTo": 1} })).is_err());
    }

    #[test]
    fn unknown_operator_is_rejected() {
        let e = entity("Transfer");
        let err = parse_filter(&e, &json!({"from": {"likeSortOf": "x"}})).unwrap_err();
        assert!(matches!(err, CoreError::InvalidValue { .. }));
    }

    #[test]
    fn derived_relation_cannot_be_filtered() {
        let e = entity("Account");
        assert!(parse_filter(&e, &json!({"transfers": {"equalTo": "x"}})).is_err());
    }

    #[test]
    fn is_null_both_directions() {
        let e = entity("Transfer");
        let t = parse_filter(&e, &json!({"from": {"isNull": true}}))
            .unwrap()
            .unwrap();
        assert_eq!(
            t,
            FilterExpr::IsNull {
                field: FilterField::resolved("from", "from"),
                negated: false
            }
        );

        let f = parse_filter(&e, &json!({"from": {"isNull": false}}))
            .unwrap()
            .unwrap();
        assert_eq!(
            f,
            FilterExpr::IsNull {
                field: FilterField::resolved("from", "from"),
                negated: true
            }
        );
    }

    #[test]
    fn empty_filter_is_none() {
        let e = entity("Transfer");
        assert!(parse_filter(&e, &json!({})).unwrap().is_none());
    }

    #[test]
    fn every_operator_in_the_schema_is_parseable() {
        // Guards builder/parser drift: anything the schema advertises must parse.
        for op in CmpOp::EQUALITY.iter().chain(CmpOp::ORDERING.iter()) {
            assert_eq!(cmp_op_from_name(op.graphql_name()), Some(*op));
        }
    }
}
