//! End-to-end against a real PostgreSQL (guide §23, the query-side half).
//!
//! `#[ignore]`d so `cargo test` stays runnable with no infrastructure; CI's
//! `integration` job runs it with `-- --ignored` against a service container.
//! Locally:
//!
//! ```bash
//! docker compose -f deploy/docker-compose.yml up -d postgres
//! cargo test -p superquery-server --test integration -- --ignored
//! ```
//!
//! The fixture is loaded through its own read-write connection; the service
//! under test only ever gets its normal read-only pool.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use clap::Parser;
use serde_json::{json, Value};
use superquery_server::{build_state, router, AppState, Config};
use tower::ServiceExt;

const PROJECT: &str = "itest_query";

const SDL: &str = r#"
    type Account @entity {
      id: ID!
      transfers: [Transfer!]! @derivedFrom(field: "fromAccount")
    }

    type Transfer @entity {
      id: ID!
      from: String!
      value: BigInt!
      blockNumber: Int!
      data: Bytes
      fromAccount: Account!
    }
"#;

const MAX_U256: &str =
    "115792089237316195423570985008687907853269984665640564039457584007913129639935";

async fn load_fixture() {
    let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    let conn_str = format!(
        "host={} port={} user={} password={} dbname={}",
        env("DB_HOST", "127.0.0.1"),
        env("DB_PORT", "5432"),
        env("DB_USER", "postgres"),
        env("DB_PASS", "postgres"),
        env("DB_DATABASE", "postgres"),
    );
    let (client, connection) = tokio_postgres::connect(&conn_str, tokio_postgres::NoTls)
        .await
        .expect("fixture connection — is PostgreSQL running?");
    tokio::spawn(connection);
    client
        .batch_execute(include_str!("fixtures/erc20.sql"))
        .await
        .expect("fixture loads");
}

async fn start() -> (AppState, Router) {
    load_fixture().await;

    let schema_path =
        std::env::temp_dir().join(format!("{PROJECT}-{}.graphql", std::process::id()));
    std::fs::write(&schema_path, SDL).unwrap();

    let config = Config::try_parse_from([
        "superquery-query",
        "--name",
        PROJECT,
        "--schema",
        schema_path.to_str().unwrap(),
        "--disable-hot-schema",
    ])
    .unwrap();

    let state = build_state(&config)
        .await
        .expect("service starts against the fixture");
    let app = router(state.clone(), &config);
    (state, app)
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, bytes.to_vec())
}

async fn gql(app: &Router, query: &str) -> Value {
    let req = Request::builder()
        .method("POST")
        .uri("/graphql")
        .header("content-type", "application/json")
        .body(Body::from(json!({ "query": query }).to_string()))
        .unwrap();
    let (_, body) = send(app, req).await;
    serde_json::from_slice(&body).unwrap()
}

async fn get(app: &Router, path: &str) -> (StatusCode, String) {
    let req = Request::builder().uri(path).body(Body::empty()).unwrap();
    let (status, body) = send(app, req).await;
    (status, String::from_utf8(body).unwrap())
}

fn ids(nodes: &Value) -> Vec<String> {
    nodes
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap().to_string())
        .collect()
}

// One test, run in sequence: every section shares the fixture schema, and
// parallel tests would race to drop and recreate it.
#[tokio::test]
#[ignore = "needs PostgreSQL; run with --ignored"]
async fn end_to_end_against_postgres() {
    let (state, app) = start().await;

    // --- readiness and metadata -------------------------------------------
    let (status, body) = get(&app, "/ready").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"indexedHeight\":120"), "{body}");

    // --- by id: precision and encoding -------------------------------------
    let r = gql(&app, r#"{ transfer(id: "t1") { value data blockNumber } }"#).await;
    assert_eq!(r["data"]["transfer"]["value"], json!(MAX_U256), "{r}");
    assert_eq!(r["data"]["transfer"]["data"], json!("0xdeadbeef"), "{r}");
    assert_eq!(r["data"]["transfer"]["blockNumber"], json!(100), "{r}");

    let r = gql(&app, r#"{ transfer(id: "missing") { id } }"#).await;
    assert_eq!(
        r["data"]["transfer"],
        Value::Null,
        "unknown id is null: {r}"
    );

    // --- filters, including a hostile value --------------------------------
    let r = gql(
        &app,
        r#"{ transfers(filter: { from: { equalTo: "0xaaa" } }) { totalCount nodes { id } } }"#,
    )
    .await;
    assert_eq!(ids(&r["data"]["transfers"]["nodes"]), ["t1", "t2"], "{r}");
    assert_eq!(r["data"]["transfers"]["totalCount"], json!(2), "{r}");

    let r = gql(
        &app,
        r#"{ transfers(filter: { from: { equalTo: "x'; DROP TABLE transfers; --" } }) { totalCount } }"#,
    )
    .await;
    assert_eq!(r["data"]["transfers"]["totalCount"], json!(0), "{r}");
    let r = gql(&app, "{ transfers { totalCount } }").await;
    assert_eq!(
        r["data"]["transfers"]["totalCount"],
        json!(5),
        "table intact: {r}"
    );

    // --- numeric ordering, not lexicographic -------------------------------
    let r = gql(
        &app,
        "{ transfers(orderBy: [VALUE_DESC], first: 2) { nodes { id } } }",
    )
    .await;
    assert_eq!(ids(&r["data"]["transfers"]["nodes"]), ["t1", "t2"], "{r}");

    // --- cursor pagination across a tied sort key --------------------------
    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let after_arg = after
            .as_ref()
            .map(|c| format!(", after: \"{c}\""))
            .unwrap_or_default();
        let r = gql(
            &app,
            &format!(
                "{{ transfers(orderBy: [BLOCK_NUMBER_ASC], first: 2{after_arg}) \
                 {{ nodes {{ id }} pageInfo {{ hasNextPage endCursor }} }} }}"
            ),
        )
        .await;
        let page = &r["data"]["transfers"];
        seen.extend(ids(&page["nodes"]));
        if !page["pageInfo"]["hasNextPage"].as_bool().unwrap() {
            break;
        }
        after = Some(page["pageInfo"]["endCursor"].as_str().unwrap().to_string());
    }
    assert_eq!(seen, ["t1", "t2", "t3", "t4", "t5"], "no repeats, no gaps");

    let r = gql(
        &app,
        r#"{ transfers(after: "not-a-cursor") { nodes { id } } }"#,
    )
    .await;
    assert!(r["errors"][0]["message"]
        .as_str()
        .unwrap()
        .contains("invalid cursor"));

    // --- forward relation: N parents, 2 statements --------------------------
    let before = state.db.stats().queries();
    let r = gql(&app, "{ transfers { nodes { id fromAccount { id } } } }").await;
    let statements = state.db.stats().queries() - before;
    let owners: Vec<_> = r["data"]["transfers"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["fromAccount"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(owners, ["a", "a", "b", "b", "a"], "{r}");
    assert_eq!(statements, 2, "one page + one batched lookup, not 1 + 5");

    // --- reverse relation: batched, per-parent, empty list for none ---------
    let before = state.db.stats().queries();
    let r = gql(&app, "{ accounts { nodes { id transfers { id } } } }").await;
    let statements = state.db.stats().queries() - before;
    let accounts = r["data"]["accounts"]["nodes"].as_array().unwrap();
    assert_eq!(ids(&accounts[0]["transfers"]), ["t1", "t2", "t5"], "{r}");
    assert_eq!(ids(&accounts[1]["transfers"]), ["t3", "t4"], "{r}");
    assert_eq!(accounts[2]["transfers"], json!([]), "{r}");
    assert_eq!(statements, 2, "one page + one batched reverse lookup");

    // --- limits are enforced before any database work ------------------------
    let flood: String = (0..60)
        .map(|i| format!("a{i}: _meta {{ chain }} "))
        .collect();
    let before = state.db.stats().queries();
    let r = gql(&app, &format!("{{ {flood} }}")).await;
    assert!(
        r["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("aliases"),
        "{r}"
    );
    assert_eq!(
        state.db.stats().queries(),
        before,
        "rejected without touching Postgres"
    );

    // --- metrics ------------------------------------------------------------
    let (status, body) = get(&app, "/metrics").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("superquery_db_queries_total"), "{body}");
    assert!(
        body.contains("superquery_graphql_requests_total{class=\"2xx\"}"),
        "{body}"
    );
}
