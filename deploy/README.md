# Simple Deployment

Run a full SuperQuery Stellar stack — Postgres, the indexer node, and the GraphQL query service — with a single command:

```bash
docker compose up
```

## What it runs

1. **postgres** — the database where indexed data is stored (data persists in `.data/postgres`)
2. **subquery-node** — the Stellar indexer ([`subquerynetwork/subql-node-stellar`](https://hub.docker.com/r/subquerynetwork/subql-node-stellar)) which fetches, filters, and processes ledger data
3. **graphql-engine** — the query service ([`subquerynetwork/subql-query`](https://hub.docker.com/r/subquerynetwork/subql-query)), serving a GraphQL playground at [http://localhost:3000](http://localhost:3000)

## Usage

Edit the `subquery-node` volume in [docker-compose.yml](docker-compose.yml) to point at your project directory (the folder containing `project.ts`/`project.yaml`, `schema.graphql`, and your built mappings), then:

```bash
docker compose up
```

Once the indexer reports ready, open http://localhost:3000 and query your data.

Tuning knobs on the node command: `--workers` (parallel block fetching), `--batch-size` (blocks per batch), and `--db-schema` (Postgres schema name, useful for running multiple projects on one database).

For production, use a managed/dedicated Postgres instance instead of the bundled container for better performance and durability.
