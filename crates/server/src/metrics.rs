//! Metrics (Milestone 14) — `GET /metrics` in the Prometheus text format.
//!
//! The signals worth exporting, from the implementation guide, and why each one
//! earns its place:
//!
//! | Metric                  | What it catches                                  |
//! |-------------------------|--------------------------------------------------|
//! | request latency         | user-visible regressions                          |
//! | DB query latency        | separates slow SQL from slow resolvers            |
//! | pool saturation         | the usual cause of a cliff under load             |
//! | errors by kind          | user error vs upstream failure                    |
//! | resolver latency        | N+1 relation storms — *not yet exported*          |
//! | query complexity        | whether limits are set sensibly — *not yet*       |
//! | active WebSockets       | subscription fan-out — *with Milestone 10*        |
//!
//! Rendered by hand rather than through a metrics crate: the format is a few
//! lines of text, and every value already lives in an atomic somewhere. Nothing
//! here carries a credential, a query string or a client address, so the
//! endpoint is safe to expose next to `/graphql`.
//!
//! Benchmarks should run against a realistic generated schema. A hello-world
//! GraphQL benchmark measures the HTTP stack, not this service — the cost here
//! is dominated by dynamic schema resolution and SQL execution, neither of which
//! a trivial schema exercises.

use std::fmt::Write as _;

use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use superquery_postgres::HistogramSnapshot;

use crate::app::AppState;

/// Prometheus text exposition format, version 0.0.4.
const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// `GET /metrics`.
pub async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, CONTENT_TYPE)], render(&state))
}

/// Render every metric for one scrape.
pub fn render(state: &AppState) -> String {
    let mut out = String::with_capacity(4096);

    write_gauge(
        &mut out,
        "superquery_build_info",
        "Query service build; the value is always 1.",
        &format!(
            "{{version=\"{}\",project=\"{}\"}}",
            env!("CARGO_PKG_VERSION"),
            state.db_schema
        ),
        1,
    );
    write_gauge(
        &mut out,
        "superquery_schema_entities",
        "Entities in the schema currently being served.",
        "",
        state.ir().len() as u64,
    );

    let db = state.db.stats();
    write_counter(
        &mut out,
        "superquery_db_queries_total",
        "Statements sent to PostgreSQL.",
        db.queries(),
    );
    write_counter(
        &mut out,
        "superquery_db_query_errors_total",
        "Statements that failed, including failures to get a pooled connection.",
        db.errors(),
    );
    write_histogram(
        &mut out,
        "superquery_db_query_duration_seconds",
        "Pool acquire plus execution time per statement.",
        &db.latency.snapshot(),
    );

    let pool = state.db.pool_status();
    write_gauge(
        &mut out,
        "superquery_db_pool_max",
        "Configured maximum pooled connections.",
        "",
        pool.max_size as u64,
    );
    write_gauge(
        &mut out,
        "superquery_db_pool_size",
        "Connections currently open.",
        "",
        pool.size as u64,
    );
    write_gauge(
        &mut out,
        "superquery_db_pool_available",
        "Open connections idle in the pool.",
        "",
        pool.available as u64,
    );
    write_gauge(
        &mut out,
        "superquery_db_pool_waiting",
        "Requests queued for a connection. Above zero means the pool is saturated.",
        "",
        pool.waiting as u64,
    );

    out
}

fn write_header(out: &mut String, name: &str, help: &str, kind: &str) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
}

fn write_gauge(out: &mut String, name: &str, help: &str, labels: &str, value: u64) {
    write_header(out, name, help, "gauge");
    let _ = writeln!(out, "{name}{labels} {value}");
}

fn write_counter(out: &mut String, name: &str, help: &str, value: u64) {
    write_header(out, name, help, "counter");
    let _ = writeln!(out, "{name} {value}");
}

/// A cumulative histogram: one `_bucket` line per bound, then the implicit
/// `+Inf` bucket, `_sum` and `_count`.
pub(crate) fn write_histogram(out: &mut String, name: &str, help: &str, h: &HistogramSnapshot) {
    write_header(out, name, help, "histogram");
    for (bound, count) in &h.buckets {
        let _ = writeln!(out, "{name}_bucket{{le=\"{bound}\"}} {count}");
    }
    let _ = writeln!(out, "{name}_bucket{{le=\"+Inf\"}} {}", h.count);
    let _ = writeln!(out, "{name}_sum {}", h.sum_seconds);
    let _ = writeln!(out, "{name}_count {}", h.count);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_renders_in_prometheus_shape() {
        let snapshot = HistogramSnapshot {
            buckets: vec![(0.01, 1), (0.1, 3)],
            count: 4,
            sum_seconds: 1.5,
        };
        let mut out = String::new();
        write_histogram(&mut out, "x_seconds", "help", &snapshot);

        assert!(out.contains("# TYPE x_seconds histogram\n"));
        assert!(out.contains("x_seconds_bucket{le=\"0.01\"} 1\n"));
        assert!(out.contains("x_seconds_bucket{le=\"0.1\"} 3\n"));
        // +Inf always equals the total count, even with slow outliers.
        assert!(out.contains("x_seconds_bucket{le=\"+Inf\"} 4\n"));
        assert!(out.contains("x_seconds_sum 1.5\n"));
        assert!(out.contains("x_seconds_count 4\n"));
    }

    #[test]
    fn gauges_and_counters_declare_their_type() {
        let mut out = String::new();
        write_gauge(&mut out, "g", "help", "", 2);
        write_counter(&mut out, "c_total", "help", 7);
        assert!(out.contains("# TYPE g gauge\ng 2\n"));
        assert!(out.contains("# TYPE c_total counter\nc_total 7\n"));
    }
}
