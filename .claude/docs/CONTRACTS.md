# Cross-repo contracts

Everything here was read out of `superquery-node`'s source, not inferred. When
this document and [`IMPLEMENTATION_GUIDE.md`](./IMPLEMENTATION_GUIDE.md)
disagree, **this document wins** — it describes the database as it exists.

The three repositories:

| Repo | Language | Role |
|---|---|---|
| `superquery-sdk` | TypeScript (Nuxt app) | project authoring surface |
| `superquery-node` | Rust workspace (`crates/subql-*`) + legacy TS | indexes chain data, **owns all writes** |
| `superquery-query` | Rust workspace (this repo) | **read-only** GraphQL API |

---

## 1. Where the guide diverges from reality

| The guide assumes | What actually exists | Consequence |
|---|---|---|
| `superquery-sdk` publishes a canonical Rust `SchemaIr` crate (Milestone 0) | The SDK is a **Nuxt/TypeScript web app**. No Rust, no IR crate, no `schema`/`types` packages. | M0 as written is unbuildable. We parse `schema.graphql` ourselves instead. |
| A `_superquery_metadata` table with columns `project_id`, `schema_version`, `schema_hash`, `manifest_hash`, `indexed_height`, `finalized_height`, `query_schema_ir` | A `_metadata` table that is **key/value**: `key text PRIMARY KEY, value text NOT NULL`, inside a per-project Postgres schema. | No `project_id`, no stored IR. Project identity is the schema name. |
| `sqlx` + `PgListener` | The node uses `tokio-postgres` + `deadpool-postgres`. | We match the node. `sqlx`'s compile-time query checking is worthless here — every statement is generated at runtime — and would demand a live DB at build time. |

There is also a **stub `subql-query` crate inside `superquery-node`**
(`crates/subql-query/src/main.rs`, a `println!`). It is a placeholder. This
repository owns the real implementation; that stub should eventually be deleted
from the node to avoid two half-services.

---

## 2. Project identity is the Postgres schema name

There is no project id anywhere in the database. A project is a Postgres schema.

```bash
# node writes into schema "app"
superquery-node   --db-schema app

# query reads from schema "app"
superquery-query  --name app
```

Upstream SubQuery works exactly this way: `packages/query/src/yargs.ts` defines
`--name` as the required "Project name", and the reference `docker-compose.yml`
pairs `--db-schema=app` on the node with `--name=app` on the query service.

Pointing `--name` at the wrong schema is the most common misconfiguration, which
is why `build_state` lists the available schemas in the error.

---

## 3. Identifier naming — the load-bearing contract

The node creates tables through Sequelize, which derives names with the
`inflection` library. The query service must reproduce this **exactly** or it
reads tables that do not exist.

```text
table  = underscored(pluralize(EntityName))
column = underscored(fieldName)
```

| Entity / field | Postgres |
|---|---|
| `Transfer` | `transfers` |
| `MyEntity` | `my_entities` |
| `Entity` | `entities` |
| `Person` | `people` |
| `Status` | `statuses` |
| `blockHeight` | `block_height` |
| `HTTPServer` | `http_server` |

**Source:** `superquery-node/crates/subql-store/src/naming.rs`
**Our copy:** `crates/query-core/src/naming.rs`

The rules are vendored, not imported, so the repos build independently. The
duplication is only safe because the parity tests in `naming.rs` assert the same
ground-truth vectors the node asserts. **If you change one, change both.**

---

## 4. Column types

The inverse of the node's `ddl.rs` type map.

| GraphQL | Postgres | On the wire |
|---|---|---|
| `ID`, `String` | `text` | string |
| `Int` | `integer` | number |
| `Float` | `double precision` | number |
| `Boolean` | `boolean` | bool |
| `BigInt` | `numeric` | **string** |
| `BigDecimal` | `numeric` | **string** |
| `Bytes` | `bytea` | `0x`-prefixed hex string |
| `Date` | `timestamp` | RFC 3339 string |
| `Json`, any list | `jsonb` | JSON |

`BigInt` and `BigDecimal` are strings on the wire because a 256-bit value has no
exact `f64`, and JSON numbers are `f64` in every mainstream client. Serializing
them as numbers silently corrupts any balance above 2^53.

**Source:** `superquery-node/crates/subql-store/src/ddl.rs`
**Our copy:** `crates/query-core/src/scalar.rs` (with a parity test)

Values are bound as text and cast in SQL (`$1::text::numeric`), which is also
what the node does — it keeps numerics exact end to end.

---

## 5. `_metadata` keys

```text
<db-schema>._metadata
┌─────────────────────────────┬──────────────┐
│ key                    text │ value   text │  PRIMARY KEY (key)
├─────────────────────────────┼──────────────┤
│ lastProcessedHeight         │ 21012345     │
│ lastFinalizedVerifiedHeight │ 21012330     │
│ targetHeight                │ 21012400     │
│ historicalStateEnabled      │ true         │
└─────────────────────────────┴──────────────┘
```

Full key set written by the node: `lastProcessedHeight`,
`lastProcessedBlockTimestamp`, `lastFinalizedVerifiedHeight`, `targetHeight`,
`startHeight`, `chain`, `genesisHash`, `specName`, `indexerNodeVersion`,
`processedBlockCount`, `schemaMigrationCount`, `historicalStateEnabled`,
`deployments`, `dynamicDatasources`, `blockOffset`, `latestSyncedPoiHeight`,
`lastCreatedPoiHeight`.

Keys are camelCase **strings in a text column**, not identifiers — the
`underscored` naming rules do not apply to them.

Every value is `text` and parsed defensively: a key that is absent, empty or
unparseable yields `None` rather than failing startup, because the node writes
them incrementally as it indexes.

**Our copy:** `crates/query-core/src/project.rs`

---

## 6. Historical storage does not exist yet

The node's `subql-config` models `HistoricalMode` (`Height` is the default), but
`subql-store`'s `PlainModel` **upserts in place** — there is no `_block_range`
column and no entity versioning. Its own docs say the historical slice arrives
in a later milestone.

So historical queries cannot be implemented here yet, at all. Exposing a
`block:` argument now would return present-day values for every height, which is
worse than not offering the feature because it looks like it works.

`crates/postgres/src/historical.rs` fixes the intended contract and gates it on
the node's own `historicalStateEnabled` flag. See Milestone 9.

---

## 7. Subscriptions: contract defined, not yet emitted

The node does not publish `NOTIFY` messages. `crates/postgres/src/notify.rs`
defines the payload and channel naming both sides will use, so the node side can
be implemented against a fixed target:

```text
channel: superquery_<db_schema>_entities
payload: {"entity":"Transfer","operation":"SET","id":"0x…","block_height":21012345}
```

The payload is a **hint**, not data — subscribers re-read the row through the
normal query path, so results obey the same limits and shape as everything else,
and the notification stays under Postgres' 8000-byte payload cap.

`NOTIFY` is transactional: an aborted transaction emits nothing. That property is
the whole reason for using it rather than polling, and it is the Milestone 10
acceptance test.

---

## 8. Upstream SubQuery CLI surface

From [`packages/query/src/yargs.ts`](https://github.com/subquery/subql/blob/main/packages/query/src/yargs.ts),
for reference when adding flags:

`--name` (required), `--port`, `--playground`, `--indexer`, `--max-connection`
(10), `--query-limit` (100), `--query-batch-limit`, `--query-depth-limit`,
`--query-alias-limit`, `--query-complexity`, `--query-timeout` (10000),
`--query-explain`, `--unsafe`, `--subscription` (false), `--aggregate` (true),
`--disable-hot-schema` (false), `--sl-keep-alive-interval` (180000),
`--log-level`, `--log-path`, `--log-rotate`, `--output-fmt`, `--pg-ca`,
`--pg-key`, `--pg-cert`.

We match these names where the concept survives the port. Two deliberate
differences:

- **`--schema` is new.** Upstream derives everything from database
  introspection; we parse the project's `schema.graphql`, because introspection
  alone cannot distinguish `BigInt` from `BigDecimal` (both `numeric`) nor
  recover entity casing from a pluralized table name.
- **`--indexer` is dropped.** Upstream calls the node's HTTP endpoint for
  metadata; we read `_metadata` straight from Postgres, so the query service does
  not depend on the node being reachable.

---

## 9. Upstream plugin inventory

The PostGraphile plugins in
[`packages/query/src/graphql/plugins`](https://github.com/subquery/subql/tree/main/packages/query/src/graphql/plugins),
mapped to where their behaviour lives here:

| Upstream plugin | Here |
|---|---|
| `PgConnectionArgFirstLastBeforeAfter` | `query-core::pagination` |
| `PgOrderByUnique` | `query-core::sort` (implicit `id` tiebreaker) |
| `PgBackwardRelationPlugin` | `graphql::schema::derived_field` |
| `QueryDepthLimitPlugin` | `graphql::limits` → `limit_depth` |
| `QueryComplexityPlugin` | `graphql::limits` → `limit_complexity` |
| `QueryAliasLimitPlugin` | not yet — M12 |
| `GetMetadataPlugin` | `graphql::schema::meta_field`, `/meta` |
| `PgSubscriptionPlugin` | `postgres::notify` (contract only) — M10 |
| `PgAggregationPlugin`, `PgAggregateSpecsPlugin`, `PgOrderByAggregatesPlugin` | not yet — M13 |
| `PgSearchPlugin`, `PgDistinctPlugin` | not planned for v1 |
| `PlaygroundPlugin` | `server::graphql_http::playground` (GraphiQL) |
| `historical/` | `postgres::historical` — M9, blocked on node |

---

## 10. Licensing

SuperQuery is a GPL-3.0 fork of SubQuery. Upstream files are used as
architectural and behavioural references. Prefer independent Rust
implementations built from observed behaviour and public interfaces over
transliterated source; where behaviour must match byte-for-byte (naming rules,
type maps), encode it as a parity test rather than a copied implementation.
