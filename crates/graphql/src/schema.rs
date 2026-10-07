//! Dynamic GraphQL schema construction.
//!
//! The generated API is built at runtime from the schema IR with
//! `async_graphql::dynamic` — the Rust answer to what PostGraphile does for
//! SubQuery, minus the plugin ecosystem. For each `@entity`:
//!
//! ```text
//!   type Transfer @entity { … }
//!         │
//!         ├──▶ type Transfer                 (object)
//!         ├──▶ type TransferConnection       (nodes, edges, pageInfo)
//!         ├──▶ type TransferEdge             (cursor, node)
//!         ├──▶ input TransferFilter
//!         ├──▶ enum TransfersOrderBy
//!         ├──▶ Query.transfer(id: ID!)
//!         └──▶ Query.transfers(first, after, filter, orderBy)
//! ```
//!
//! Resolvers carry rows as `serde_json` maps rather than generated structs,
//! because the entity set is not known until runtime. Each object field looks up
//! its own key in the parent map.

use std::sync::Arc;

use async_graphql::dynamic::{
    Field, FieldFuture, FieldValue, InputValue, Object, ResolverContext, Schema, SchemaError,
    TypeRef,
};
use async_graphql::Value as GqlValue;
use superquery_postgres::row::EntityRow;
use superquery_postgres::{decode_row, sql};
use superquery_query_core::pagination::PageDirection;
use superquery_query_core::schema::{Entity, FieldKind};
use superquery_query_core::{Cursor, Ordering, PageInfo, PageRequest, ScalarType, SchemaIr};

use crate::context::QueryContext;
use crate::limits::Limits;
use crate::loader::{DerivedKey, EntityKey};
use crate::{filters, naming, ordering, scalars};

/// A resolved page, passed from the collection resolver to the connection fields.
struct ConnectionData {
    rows: Vec<EntityRow>,
    cursors: Vec<String>,
    page_info: PageInfo,
}

/// Build the complete GraphQL schema for a project.
pub fn build_schema(
    ir: Arc<SchemaIr>,
    db: superquery_postgres::Database,
    db_schema: impl Into<String>,
    limits: Limits,
) -> Result<Schema, SchemaError> {
    let ctx = QueryContext::new(db, Arc::clone(&ir), db_schema, limits.clone());

    let mut builder = Schema::build("Query", None, None);

    for scalar in scalars::scalar_types() {
        builder = builder.register(scalar);
    }
    for input in filters::scalar_filter_inputs() {
        builder = builder.register(input);
    }
    builder = builder.register(page_info_object());
    builder = builder.register(meta_object());

    let mut query = Object::new("Query");

    for entity in ir.entities() {
        builder = builder
            .register(entity_object(entity))
            .register(edge_object(entity))
            .register(connection_object(entity))
            .register(filters::entity_filter_input(entity));

        if ordering::has_order_by(entity) {
            builder = builder.register(ordering::order_by_enum(entity));
        }

        query = query
            .field(by_id_field(entity))
            .field(collection_field(entity));
    }

    query = query.field(meta_field());
    builder = builder.register(query).data(ctx);

    if let Some(depth) = limits.max_depth {
        builder = builder.limit_depth(depth);
    }
    if let Some(complexity) = limits.max_complexity {
        builder = builder.limit_complexity(complexity);
    }

    builder.finish()
}

// ---------------------------------------------------------------------------
// Object types
// ---------------------------------------------------------------------------

/// The entity object: one GraphQL field per schema field.
fn entity_object(entity: &Entity) -> Object {
    let mut object =
        Object::new(&entity.name).description(format!("Indexed `{}` entities.", entity.name));

    for field in &entity.fields {
        object = match &field.kind {
            FieldKind::Scalar(scalar) => object.field(scalar_field(field, *scalar)),
            FieldKind::Relation { target } => object.field(relation_field(field, target)),
            FieldKind::Derived {
                target,
                target_field,
            } => object.field(derived_field(field, target, target_field)),
        };
    }

    object
}

/// A plain column: read the parent row's entry for this field.
fn scalar_field(field: &superquery_query_core::SchemaField, scalar: ScalarType) -> Field {
    let name = field.name.clone();
    let type_ref = scalar_type_ref(scalar, field.is_list, field.nullable);

    Field::new(&field.name, type_ref, move |ctx: ResolverContext| {
        let name = name.clone();
        FieldFuture::new(async move {
            let row = ctx.parent_value.try_downcast_ref::<EntityRow>()?;
            match row.get(&name) {
                None | Some(serde_json::Value::Null) => Ok(None),
                Some(value) => Ok(Some(FieldValue::value(GqlValue::from_json(value.clone())?))),
            }
        })
    })
}

/// A foreign key: fetch the referenced row by the id stored in this column.
///
/// Goes through the [`EntityLoader`](crate::loader::EntityLoader), so every
/// parent in a page shares one `WHERE id = ANY($1)` instead of issuing its own
/// statement.
fn relation_field(field: &superquery_query_core::SchemaField, target: &str) -> Field {
    let name = field.name.clone();
    let target = target.to_string();
    let type_ref = if field.nullable {
        TypeRef::named(&target)
    } else {
        TypeRef::named_nn(&target)
    };

    Field::new(&field.name, type_ref, move |ctx: ResolverContext| {
        let (name, target) = (name.clone(), target.clone());
        FieldFuture::new(async move {
            let row = ctx.parent_value.try_downcast_ref::<EntityRow>()?;
            let Some(id) = row.get(&name).and_then(|v| v.as_str()) else {
                return Ok(None);
            };

            let qctx = ctx.data::<QueryContext>()?;
            let loaded = qctx
                .entities
                .load_one(EntityKey::new(target, id))
                .await
                .map_err(to_gql)?;

            Ok(loaded.map(FieldValue::owned_any))
        })
    })
}

/// A `@derivedFrom` reverse relation: a filtered list on the owning side.
///
/// Exposed as a plain list rather than a connection, matching SubQuery, since
/// reverse relations are usually small. Loaded through the
/// [`DerivedLoader`](crate::loader::DerivedLoader): every parent in the layer
/// shares one statement, and each parent is capped at the default page size so
/// a pathological parent cannot pull an unbounded set.
fn derived_field(
    field: &superquery_query_core::SchemaField,
    target: &str,
    target_field: &str,
) -> Field {
    let target = target.to_string();
    let target_field = target_field.to_string();
    let type_ref = TypeRef::named_nn_list_nn(&target);

    Field::new(&field.name, type_ref, move |ctx: ResolverContext| {
        let (target, target_field) = (target.clone(), target_field.clone());
        FieldFuture::new(async move {
            let row = ctx.parent_value.try_downcast_ref::<EntityRow>()?;
            let Some(parent_id) = row.get("id").and_then(|v| v.as_str()) else {
                return Ok(None);
            };

            let qctx = ctx.data::<QueryContext>()?;
            let key = DerivedKey {
                entity: target,
                fk_field: target_field,
                parent_id: parent_id.to_string(),
            };
            let rows = qctx
                .derived
                .load_one(key)
                .await
                .map_err(to_gql)?
                .unwrap_or_default();

            Ok(Some(FieldValue::list(
                rows.into_iter().map(FieldValue::owned_any),
            )))
        })
    })
}

fn edge_object(entity: &Entity) -> Object {
    let entity_name = entity.name.clone();
    Object::new(naming::edge_type(&entity.name))
        .description(format!("A `{}` and its pagination cursor.", entity.name))
        .field(Field::new(
            "cursor",
            TypeRef::named_nn("Cursor"),
            |ctx: ResolverContext| {
                FieldFuture::new(async move {
                    let cursor = ctx.parent_value.try_downcast_ref::<(EntityRow, String)>()?;
                    Ok(Some(FieldValue::value(cursor.1.clone())))
                })
            },
        ))
        .field(Field::new(
            "node",
            TypeRef::named_nn(&entity_name),
            |ctx: ResolverContext| {
                FieldFuture::new(async move {
                    let pair = ctx.parent_value.try_downcast_ref::<(EntityRow, String)>()?;
                    Ok(Some(FieldValue::owned_any(pair.0.clone())))
                })
            },
        ))
}

fn connection_object(entity: &Entity) -> Object {
    let entity_name = entity.name.clone();
    let edge_name = naming::edge_type(&entity.name);

    Object::new(naming::connection_type(&entity.name))
        .description(format!("A paginated list of `{}`.", entity.name))
        // `nodes` is the ergonomic path; `edges` exists for cursor access.
        .field(Field::new(
            "nodes",
            TypeRef::named_nn_list_nn(&entity_name),
            |ctx: ResolverContext| {
                FieldFuture::new(async move {
                    let data = ctx.parent_value.try_downcast_ref::<ConnectionData>()?;
                    Ok(Some(FieldValue::list(
                        data.rows.iter().cloned().map(FieldValue::owned_any),
                    )))
                })
            },
        ))
        .field(Field::new(
            "edges",
            TypeRef::named_nn_list_nn(&edge_name),
            |ctx: ResolverContext| {
                FieldFuture::new(async move {
                    let data = ctx.parent_value.try_downcast_ref::<ConnectionData>()?;
                    let edges: Vec<_> = data
                        .rows
                        .iter()
                        .cloned()
                        .zip(data.cursors.iter().cloned())
                        .map(FieldValue::owned_any)
                        .collect();
                    Ok(Some(FieldValue::list(edges)))
                })
            },
        ))
        .field(Field::new(
            "pageInfo",
            TypeRef::named_nn("PageInfo"),
            |ctx: ResolverContext| {
                FieldFuture::new(async move {
                    let data = ctx.parent_value.try_downcast_ref::<ConnectionData>()?;
                    Ok(Some(FieldValue::owned_any(data.page_info.clone())))
                })
            },
        ))
}

fn page_info_object() -> Object {
    fn bool_field(name: &'static str, get: fn(&PageInfo) -> bool) -> Field {
        Field::new(
            name,
            TypeRef::named_nn(TypeRef::BOOLEAN),
            move |ctx: ResolverContext| {
                FieldFuture::new(async move {
                    let info = ctx.parent_value.try_downcast_ref::<PageInfo>()?;
                    Ok(Some(FieldValue::value(get(info))))
                })
            },
        )
    }

    fn cursor_field(name: &'static str, get: fn(&PageInfo) -> Option<String>) -> Field {
        Field::new(
            name,
            TypeRef::named("Cursor"),
            move |ctx: ResolverContext| {
                FieldFuture::new(async move {
                    let info = ctx.parent_value.try_downcast_ref::<PageInfo>()?;
                    Ok(get(info).map(FieldValue::value))
                })
            },
        )
    }

    Object::new("PageInfo")
        .description("Relay-style cursor pagination metadata.")
        .field(bool_field("hasNextPage", |i| i.has_next_page))
        .field(bool_field("hasPreviousPage", |i| i.has_previous_page))
        .field(cursor_field("startCursor", |i| i.start_cursor.clone()))
        .field(cursor_field("endCursor", |i| i.end_cursor.clone()))
}

/// `_meta` — index state, so a dApp can tell how fresh a result is.
fn meta_object() -> Object {
    fn height(
        name: &'static str,
        get: fn(&superquery_query_core::ProjectMeta) -> Option<i64>,
    ) -> Field {
        Field::new(
            name,
            TypeRef::named("BigInt"),
            move |ctx: ResolverContext| {
                FieldFuture::new(async move {
                    let meta = ctx
                        .parent_value
                        .try_downcast_ref::<superquery_query_core::ProjectMeta>()?;
                    Ok(get(meta).map(|v| FieldValue::value(v.to_string())))
                })
            },
        )
    }

    Object::new("_Meta")
        .description("State of the index backing this API.")
        .field(height("indexedHeight", |m| m.last_processed_height))
        .field(height("finalizedHeight", |m| {
            m.last_finalized_verified_height
        }))
        .field(height("targetHeight", |m| m.target_height))
        .field(height("blocksBehind", |m| m.blocks_behind()))
        .field(Field::new(
            "chain",
            TypeRef::named(TypeRef::STRING),
            |ctx: ResolverContext| {
                FieldFuture::new(async move {
                    let meta = ctx
                        .parent_value
                        .try_downcast_ref::<superquery_query_core::ProjectMeta>()?;
                    Ok(meta.chain.clone().map(FieldValue::value))
                })
            },
        ))
        .field(Field::new(
            "historicalStateEnabled",
            TypeRef::named_nn(TypeRef::BOOLEAN),
            |ctx: ResolverContext| {
                FieldFuture::new(async move {
                    let meta = ctx
                        .parent_value
                        .try_downcast_ref::<superquery_query_core::ProjectMeta>()?;
                    Ok(Some(FieldValue::value(meta.historical_state_enabled)))
                })
            },
        ))
}

fn meta_field() -> Field {
    Field::new(
        "_meta",
        TypeRef::named_nn("_Meta"),
        |ctx: ResolverContext| {
            FieldFuture::new(async move {
                let qctx = ctx.data::<QueryContext>()?;
                // Read through rather than cache: staleness here is exactly the
                // thing callers are asking about.
                let rows = qctx
                    .db
                    .read_metadata(&qctx.db_schema)
                    .await
                    .map_err(to_gql)?;
                let meta = superquery_query_core::ProjectMeta::from_rows(&qctx.db_schema, rows);
                Ok(Some(FieldValue::owned_any(meta)))
            })
        },
    )
}

// ---------------------------------------------------------------------------
// Query root fields
// ---------------------------------------------------------------------------

fn by_id_field(entity: &Entity) -> Field {
    let entity_name = entity.name.clone();

    Field::new(
        naming::by_id_field(&entity.name),
        TypeRef::named(&entity_name),
        move |ctx: ResolverContext| {
            let entity_name = entity_name.clone();
            FieldFuture::new(async move {
                let id = ctx.args.try_get("id")?.string()?.to_string();
                let qctx = ctx.data::<QueryContext>()?;
                let entity = lookup_entity(&qctx.ir, &entity_name)?;

                let (query, fields) =
                    sql::select_by_id(&qctx.db_schema, entity, &id).map_err(to_gql)?;
                let rows = qctx
                    .db
                    .query(&query.sql, &query.params_as_refs())
                    .await
                    .map_err(to_gql)?;

                // A missing row is null, not an error — that is what GraphQL
                // nullability is for.
                match rows.first() {
                    Some(r) => Ok(Some(FieldValue::owned_any(
                        decode_row(r, &fields).map_err(to_gql)?,
                    ))),
                    None => Ok(None),
                }
            })
        },
    )
    .argument(InputValue::new("id", TypeRef::named_nn(TypeRef::ID)))
    .description(format!("Fetch a single `{}` by id.", entity.name))
}

fn collection_field(entity: &Entity) -> Field {
    let entity_name = entity.name.clone();
    let has_ordering = ordering::has_order_by(entity);
    let order_enum = naming::order_by_enum(&entity.name);

    let mut field = Field::new(
        naming::collection_field(&entity.name),
        TypeRef::named_nn(naming::connection_type(&entity.name)),
        move |ctx: ResolverContext| {
            let entity_name = entity_name.clone();
            FieldFuture::new(async move { resolve_collection(ctx, &entity_name).await })
        },
    )
    .description(format!("Fetch a paginated list of `{}`.", entity.name))
    .argument(InputValue::new("first", TypeRef::named(TypeRef::INT)))
    .argument(InputValue::new("last", TypeRef::named(TypeRef::INT)))
    .argument(InputValue::new("after", TypeRef::named("Cursor")))
    .argument(InputValue::new("before", TypeRef::named("Cursor")))
    .argument(InputValue::new(
        "filter",
        TypeRef::named(naming::filter_input(&entity.name)),
    ));

    if has_ordering {
        field = field.argument(InputValue::new(
            "orderBy",
            TypeRef::named_nn_list(order_enum),
        ));
    }

    field
}

/// The collection resolver: parse arguments, run one statement, shape a page.
async fn resolve_collection<'a>(
    ctx: ResolverContext<'a>,
    entity_name: &str,
) -> async_graphql::Result<Option<FieldValue<'a>>> {
    let qctx = ctx.data::<QueryContext>()?;
    let entity = lookup_entity(&qctx.ir, entity_name)?;

    // orderBy first: the cursor's arity depends on it.
    let order_values: Vec<String> = match ctx.args.get("orderBy") {
        Some(v) => v
            .list()?
            .iter()
            .map(|item| item.enum_name().map(str::to_string))
            .collect::<async_graphql::Result<_>>()?,
        None => Vec::new(),
    };
    let ordering = ordering::parse_ordering(entity, &order_values).map_err(to_gql)?;

    let filter = match ctx.args.get("filter") {
        Some(v) => {
            let json = v.as_value().clone().into_json()?;
            filters::parse_filter(entity, &json).map_err(to_gql)?
        }
        None => None,
    };

    let page = PageRequest::resolve(
        optional_u32(&ctx, "first")?,
        optional_u32(&ctx, "last")?,
        optional_str(&ctx, "after")?.as_deref(),
        optional_str(&ctx, "before")?.as_deref(),
        ordering.keys().len(),
        qctx.limits.max_page_size,
        qctx.limits.default_page_size,
    )
    .map_err(to_gql)?;

    let (query, fields) =
        sql::select_collection(&qctx.db_schema, entity, filter.as_ref(), &ordering, &page)
            .map_err(to_gql)?;

    let raw = qctx
        .db
        .query(&query.sql, &query.params_as_refs())
        .await
        .map_err(to_gql)?;

    // We asked for limit+1; the extra row proves another page exists.
    let has_more = raw.len() > page.limit as usize;
    let mut rows = raw
        .iter()
        .take(page.limit as usize)
        .map(|r| decode_row(r, &fields).map_err(to_gql))
        .collect::<async_graphql::Result<Vec<_>>>()?;

    // Backward pages were fetched in reverse order; restore the client's.
    if page.direction == PageDirection::Backward {
        rows.reverse();
    }

    let cursors = rows
        .iter()
        .map(|row| build_cursor(entity, &ordering, row).map(|c| c.encode()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(to_gql)?;

    let (has_next_page, has_previous_page) = match page.direction {
        PageDirection::Forward => (has_more, page.cursor.is_some()),
        // Reading backward, "more" means more rows *before* this page.
        PageDirection::Backward => (page.cursor.is_some(), has_more),
    };

    let page_info = PageInfo {
        has_next_page,
        has_previous_page,
        start_cursor: cursors.first().cloned(),
        end_cursor: cursors.last().cloned(),
    };

    Ok(Some(FieldValue::owned_any(ConnectionData {
        rows,
        cursors,
        page_info,
    })))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a row's cursor from the ordering keys' values.
///
/// Values are encoded exactly as [`ScalarType::encode_param`] would bind them,
/// so the keyset comparison compares like with like (notably: `Bytes` loses its
/// `0x` prefix, matching the `decode(…, 'hex')` cast).
fn build_cursor(
    entity: &Entity,
    ordering: &Ordering,
    row: &EntityRow,
) -> superquery_query_core::CoreResult<Cursor> {
    let mut values = Vec::with_capacity(ordering.keys().len());

    for key in ordering.keys() {
        let name = key.field.name();
        let field =
            entity
                .field(name)
                .ok_or_else(|| superquery_query_core::CoreError::UnknownField {
                    entity: entity.name.clone(),
                    field: name.to_string(),
                })?;
        let scalar = field.scalar().unwrap_or(ScalarType::String);
        let value = row.get(name).cloned().unwrap_or(serde_json::Value::Null);
        values.push(scalar.encode_param(&value)?);
    }

    Ok(Cursor::new(values))
}

fn lookup_entity<'a>(ir: &'a SchemaIr, name: &str) -> async_graphql::Result<&'a Entity> {
    ir.entity(name).ok_or_else(|| {
        // Only reachable if the schema was swapped mid-request with an
        // incompatible one; hot reload must keep old requests on their IR.
        async_graphql::Error::new(format!("entity `{name}` is not in the active schema"))
    })
}

/// The GraphQL `Int` type is i32; page sizes are u32. Negative is a user error.
fn optional_u32(ctx: &ResolverContext, name: &str) -> async_graphql::Result<Option<u32>> {
    match ctx.args.get(name) {
        Some(v) => {
            let n = v.i64()?;
            u32::try_from(n)
                .map(Some)
                .map_err(|_| async_graphql::Error::new(format!("`{name}` must not be negative")))
        }
        None => Ok(None),
    }
}

fn optional_str(ctx: &ResolverContext, name: &str) -> async_graphql::Result<Option<String>> {
    match ctx.args.get(name) {
        Some(v) => Ok(Some(v.string()?.to_string())),
        None => Ok(None),
    }
}

/// GraphQL type reference for a scalar field, honouring list and nullability.
fn scalar_type_ref(scalar: ScalarType, is_list: bool, nullable: bool) -> TypeRef {
    let name = scalar.graphql_name();
    match (is_list, nullable) {
        (false, true) => TypeRef::named(name),
        (false, false) => TypeRef::named_nn(name),
        (true, true) => TypeRef::named_nn_list(name),
        (true, false) => TypeRef::named_nn_list_nn(name),
    }
}

/// Surface a domain error as a GraphQL error.
///
/// These are user errors (bad cursor, unknown field) or upstream failures; the
/// message is safe to return — it never contains credentials or SQL text.
fn to_gql<E: std::fmt::Display>(err: E) -> async_graphql::Error {
    async_graphql::Error::new(err.to_string())
}
