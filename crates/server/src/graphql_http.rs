//! GraphQL over HTTP, plus the GraphiQL playground.

use async_graphql::dynamic::Schema;
use async_graphql::http::GraphiQLSource;
use async_graphql::{BatchRequest, BatchResponse, Request, Response, ServerError};
use async_graphql_axum::{GraphQLBatchRequest, GraphQLResponse};
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use superquery_graphql::Limits;

use crate::app::AppState;

/// `POST /graphql` — a single query, or a JSON array of them.
///
/// The schema is cloned out of the lock before execution rather than held
/// across the await. That is what lets hot reload (Milestone 11) swap the schema
/// under a running server: in-flight requests keep executing against the schema
/// they started with, and only new requests see the replacement.
///
/// Document-level limits run first, on the raw query text, so a rejected
/// request never reaches validation, let alone the database.
pub async fn graphql_handler(
    State(state): State<AppState>,
    req: GraphQLBatchRequest,
) -> GraphQLResponse {
    let batch = req.into_inner();

    // Checked before anything runs: an oversized batch is refused whole rather
    // than half-executed.
    if let Some(max) = state.batch_limit {
        let size = batch.iter().count();
        if size > max {
            return reject(format!("batch contains {size} queries; the limit is {max}")).into();
        }
    }

    let schema = state.schema();
    match batch {
        BatchRequest::Single(req) => execute_one(&schema, &state.limits, req).await.into(),
        BatchRequest::Batch(reqs) => {
            // Sequential on purpose. Concurrent execution would let one HTTP
            // request hold `batch_limit` pool connections at once; in sequence
            // it costs what its queries would cost sent one by one, and the
            // request timeout still bounds the total.
            let mut responses = Vec::with_capacity(reqs.len());
            for req in reqs {
                responses.push(execute_one(&schema, &state.limits, req).await);
            }
            BatchResponse::Batch(responses).into()
        }
    }
}

async fn execute_one(schema: &Schema, limits: &Limits, req: Request) -> Response {
    if let Err(message) = limits.check_document(&req.query) {
        return reject(message);
    }
    schema.execute(req).await
}

fn reject(message: String) -> Response {
    Response::from_errors(vec![ServerError::new(message, None)])
}

/// `GET /` — the GraphiQL playground, when `--playground` is set.
pub async fn playground() -> impl IntoResponse {
    Html(
        GraphiQLSource::build()
            .endpoint("/graphql")
            .subscription_endpoint("/graphql")
            .title("SuperQuery")
            .finish(),
    )
}
