# SuperQuery Query

**GraphQL API over blockchain data indexed by [`superquery-node`](https://github.com/blockSuperquery/superquery-node).**

Point it at a Postgres schema the node writes to and a project's
`schema.graphql`, and it generates and serves a typed GraphQL API — filters,
ordering, cursor pagination and relations — at runtime.

```text
superquery-node ──writes──▶ PostgreSQL ──reads──▶ superquery-query ──▶ dApps
```

Pure Rust: `axum`, `async-graphql` (dynamic schema), `tokio-postgres`.
Read-only by construction — the node owns every write.

## Quick start

```bash
cargo build --release

DB_HOST=127.0.0.1 DB_USER=postgres DB_PASS=postgres DB_DATABASE=postgres \
./target/release/superquery-query \
  --name app \
  --schema ./project/schema.graphql \
  --port 3000 \
  --playground
```

`--name` is the Postgres schema the node writes to — it must match the node's
`--db-schema`. That schema name **is** the project's identity.

Then open <http://localhost:3000> for the playground.

## What you get

Given a project schema:

```graphql
type Transfer @entity {
  id: ID!
  from: String!
  to: String!
  value: BigInt!
  blockNumber: Int!
  fromAccount: Account!
}

type Account @entity {
  id: ID!
  balance: BigInt!
  transfers: [Transfer!]! @derivedFrom(field: "fromAccount")
}
```

you can query:

```graphql
{
  transfers(
    first: 10
    filter: { from: { equalTo: "0xabc" }, value: { greaterThan: "1000000" } }
    orderBy: [BLOCK_NUMBER_DESC]
  ) {
    nodes {
      id
      value              # exact, even at 2^256-1 — BigInt is a string
      fromAccount { id balance }
    }
    pageInfo { hasNextPage endCursor }
  }

  _meta { indexedHeight finalizedHeight blocksBehind }
}
```

## Endpoints

| Endpoint | Purpose |
|---|---|
| `POST /graphql` | the API |
| `GET /` | GraphiQL playground (with `--playground`) |
| `GET /health` | liveness — never touches the database |
| `GET /ready` | readiness — 503 while Postgres is unreachable |
| `GET /meta` | index state: heights, chain, entities |

`/health` and `/ready` answer different questions on purpose: a database outage
should remove an instance from the load balancer, not restart it.

## Configuration

Flags follow upstream SubQuery's `@subql/query` where the concept survives the
port. Run `--help` for the full list.

| Flag | Default | |
|---|---|---|
| `--name` | *required* | Postgres schema = project identity |
| `--schema` | *required* | path to `schema.graphql` |
| `--port` | `3000` | |
| `--playground` | off | GraphiQL at `GET /` |
| `--query-limit` | `100` | max rows per connection |
| `--query-depth-limit` | `10` | max nesting |
| `--query-complexity` | `1000` | max complexity score |
| `--query-timeout` | `10000` | ms |
| `--unsafe` | off | disables the limits above |

Database connection comes from `DB_HOST`, `DB_PORT`, `DB_USER`, `DB_PASS`,
`DB_DATABASE` — the same variables the node reads, so one environment block
configures both.

**Do not run with `--unsafe` on the public internet.** A generated GraphQL API
over a relational database is an excellent denial-of-service target.

## Design notes

- **`BigInt` is a string.** A 256-bit value has no exact `f64`, and JSON numbers
  are `f64` in every mainstream client. Sending them as numbers silently
  corrupts any balance above 2^53.
- **Cursor pagination, not offsets.** `OFFSET 50000` makes Postgres walk and
  discard 50 000 rows, and it skips or repeats rows as the indexer appends. Every
  ordering carries an implicit `id` tiebreaker so pages are deterministic.
- **Client input never becomes SQL syntax.** Field names are resolved against the
  schema before any SQL is built; values always bind as parameters.
- **Startup validates the schema against the database** and reports every
  mismatch at once, so a node/query version skew fails at boot rather than inside
  a resolver.

Deeper detail: [`.claude/docs/CONTRACTS.md`](.claude/docs/CONTRACTS.md) and
[`.claude/tasks/rust-query-service.md`](.claude/tasks/rust-query-service.md).

## Status

Working: dynamic schema generation, by-id and collection queries, filtering,
sorting, cursor pagination, forward and reverse relations, query limits, health
and metadata endpoints.

Blocked on node support: historical queries (needs entity versioning) and
subscriptions (needs `NOTIFY` on commit). Both contracts are already defined in
this repo so the node side has a fixed target.

Next up: DataLoader batching for relations.

## Development

```bash
cargo test --workspace          # 127 tests, no database required
cargo clippy --workspace --all-targets
```

## Attribution & licence

SuperQuery is a fork of [SubQuery](https://subquery.network), created to port the
indexing stack to Rust. Upstream `@subql/query` is used as an architectural and
behavioural reference, not transliterated — full credit to the SubQuery team and
contributors.

Licensed under [GPL-3.0](LICENSE).

Copyright © 2022 [SubQuery Pte Ltd](https://subquery.network) authors & contributors
Copyright © 2026 SuperQuery authors & contributors
