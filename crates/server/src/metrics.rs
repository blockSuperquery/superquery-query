//! Metrics — **placeholder** (Milestone 14).
//!
//! The signals worth exporting, from the implementation guide, and why each one
//! earns its place:
//!
//! | Metric                  | What it catches                                  |
//! |-------------------------|--------------------------------------------------|
//! | request latency         | user-visible regressions                          |
//! | DB query latency        | separates slow SQL from slow resolvers            |
//! | resolver latency        | N+1 relation storms (Milestone 8's failure mode)  |
//! | pool saturation         | the usual cause of a cliff under load             |
//! | query complexity        | whether the configured limits are set sensibly    |
//! | active WebSockets       | subscription fan-out cost                         |
//! | errors by kind          | user error vs upstream failure                    |
//!
//! Benchmarks should run against a realistic generated schema. A hello-world
//! GraphQL benchmark measures the HTTP stack, not this service — the cost here
//! is dominated by dynamic schema resolution and SQL execution, neither of which
//! a trivial schema exercises.
