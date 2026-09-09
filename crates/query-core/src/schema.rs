//! Schema IR — the canonical in-memory model of a project's entities.
//!
//! # Where this comes from
//!
//! The implementation guide's Milestone 0 assumes `superquery-sdk` publishes a
//! canonical `SchemaIr` crate. It does not — the SDK is a TypeScript/Nuxt app.
//! So v0 parses the project's `schema.graphql` directly, using the same rules as
//! the node's `subql-store::schema::parse_entities`, and then validates the
//! result against the live Postgres schema (see `superquery-postgres::validate`).
//!
//! That gives us the same guarantee the guide wanted — a mismatch between DB and
//! project schema fails loudly at startup — without inventing a cross-repo
//! artifact that nothing produces yet.
//!
//! When the SDK does emit a canonical IR, `SchemaIr` becomes its deserialization
//! target and `parse_sdl` becomes one of two constructors. Nothing downstream of
//! this module needs to change.

use std::collections::BTreeMap;

use graphql_parser::schema::{Definition, Type, TypeDefinition};
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};
use crate::naming::{field_to_column_name, model_to_table_name};
use crate::scalar::ScalarType;

/// The full set of entities a project exposes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SchemaIr {
    /// Entities keyed by their schema name (`Transfer`), in declaration order
    /// via `order` so generated GraphQL is deterministic.
    entities: BTreeMap<String, Entity>,
    order: Vec<String>,
}

impl SchemaIr {
    /// Entities in declaration order.
    pub fn entities(&self) -> impl Iterator<Item = &Entity> {
        self.order.iter().filter_map(|n| self.entities.get(n))
    }

    pub fn entity(&self, name: &str) -> Option<&Entity> {
        self.entities.get(name)
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

/// One `type X @entity` and everything the query layer needs to serve it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    /// Name as written in the schema, e.g. `Transfer`.
    pub name: String,
    /// The Postgres table it lives in, e.g. `transfers`. Derived, never guessed.
    pub table: String,
    pub fields: Vec<Field>,
}

impl Entity {
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// Fields backed by a real column (i.e. excluding `@derivedFrom` reverse
    /// relations, which are computed by a join at query time).
    pub fn stored_fields(&self) -> impl Iterator<Item = &Field> {
        self.fields.iter().filter(|f| f.column.is_some())
    }
}

/// A single field on an entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Field {
    /// Name as written, e.g. `blockNumber`.
    pub name: String,
    /// The column backing it, e.g. `block_number`. `None` for derived relations.
    pub column: Option<String>,
    pub kind: FieldKind,
    pub is_list: bool,
    pub nullable: bool,
}

impl Field {
    /// The column name, quoted for interpolation into SQL.
    ///
    /// Safe because column names are derived from the parsed schema, never from
    /// client input — clients select fields by their *GraphQL* name, which is
    /// resolved through the IR before any SQL is built.
    pub fn quoted_column(&self) -> Option<String> {
        self.column.as_ref().map(|c| format!("\"{c}\""))
    }

    pub fn scalar(&self) -> Option<ScalarType> {
        match self.kind {
            FieldKind::Scalar(s) => Some(s),
            // A foreign key column is stored as the referenced entity's id: text.
            FieldKind::Relation { .. } => Some(ScalarType::Id),
            FieldKind::Derived { .. } => None,
        }
    }
}

/// What a field actually is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldKind {
    /// A plain scalar column.
    Scalar(ScalarType),
    /// A foreign key: this entity stores the target's id in its own column.
    Relation { target: String },
    /// A reverse relation computed from the other side's FK. No column here.
    Derived {
        target: String,
        target_field: String,
    },
}

/// Parse a project's `schema.graphql` into the IR.
///
/// Mirrors the node's `parse_entities`: only object types carrying `@entity` are
/// considered. Unlike the node's current slice, this also resolves relations, so
/// entity-typed fields do not fall through to "unsupported type".
pub fn parse_sdl(sdl: &str) -> CoreResult<SchemaIr> {
    // `graphql-parser` treats an empty document as a syntax error
    // ("Unexpected end of input"), which is a confusing thing to show someone
    // whose schema file is simply empty. An empty schema is legal-but-useless;
    // the server warns about a zero-entity project at startup, where the
    // message can actually be helpful.
    if sdl.trim().is_empty() {
        return Ok(SchemaIr::default());
    }

    let doc = graphql_parser::schema::parse_schema::<String>(sdl)
        .map_err(|e| CoreError::SchemaParse(e.to_string()))?;

    // Pass 1: collect the names of every @entity so relation targets resolve.
    let mut entity_names = Vec::new();
    for def in &doc.definitions {
        if let Definition::TypeDefinition(TypeDefinition::Object(obj)) = def {
            if obj.directives.iter().any(|d| d.name == "entity") {
                entity_names.push(obj.name.clone());
            }
        }
    }

    // Pass 2: build each entity's fields, now able to classify relations.
    let mut entities = BTreeMap::new();
    let mut order = Vec::new();

    for def in &doc.definitions {
        let Definition::TypeDefinition(TypeDefinition::Object(obj)) = def else {
            continue;
        };
        if !obj.directives.iter().any(|d| d.name == "entity") {
            continue;
        }

        let mut fields = Vec::with_capacity(obj.fields.len());
        for f in &obj.fields {
            let (base_type, is_list) = unwrap_type(&f.field_type);
            let nullable = !matches!(f.field_type, Type::NonNullType(_));

            // `@derivedFrom(field: "x")` marks a reverse relation with no column.
            let derived_from = f
                .directives
                .iter()
                .find(|d| d.name == "derivedFrom")
                .and_then(|d| {
                    d.arguments
                        .iter()
                        .find(|(k, _)| k == "field")
                        .map(|(_, v)| v.to_string().trim_matches('"').to_string())
                });

            let kind = if let Some(target_field) = derived_from {
                FieldKind::Derived {
                    target: base_type.clone(),
                    target_field,
                }
            } else if let Some(scalar) = ScalarType::from_sdl_name(&base_type) {
                FieldKind::Scalar(scalar)
            } else if entity_names.contains(&base_type) {
                FieldKind::Relation {
                    target: base_type.clone(),
                }
            } else {
                return Err(CoreError::UnsupportedType {
                    entity: obj.name.clone(),
                    field: f.name.clone(),
                    ty: base_type,
                });
            };

            // Derived fields have no column; everything else does.
            let column = match kind {
                FieldKind::Derived { .. } => None,
                _ => Some(field_to_column_name(&f.name)),
            };

            fields.push(Field {
                name: f.name.clone(),
                column,
                kind,
                is_list,
                nullable,
            });
        }

        // Every entity needs a primary key to be addressable by id.
        if !fields
            .iter()
            .any(|f| f.name == "id" && matches!(f.kind, FieldKind::Scalar(ScalarType::Id)))
        {
            return Err(CoreError::MissingId(obj.name.clone()));
        }

        order.push(obj.name.clone());
        entities.insert(
            obj.name.clone(),
            Entity {
                name: obj.name.clone(),
                table: model_to_table_name(&obj.name),
                fields,
            },
        );
    }

    let ir = SchemaIr { entities, order };
    validate_derived(&ir)?;
    Ok(ir)
}

/// Check that every `@derivedFrom` points at a field that actually exists and is
/// a relation back to this entity. Catching this at parse time turns a runtime
/// SQL error into a startup error with a name in it.
fn validate_derived(ir: &SchemaIr) -> CoreResult<()> {
    for entity in ir.entities() {
        for field in &entity.fields {
            let FieldKind::Derived {
                target,
                target_field,
            } = &field.kind
            else {
                continue;
            };
            let ok = ir
                .entity(target)
                .and_then(|t| t.field(target_field))
                .is_some_and(|f| matches!(&f.kind, FieldKind::Relation { .. }));
            if !ok {
                return Err(CoreError::BadDerivedFrom {
                    entity: entity.name.clone(),
                    field: field.name.clone(),
                    target_entity: target.clone(),
                    target: target_field.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Unwrap a GraphQL type to its base named type plus whether a list is involved.
fn unwrap_type(ty: &Type<'_, String>) -> (String, bool) {
    match ty {
        Type::NamedType(n) => (n.clone(), false),
        Type::ListType(inner) => (unwrap_type(inner).0, true),
        Type::NonNullType(inner) => unwrap_type(inner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SDL: &str = r#"
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

        type NotAnEntity {
          id: ID!
        }
    "#;

    #[test]
    fn parses_only_entities() {
        let ir = parse_sdl(SDL).unwrap();
        assert_eq!(ir.len(), 2);
        assert!(ir.entity("Transfer").is_some());
        assert!(ir.entity("NotAnEntity").is_none());
    }

    #[test]
    fn derives_table_and_column_names() {
        let ir = parse_sdl(SDL).unwrap();
        let t = ir.entity("Transfer").unwrap();
        assert_eq!(t.table, "transfers");
        assert_eq!(
            t.field("blockNumber").unwrap().column.as_deref(),
            Some("block_number")
        );
        assert_eq!(ir.entity("Account").unwrap().table, "accounts");
    }

    #[test]
    fn classifies_field_kinds() {
        let ir = parse_sdl(SDL).unwrap();
        let t = ir.entity("Transfer").unwrap();

        assert_eq!(
            t.field("value").unwrap().kind,
            FieldKind::Scalar(ScalarType::BigInt)
        );
        assert_eq!(
            t.field("fromAccount").unwrap().kind,
            FieldKind::Relation {
                target: "Account".into()
            }
        );

        let a = ir.entity("Account").unwrap();
        assert_eq!(
            a.field("transfers").unwrap().kind,
            FieldKind::Derived {
                target: "Transfer".into(),
                target_field: "fromAccount".into()
            }
        );
    }

    #[test]
    fn derived_fields_have_no_column() {
        let ir = parse_sdl(SDL).unwrap();
        let a = ir.entity("Account").unwrap();
        assert!(a.field("transfers").unwrap().column.is_none());
        // ...and are excluded from the stored projection.
        assert!(!a.stored_fields().any(|f| f.name == "transfers"));
    }

    #[test]
    fn foreign_key_column_is_the_field_name() {
        // `fromAccount: Account!` stores Account.id in column `from_account`.
        let ir = parse_sdl(SDL).unwrap();
        let f = ir.entity("Transfer").unwrap().field("fromAccount").unwrap();
        assert_eq!(f.column.as_deref(), Some("from_account"));
        assert_eq!(f.scalar(), Some(ScalarType::Id));
    }

    #[test]
    fn nullability_and_lists() {
        let ir = parse_sdl(SDL).unwrap();
        let t = ir.entity("Transfer").unwrap();
        assert!(!t.field("value").unwrap().nullable);
        assert!(t.field("data").unwrap().nullable);
        assert!(t.field("tags").unwrap().is_list);
    }

    #[test]
    fn empty_schema_yields_an_empty_ir_not_a_parse_error() {
        for input in ["", "   ", "\n\n"] {
            let ir = parse_sdl(input).expect("empty schema should parse");
            assert!(ir.is_empty());
        }
    }

    #[test]
    fn schema_with_no_entity_directive_is_empty() {
        // Valid SDL, just nothing indexed — not an error.
        let ir = parse_sdl("type Foo { id: ID! }").unwrap();
        assert!(ir.is_empty());
    }

    #[test]
    fn entity_without_id_is_rejected() {
        let err = parse_sdl("type Bad @entity { name: String! }").unwrap_err();
        assert!(matches!(err, CoreError::MissingId(e) if e == "Bad"));
    }

    #[test]
    fn unknown_type_is_rejected_with_location() {
        let err = parse_sdl("type X @entity { id: ID! v: Mystery }").unwrap_err();
        match err {
            CoreError::UnsupportedType { entity, field, ty } => {
                assert_eq!(
                    (entity.as_str(), field.as_str(), ty.as_str()),
                    ("X", "v", "Mystery")
                );
            }
            other => panic!("wrong error: {other}"),
        }
    }

    #[test]
    fn dangling_derived_from_is_rejected() {
        let sdl = r#"
            type A @entity { id: ID! bs: [B!]! @derivedFrom(field: "nope") }
            type B @entity { id: ID! }
        "#;
        assert!(matches!(
            parse_sdl(sdl),
            Err(CoreError::BadDerivedFrom { .. })
        ));
    }
}
