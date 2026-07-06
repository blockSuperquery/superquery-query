# Welcome to SuperQuery!

**Flexible, reliable, and decentralised APIs for your web3 project**

SuperQuery is an open, flexible, fast and universal data indexing framework for web3, focused on the [Stellar](https://stellar.org) and Soroban ecosystem. It lets any team process, transform, and query Stellar ledger data — operations, effects, transactions, and Soroban smart contract events — through a fully typed GraphQL API.

This repository is the **SuperQuery Stellar Starter**: the indexer node, common libraries, and type definitions for building Stellar data indexing projects. It also serves as the reference implementation for an ongoing port of the indexing engine to Rust.

## Get Started

#### Create a project

Follow the [SubQuery Academy Quick Start](https://academy.subquery.network/quickstart/quickstart.html) to learn the project structure (manifest, GraphQL schema, and mapping handlers) — the same concepts apply here.

#### Run your own Indexer and Query Service

You'll need a Postgres database, a Stellar/Soroban RPC endpoint to extract chain data, and a moderately powerful computer to run the indexer in the background.

Pair the indexer with the GraphQL query service [`@subql/query`](https://www.npmjs.com/package/@subql/query) to interact with your indexed data.

#### Components

* [`node`](packages/node) — the Stellar indexer node that fetches, filters, and processes ledger data
* [`common-stellar`](packages/common-stellar) — shared project/manifest handling for Stellar projects
* [`types`](packages/types) — TypeScript type definitions for Stellar handlers and data shapes

## Contribute

We love contributions and feedback. Please open an issue in this repository so we can give you support.

## Attribution & Copyright

SuperQuery is a fork of the [SubQuery Stellar SDK](https://github.com/subquery/subql-stellar), created to study the indexing engine and port it to Rust. The commit history intentionally preserves the work of the original authors — full credit goes to the [SubQuery](https://subquery.network) team and community contributors who built this.

Licensed under [GPL-3.0](LICENSE).

Copyright © 2022 [SubQuery Pte Ltd](https://subquery.network) authors & contributors
Copyright © 2026 SuperQuery authors & contributors
