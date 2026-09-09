# SuperQuery Query — Rust Implementation Guide

> Repository: `blockSuperquery/superquery-query`
> Role: Read indexed PostgreSQL data and expose a generated GraphQL API.
> Closest upstream equivalent: SubQuery's `@subql/query` package.

> **Editorial note.** This is the original design guide, transcribed with its
> text encoding repaired. It is kept as written so the intent is preserved.
> Three of its assumptions turned out not to hold once the sibling repositories
> were inspected; those are recorded in [`CONTRACTS.md`](./CONTRACTS.md) and the
> resolutions are in [`../tasks/rust-query-service.md`](../tasks/rust-query-service.md).
> **Where this document and `CONTRACTS.md` disagree, `CONTRACTS.md` wins** — it
> describes the database as it actually exists.

---

# 1. What this repository owns

```text
superquery-node
      │
      │ writes
      ▼
 PostgreSQL
      │
      │ reads
      ▼
superquery-query
      │
      ├── dynamic GraphQL schema
      ├── filters
      ├── sort
      ├── pagination
      ├── relationships
      ├── historical reads
      ├── subscriptions
      └── query limits
      │
      ▼
 dApps / dashboards / users
```

`superquery-query` should be read-only with respect to indexed project entities.

It may maintain its own transient/cache metadata if needed, but **node owns
canonical indexed-state writes**.

---

# 2. Closest SubQuery equivalent

Primary package: [`subquery/subql/packages/query`](https://github.com/subquery/subql/tree/main/packages/query)

Important directories/files:

- [`configure`](https://github.com/subquery/subql/tree/main/packages/query/src/configure)
- [`graphql`](https://github.com/subquery/subql/tree/main/packages/query/src/graphql)
- [`utils`](https://github.com/subquery/subql/tree/main/packages/query/src/utils)
- [`app.module.ts`](https://github.com/subquery/subql/blob/main/packages/query/src/app.module.ts)
- [`main.ts`](https://github.com/subquery/subql/blob/main/packages/query/src/main.ts)
- [`yargs.ts`](https://github.com/subquery/subql/blob/main/packages/query/src/yargs.ts)

GraphQL core:

- [`graphql.module.ts`](https://github.com/subquery/subql/blob/main/packages/query/src/graphql/graphql.module.ts)
- [`project.service.ts`](https://github.com/subquery/subql/blob/main/packages/query/src/graphql/project.service.ts)
- [`plugins`](https://github.com/subquery/subql/tree/main/packages/query/src/graphql/plugins)

---

# 3. What SubQuery query is doing conceptually

```text
PostgreSQL schema
      │
      ▼
GraphQL schema builder
      │
      ├── project metadata
      ├── custom scalars
      ├── subscriptions
      ├── historical behavior
      ├── query limits
      └── hot reload
      │
      ▼
GraphQL HTTP/WebSocket service
```

For SuperQuery, reproduce the capabilities, **not the PostGraphile/Apollo
implementation stack**.

---

# 4. Recommended Rust architecture

```text
superquery-query/
├── Cargo.toml
├── crates/
│   ├── query-core/
│   ├── postgres/
│   ├── graphql/
│   └── server/
├── bins/
│   └── superquery-query/
└── tests/
```

---

# 5. Important design choice: do not port PostGraphile

## A. Database-introspection-driven GraphQL

Closest to SubQuery, but building a complete PostGraphile-like engine is a huge
project, and inferring all GraphQL semantics from PostgreSQL alone is difficult.

## B. Shared schema-IR-driven GraphQL — recommended

```text
schema.graphql
     │
     ▼
superquery-sdk parser
     │
     ▼
canonical Schema IR
     │
     ├── node creates/writes tables
     └── query creates GraphQL schema
```

The query service should still validate the actual PostgreSQL schema against the
IR so it fails clearly if the DB and project schema diverge.

---

# 6. Rust stack

`tokio`, `axum`, `async-graphql`, `async-graphql-axum`, `sqlx`, `serde`,
`serde_json`, `thiserror`, `anyhow`, `tracing`, `tower`, `tower-http`, `futures`.

For Postgres notification/subscription support, use `sqlx::postgres::PgListener`
or equivalent. Use parameterized SQL only.

---

# 7. Configuration and startup

```bash
superquery-query \
  --database-url postgres://... \
  --project-id erc20-transfers \
  --port 3000
```

Startup:

```text
load config → connect PostgreSQL → read metadata → load schema IR/version
  → validate tables → build GraphQL schema → start HTTP + WebSocket
```

Acceptance:

- readiness does not return success until DB + schema are usable
- startup errors say whether problem is DB, project ID or schema compatibility

---

# 8. Project discovery

SuperQuery needs a way to determine: project ID, schema version, indexed height,
finalized height, schema metadata/IR, historical support, available entities.

Do not require access to the original developer source directory.

---

# 9. Dynamic GraphQL schema

Use `async_graphql::dynamic`. Build objects from entity definitions, fields from
schema scalars, list fields, by-ID fields, filters, orderBy input, pagination,
and relation resolvers.

```graphql
type Transfer @entity {
  id: ID!
  from: String!
  to: String!
  value: BigInt!
}
```

becomes

```graphql
type Query {
  transfer(id: ID!): Transfer
  transfers(
    first: Int
    after: Cursor
    filter: TransferFilter
    orderBy: [TransferOrder!]
  ): TransferConnection!
}
```

---

# 10. Scalar mapping

| Project scalar | Rust/query representation |
|---|---|
| `ID` | `String` |
| `String` | `String` |
| `Boolean` | `bool` |
| `Int` | `i32` |
| `BigInt` | decimal/string-safe GraphQL scalar |
| `BigDecimal` | decimal GraphQL scalar |
| `Bytes` | hex GraphQL scalar |
| `Date` | ISO/RFC3339 scalar |

Do not serialize blockchain 256-bit values into JSON numbers and lose precision.

---

# 11. Basic entity queries

SQL generator must use placeholders. Never concatenate arbitrary field/value
input into SQL.

```sql
SELECT id, "from", "to", value FROM "transfers" LIMIT $1
```

---

# 12. Filtering

```rust
pub enum FilterExpr {
    Eq(Field, Value), Ne(Field, Value),
    Gt(Field, Value), Gte(Field, Value),
    Lt(Field, Value), Lte(Field, Value),
    In(Field, Vec<Value>),
    And(Vec<FilterExpr>), Or(Vec<FilterExpr>),
}
```

GraphQL converts user input to `FilterExpr`. Postgres crate compiles
`FilterExpr` into parameterized SQL.

Start with: equalTo, notEqualTo, greaterThan, greaterThanOrEqualTo, lessThan,
lessThanOrEqualTo, in, and, or. Add string operators later.

Acceptance: SQL injection attempts remain data parameters, never SQL syntax.

---

# 13. Sorting

Internally, only enum values generated from known schema fields are accepted. Do
not accept arbitrary raw column names from clients.

---

# 14. Cursor pagination

Prefer cursor pagination over large offsets. Cursor should be opaque externally.
Queries must include a deterministic tiebreaker.

Bad: `ORDER BY block_number DESC`
Better: `ORDER BY block_number DESC, id ASC`

---

# 15. Relationships

Implement: direct FK relation, derived reverse relation, one-to-many, nullable
relation. Avoid N+1 queries using batching/DataLoader-style execution.

---

# 16. Historical queries

This feature **cannot be implemented correctly in query alone**. Contract needed
from node: entity version, `valid_from_height`, `valid_to_height`, or an
equivalent historical model.

Do not expose historical GraphQL until node's historical storage semantics are
stable.

---

# 17. Subscriptions

```text
node commits entity changes → PostgreSQL NOTIFY → PgListener
  → query event bus → GraphQL WebSocket subscriber
```

Node may publish a lightweight notification such as:

```json
{"project": "erc20", "entity": "Transfer", "operation": "SET", "id": "..."}
```

Start subscriptions after ordinary queries are stable.

---

# 18. Schema hot reload

Requirements: in-flight requests continue safely; new requests see new schema;
failed rebuild leaves old schema active; incompatible schema requires explicit
restart/migration.

---

# 19. Query protection

Implement: maximum query depth, maximum aliases, complexity/cost score,
result/page limits, request timeout, body size limit, connection limit/rate
limiting later.

Do not expose an unrestricted generated GraphQL API to the public internet.

```text
default page size: 20
max page size: 100
max query depth: configurable
request timeout: configurable
```

Avoid hard-coding values into core types; expose server config.

---

# 20. Health and metadata endpoints

```text
GET /health
GET /ready
GET /meta
POST /graphql
GET/WS /graphql
```

Do not leak database credentials or RPC configuration.

---

# 21. Step-by-step implementation

| Milestone | Deliverable |
|---|---|
| M0 | Consume shared SDK schema model |
| M1 | Axum server + Postgres pool |
| M2 | Project metadata discovery |
| M3 | Dynamic scalar/entity GraphQL schema |
| M4 | By-ID query |
| M5 | Collection query |
| M6 | Filtering |
| M7 | Sorting + cursor pagination |
| M8 | Relationships |
| M9 | Historical reads |
| M10 | WebSocket subscriptions |
| M11 | Schema hot reload |
| M12 | Query limits |
| M13 | Aggregates |
| M14 | Metrics/performance |

---

# 22. V0.1 definition of done

```graphql
query {
  transfers(first: 10, filter: { from: { equalTo: "0x..." } }) {
    nodes { id from to value blockNumber }
  }
}
```

---

# 23. Cross-repo integration test

```text
superquery-sdk → mapping + SchemaIr → superquery-node → PostgreSQL
  → superquery-query → GraphQL
```

Assertion: expected transaction/log produces exactly expected entity; GraphQL
returns it; restart preserves state; synthetic reorg updates result correctly.

---

# 24. Grant-friendly issue sequence

1. Bootstrap Axum GraphQL server
2. Add PostgreSQL pool and readiness
3. Read project metadata
4. Load canonical Schema IR
5. Map project scalars to GraphQL scalars
6. Generate dynamic entity objects
7. Add by-ID resolver
8. Add collection resolver
9. Define typed filter IR
10. Compile filters to parameterized SQL
11. Add safe sorting enums
12. Add cursor codec
13. Add cursor pagination
14. Add direct relations
15. Add derived relations
16. Add resolver batching
17. Add historical block selector
18. Add PostgreSQL notification listener
19. Add GraphQL subscriptions
20. Add hot schema reload
21. Add query depth limit
22. Add complexity limit
23. Add metrics
24. Add aggregate queries

---

# 25. What not to port blindly

Do not recreate Apollo Server, PostGraphile, the Graphile plugin ecosystem, or
NestJS inside Rust. Instead map behavior:

| Upstream concept | SuperQuery Rust |
|---|---|
| Apollo/Nest server wiring | Axum + Tower |
| PostGraphile schema construction | `async-graphql::dynamic` + Schema IR |
| node-postgres pool | connection pool |
| GraphQL WS stack | `async-graphql` subscriptions + Axum WS |
| PG pub/sub | `LISTEN`/`NOTIFY` listener |
| JS plugins | explicit Rust middleware/schema passes |

---

# 26. Source-license note

Use the linked SubQuery files as architectural and behavioral references. Before
copying source code or substantial implementation text, verify the license and
notices for the exact package/repository/version. Prefer an independent Rust
implementation built from the behavior and public interfaces.
