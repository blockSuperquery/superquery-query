//! GraphQL over WebSocket — **not wired up** (Milestone 10).
//!
//! The transport half is straightforward: `async_graphql_axum::GraphQLSubscription`
//! speaks `graphql-transport-ws` over an Axum upgrade. What is missing is the
//! event source — the node does not yet publish the `NOTIFY` messages that
//! `superquery_postgres::notify` defines the contract for.
//!
//! Registering the route before that exists would advertise a subscription that
//! silently never fires, which is worse than a schema that does not offer one.
//! See `superquery_graphql::subscriptions` for the sequencing rationale.
