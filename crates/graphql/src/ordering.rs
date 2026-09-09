//! `orderBy` enums, and the parser back to an [`Ordering`].
//!
//! Clients never send column names — only enum values we generated, which
//! GraphQL validates before the resolver runs. An arbitrary string cannot reach
//! the SQL builder even in principle.
//!
//! # Why nullable fields are excluded
//!
//! Cursor pagination compares the ordering keys of the last row seen. With a
//! NULL in that tuple the comparison is not total — `NULL > x` is neither true
//! nor false — so pages can silently repeat or skip rows. Rather than emit an
//! `orderBy` value that breaks on page two, nullable fields are simply not
//! offered. Lifting this needs explicit `NULLS FIRST/LAST` handling in the
//! keyset predicate (tracked with Milestone 7).

use async_graphql::dynamic::Enum;
use superquery_query_core::filter::Field as FilterField;
use superquery_query_core::schema::Entity;
use superquery_query_core::sort::{SortDirection, SortKey};
use superquery_query_core::{CoreError, CoreResult, Ordering};

use crate::naming;

/// Fields that may appear in `orderBy`: stored, orderable, non-null, not a list.
fn orderable_fields(entity: &Entity) -> impl Iterator<Item = &superquery_query_core::SchemaField> {
    entity
        .stored_fields()
        .filter(|f| !f.is_list && !f.nullable && f.scalar().is_some_and(|s| s.is_orderable()))
}

/// Build `<Plural>OrderBy` for an entity.
pub fn order_by_enum(entity: &Entity) -> Enum {
    let mut e = Enum::new(naming::order_by_enum(&entity.name)).description(format!(
        "Ordering options for `{}` collections. Results always carry an implicit \
         `id` tiebreaker, so pagination is deterministic.",
        entity.name
    ));

    for field in orderable_fields(entity) {
        let Some(column) = &field.column else {
            continue;
        };
        for direction in [SortDirection::Asc, SortDirection::Desc] {
            let key = SortKey::new(FilterField::resolved(&field.name, column), direction);
            e = e.item(key.enum_value());
        }
    }

    e
}

/// Whether the entity has anything to order by beyond the implicit `id`.
pub fn has_order_by(entity: &Entity) -> bool {
    orderable_fields(entity).next().is_some()
}

/// Resolve enum values back into an [`Ordering`], appending the `id` tiebreaker.
pub fn parse_ordering(entity: &Entity, values: &[String]) -> CoreResult<Ordering> {
    let id_field = entity
        .field("id")
        .and_then(|f| f.column.clone())
        .map(|c| FilterField::resolved("id", c))
        .ok_or_else(|| CoreError::MissingId(entity.name.clone()))?;

    let mut keys = Vec::with_capacity(values.len());
    for value in values {
        keys.push(parse_one(entity, value)?);
    }

    Ok(Ordering::new(keys, id_field))
}

/// Match an enum value against the ones we generated.
///
/// Done by regenerating and comparing rather than by string-splitting: splitting
/// `BLOCK_NUMBER_DESC` on `_` is ambiguous for a column literally named
/// `block_number_desc`, and this way the accepted set is exactly the advertised
/// set by construction.
fn parse_one(entity: &Entity, value: &str) -> CoreResult<SortKey> {
    for field in orderable_fields(entity) {
        let Some(column) = &field.column else {
            continue;
        };
        for direction in [SortDirection::Asc, SortDirection::Desc] {
            let key = SortKey::new(FilterField::resolved(&field.name, column), direction);
            if key.enum_value() == value {
                return Ok(key);
            }
        }
    }
    Err(CoreError::InvalidValue {
        field: "orderBy".to_string(),
        reason: format!("`{value}` is not a valid ordering for `{}`", entity.name),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use superquery_query_core::parse_sdl;

    const SDL: &str = r#"
        type Transfer @entity {
          id: ID!
          from: String!
          value: BigInt!
          blockNumber: Int!
          memo: String
          tags: [String!]
          meta: Json!
        }
    "#;

    fn entity() -> Entity {
        parse_sdl(SDL).unwrap().entity("Transfer").unwrap().clone()
    }

    #[test]
    fn offers_non_null_scalar_fields_both_directions() {
        let e = entity();
        let names: Vec<_> = orderable_fields(&e).map(|f| f.name.clone()).collect();
        assert_eq!(names, ["id", "from", "value", "blockNumber"]);
    }

    #[test]
    fn excludes_nullable_list_and_json_fields() {
        let e = entity();
        let names: Vec<_> = orderable_fields(&e).map(|f| f.name.clone()).collect();
        assert!(
            !names.contains(&"memo".to_string()),
            "nullable field offered"
        );
        assert!(!names.contains(&"tags".to_string()), "list field offered");
        assert!(!names.contains(&"meta".to_string()), "json field offered");
    }

    #[test]
    fn parses_generated_values() {
        let e = entity();
        let o = parse_ordering(&e, &["BLOCK_NUMBER_DESC".to_string()]).unwrap();
        assert_eq!(o.to_sql(), r#""block_number" DESC, "id" DESC"#);
    }

    #[test]
    fn multiple_keys_keep_their_order() {
        let e = entity();
        let o = parse_ordering(&e, &["VALUE_DESC".into(), "FROM_ASC".into()]).unwrap();
        assert_eq!(o.to_sql(), r#""value" DESC, "from" ASC, "id" ASC"#);
    }

    #[test]
    fn empty_order_by_defaults_to_id() {
        let e = entity();
        assert_eq!(parse_ordering(&e, &[]).unwrap().to_sql(), r#""id" ASC"#);
    }

    #[test]
    fn unknown_ordering_is_rejected() {
        let e = entity();
        assert!(parse_ordering(&e, &["MEMO_ASC".to_string()]).is_err());
        assert!(parse_ordering(&e, &["'; DROP TABLE t; --".to_string()]).is_err());
    }

    #[test]
    fn advertised_values_all_parse() {
        // Builder/parser parity: everything in the enum must resolve.
        let e = entity();
        for field in orderable_fields(&e) {
            let column = field.column.clone().unwrap();
            for dir in [SortDirection::Asc, SortDirection::Desc] {
                let v = SortKey::new(FilterField::resolved(&field.name, &column), dir).enum_value();
                assert!(parse_one(&e, &v).is_ok(), "advertised but unparseable: {v}");
            }
        }
    }
}
