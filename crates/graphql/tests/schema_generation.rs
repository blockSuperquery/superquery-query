//! End-to-end schema generation: SDL in → GraphQL SDL out.
//!
//! No database is touched. `Database::connect` only builds a lazy pool, so the
//! entire generation path — objects, connections, filters, ordering enums,
//! scalars, query fields — is exercised without infrastructure. That keeps this
//! runnable in CI and on a laptop, which is what makes it a useful guard.

use std::sync::Arc;

use superquery_graphql::{build_schema, Limits};
use superquery_postgres::{Database, DbConfig};
use superquery_query_core::parse_sdl;

const PROJECT_SDL: &str = r#"
    type Transfer @entity {
      id: ID!
      from: String!
      to: String!
      value: BigInt!
      blockNumber: Int!
      data: Bytes
      tags: [String!]
      fromAccount: Account!
    }

    type Account @entity {
      id: ID!
      balance: BigInt!
      transfers: [Transfer!]! @derivedFrom(field: "fromAccount")
    }
"#;

fn generated_sdl() -> String {
    let ir = Arc::new(parse_sdl(PROJECT_SDL).expect("project schema parses"));
    let db = Database::connect(&DbConfig::default()).expect("lazy pool");
    let schema = build_schema(ir, db, "app", Limits::default()).expect("schema builds");
    schema.sdl()
}

#[test]
fn generates_query_fields_for_every_entity() {
    let sdl = generated_sdl();
    assert!(sdl.contains("transfer(id: ID!): Transfer"), "{sdl}");
    assert!(sdl.contains("account(id: ID!): Account"), "{sdl}");
    assert!(sdl.contains("transfers("), "{sdl}");
    assert!(sdl.contains("accounts("), "{sdl}");
}

#[test]
fn collection_field_returns_a_connection() {
    let sdl = generated_sdl();
    assert!(sdl.contains("): TransferConnection!"), "{sdl}");
    assert!(sdl.contains("type TransferConnection"), "{sdl}");
    assert!(sdl.contains("type TransferEdge"), "{sdl}");
    assert!(sdl.contains("type PageInfo"), "{sdl}");
}

#[test]
fn big_int_is_a_custom_scalar_not_a_float() {
    let sdl = generated_sdl();
    assert!(sdl.contains("scalar BigInt"), "{sdl}");
    // The precision guarantee, visible in the public schema.
    assert!(sdl.contains("value: BigInt!"), "{sdl}");
    assert!(!sdl.contains("value: Float"), "{sdl}");
}

#[test]
fn nullability_and_lists_survive_generation() {
    let sdl = generated_sdl();
    assert!(
        sdl.contains("data: Bytes\n") || sdl.contains("data: Bytes "),
        "{sdl}"
    );
    assert!(sdl.contains("from: String!"), "{sdl}");
    assert!(sdl.contains("tags: [String!]"), "{sdl}");
}

#[test]
fn relations_are_object_typed_not_id_typed() {
    let sdl = generated_sdl();
    // Forward relation resolves to the entity, not to its raw id column.
    assert!(sdl.contains("fromAccount: Account!"), "{sdl}");
    // Reverse @derivedFrom relation appears on the other side.
    assert!(sdl.contains("transfers: [Transfer!]!"), "{sdl}");
}

#[test]
fn filter_inputs_are_generated_per_entity_and_scalar() {
    let sdl = generated_sdl();
    assert!(sdl.contains("input TransferFilter"), "{sdl}");
    assert!(sdl.contains("input BigIntFilter"), "{sdl}");
    assert!(sdl.contains("input StringFilter"), "{sdl}");
    // Boolean ops are shared; ordering ops only where meaningful.
    assert!(sdl.contains("equalTo: BigInt"), "{sdl}");
    assert!(sdl.contains("greaterThanOrEqualTo: BigInt"), "{sdl}");
}

#[test]
fn filters_compose_with_and_or_not() {
    let sdl = generated_sdl();
    assert!(sdl.contains("and: [TransferFilter!]"), "{sdl}");
    assert!(sdl.contains("or: [TransferFilter!]"), "{sdl}");
    assert!(sdl.contains("not: TransferFilter"), "{sdl}");
}

#[test]
fn order_by_enum_only_exposes_safe_keys() {
    let sdl = generated_sdl();
    assert!(sdl.contains("enum TransfersOrderBy"), "{sdl}");
    assert!(sdl.contains("BLOCK_NUMBER_DESC"), "{sdl}");
    assert!(sdl.contains("VALUE_ASC"), "{sdl}");
    // `data` is nullable → excluded, because a null breaks keyset pagination.
    assert!(!sdl.contains("DATA_ASC"), "{sdl}");
    // `tags` is a list → not orderable.
    assert!(!sdl.contains("TAGS_ASC"), "{sdl}");
}

#[test]
fn pagination_arguments_are_present() {
    let sdl = generated_sdl();
    for arg in ["first: Int", "last: Int", "after: Cursor", "before: Cursor"] {
        assert!(sdl.contains(arg), "missing `{arg}` in:\n{sdl}");
    }
}

#[test]
fn connections_expose_total_count() {
    let sdl = generated_sdl();
    assert!(sdl.contains("totalCount: Int!"), "{sdl}");
}

#[test]
fn meta_field_is_exposed() {
    let sdl = generated_sdl();
    assert!(sdl.contains("_meta: _Meta!"), "{sdl}");
    assert!(sdl.contains("indexedHeight"), "{sdl}");
}

#[test]
fn no_mutations_are_generated() {
    // The node owns every write; this API must not offer any.
    let sdl = generated_sdl();
    assert!(
        !sdl.contains("type Mutation"),
        "query service exposes mutations:\n{sdl}"
    );
}

#[test]
fn schema_with_no_entities_still_builds() {
    // An empty project should produce a valid (if boring) schema rather than
    // failing at startup.
    let ir = Arc::new(parse_sdl("").expect("empty schema parses"));
    let db = Database::connect(&DbConfig::default()).unwrap();
    let schema = build_schema(ir, db, "app", Limits::default()).expect("builds");
    assert!(schema.sdl().contains("_meta"));
}
