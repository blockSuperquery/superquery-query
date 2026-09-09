# CLAUDE.md

Guidance for Claude Code when working in this repository.

## What this repo is

`superquery-query` is the **read-only GraphQL API** over data indexed by
`superquery-node`. Pure Rust, Cargo workspace. It never writes project entities.

Three sibling repos:

| Repo | Language | Role |
|---|---|---|
| `superquery-sdk` | TypeScript (Nuxt) | project authoring |
| `superquery-node` | Rust workspace | indexing — **owns all DB writes** |
| `superquery-query` | Rust workspace | this repo — read-only GraphQL |

## Read before changing anything

- [`.claude/docs/CONTRACTS.md`](.claude/docs/CONTRACTS.md) — what the node
  actually writes to Postgres. **Authoritative.** The implementation guide is
  wrong in three places; this file records them.
- [`.claude/tasks/rust-query-service.md`](.claude/tasks/rust-query-service.md) —
  milestone status, what is next, what is blocked and on what.
- [`.claude/docs/IMPLEMENTATION_GUIDE.md`](.claude/docs/IMPLEMENTATION_GUIDE.md)
  — the original design guide, for intent.

## Commands

```bash
cargo build
cargo test --workspace                    # no database needed
cargo clippy --workspace --all-targets
cargo run -p superquery-query -- --help
```

Run against a local database:

```bash
docker compose -f deploy/docker-compose.yml up -d postgres
DB_HOST=127.0.0.1 DB_PORT=5432 DB_USER=postgres DB_PASS=postgres DB_DATABASE=postgres \
  cargo run -p superquery-query -- --name app --schema ./schema.graphql --playground
```

## Layout

```text
crates/query-core   schema IR, naming, filter/sort/cursor IR   — no I/O, no deps on siblings
crates/postgres     pool, metadata, introspection, SQL          — depends on query-core
crates/graphql      async-graphql::dynamic schema + resolvers   — depends on both
crates/server       axum, config, health, startup               — depends on all
bins/superquery-query
```

## Invariants

These are not style preferences. Breaking one produces a wrong result or a
vulnerability.

1. **Client input never becomes SQL syntax.** Values bind as `$n`; identifiers
   come from the schema IR. A field name is resolved against the IR before any
   SQL is built — an unknown name is an error, never a passthrough. New
   operators go in `CmpOp` so the SQL compiler's `match` stays exhaustive.

2. **Naming must match the node byte-for-byte.** `table =
   underscored(pluralize(Entity))`, `column = underscored(field)`.
   `crates/query-core/src/naming.rs` is vendored from
   `superquery-node/crates/subql-store/src/naming.rs`. Changing either requires
   changing both, in the same change, with the parity tests updated.

3. **`BigInt` and `BigDecimal` are strings on the wire.** A 256-bit value has no
   exact `f64`, and JSON numbers are `f64` in every mainstream client. Never
   serialize them as numbers.

4. **Every ordering ends with `id`.** Without a unique tiebreaker, paginated
   reads silently repeat or skip rows. `Ordering::new` appends it.

5. **`query-core` stays I/O-free.**

6. **The service stays read-only.** The pool sets
   `default_transaction_read_only=on` per session. Keep it.

7. **Prefer a startup error over a resolver error.** An operator can fix a
   startup failure; a resolver error reaches a dApp at 3am instead.

## Gotchas

- **Project identity is the Postgres schema name.** There is no project id.
  `--name` here must equal `--db-schema` on the node.
- **`_metadata` is key/value**, not the columnar table the guide describes.
- **Historical and subscriptions are blocked on the node**, not on effort here.
  Their contracts are already fixed in `postgres::historical` and
  `postgres::notify`. Do not expose either until the node implements its side —
  a `block:` argument that silently returns present-day values is worse than no
  feature at all.
- **`graphql-parser` rejects an empty document**; `parse_sdl` special-cases it so
  an empty schema file gives a useful warning rather than a syntax error.
- There is a stub `subql-query` crate inside `superquery-node`. It is a
  placeholder; this repo owns the real implementation.

## Conventions

- Plans go in `.claude/tasks/TASK_NAME.md`, updated as work proceeds.
- Comments explain *why*, not *what*. The tricky parts here are the keyset
  predicate, the scalar/type parity with the node, and the injection boundary —
  those carry the density.
- Tests assert behaviour that matters: precision, injection resistance,
  pagination correctness under ties, parity with the node.
