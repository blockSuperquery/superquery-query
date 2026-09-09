//! GraphQL-side naming: entity → query fields and generated type names.
//!
//! Distinct from `query-core::naming`, which maps entities to *Postgres*
//! identifiers. These names are what a dApp developer types, and they follow
//! SubQuery's conventions so existing queries port across with minimal edits.
//!
//! | Entity `Transfer` | Name                  |
//! |-------------------|-----------------------|
//! | by-id field       | `transfer`            |
//! | collection field  | `transfers`           |
//! | object type       | `Transfer`            |
//! | connection type   | `TransferConnection`  |
//! | edge type         | `TransferEdge`        |
//! | filter input      | `TransferFilter`      |
//! | ordering enum     | `TransfersOrderBy`    |

use superquery_query_core::naming::pluralize;

/// Lower the first character: `Transfer` → `transfer`.
fn camel(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The `transfer(id: ID!)` field name.
pub fn by_id_field(entity: &str) -> String {
    camel(entity)
}

/// The `transfers(...)` field name.
pub fn collection_field(entity: &str) -> String {
    camel(&pluralize(entity))
}

pub fn connection_type(entity: &str) -> String {
    format!("{entity}Connection")
}

pub fn edge_type(entity: &str) -> String {
    format!("{entity}Edge")
}

pub fn filter_input(entity: &str) -> String {
    format!("{entity}Filter")
}

pub fn order_by_enum(entity: &str) -> String {
    format!("{}OrderBy", pluralize(entity))
}

/// The per-scalar filter input, e.g. `BigIntFilter`.
pub fn scalar_filter_input(scalar_name: &str) -> String {
    format!("{scalar_name}Filter")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_names_follow_subquery_convention() {
        assert_eq!(by_id_field("Transfer"), "transfer");
        assert_eq!(collection_field("Transfer"), "transfers");
        assert_eq!(by_id_field("MyEntity"), "myEntity");
        assert_eq!(collection_field("MyEntity"), "myEntities");
    }

    #[test]
    fn generated_type_names() {
        assert_eq!(connection_type("Transfer"), "TransferConnection");
        assert_eq!(edge_type("Transfer"), "TransferEdge");
        assert_eq!(filter_input("Transfer"), "TransferFilter");
        assert_eq!(order_by_enum("Transfer"), "TransfersOrderBy");
    }

    #[test]
    fn irregular_plurals_carry_through() {
        assert_eq!(collection_field("Entity"), "entities");
    }
}
