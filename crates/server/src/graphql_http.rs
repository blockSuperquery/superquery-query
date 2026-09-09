//! GraphQL over HTTP, plus the GraphiQL playground.

use async_graphql::http::GraphiQLSource;
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::extract::State;
use axum::response::{Html, IntoResponse};

use crate::app::AppState;

/// `POST /graphql`.
///
/// The schema is cloned out of the lock before execution rather than held
/// across the await. That is what lets hot reload (Milestone 11) swap the schema
/// under a running server: in-flight requests keep executing against the schema
/// they started with, and only new requests see the replacement.
pub async fn graphql_handler(
    State(state): State<AppState>,
    req: GraphQLRequest,
) -> GraphQLResponse {
    let schema = state.schema();
    schema.execute(req.into_inner()).await.into()
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
