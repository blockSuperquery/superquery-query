//! GraphQL subscriptions — **not implemented** (Milestone 10).
//!
//! The transport contract is already fixed in
//! `superquery_postgres::notify`: the node will `NOTIFY` on a per-project
//! channel after each committed block, and this module will turn that stream
//! into GraphQL subscription events.
//!
//! ```text
//!   PgListener ──▶ broadcast::Sender ──▶ Subscription.transfers(filter: …)
//! ```
//!
//! # Ordering with the rest of the roadmap
//!
//! Deliberately last among the read features. A subscription is a long-lived
//! query, so it inherits every correctness question the one-shot path has —
//! filters, limits, scalar encoding — and adds fan-out, backpressure, and
//! per-connection resource accounting on top. Building it before the query path
//! is stable means debugging both at once.
//!
//! The acceptance test that matters: a committed entity change emits exactly one
//! event, and a rolled-back transaction emits none. That is a property of
//! `NOTIFY`'s transactional semantics, and it is the reason for using it rather
//! than polling.

/// Whether subscriptions are enabled for this deployment.
///
/// Mirrors upstream SubQuery's `--subscription` flag, which is off by default.
#[derive(Debug, Clone, Copy, Default)]
pub struct SubscriptionConfig {
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_by_default() {
        assert!(!SubscriptionConfig::default().enabled);
    }
}
