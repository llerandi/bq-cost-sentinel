use crate::auth::BearerToken;
use crate::pricing::{calculate_cost, exceeds_budget};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::Value;
use std::sync::Arc;
use tracing::{error, info, warn};

pub struct AppState {
    pub config: crate::config::AppConfig,
    pub google_client: crate::google_client::BqClient
}

/// Builds the application's Axum router. Extracted out of `main.rs` so it
/// can also be used directly in tests via `tower::ServiceExt::oneshot`,
/// without needing to bind a real TCP listener.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(|| async { "OK" }))
        .route(
            "/bigquery/v2/projects/:project_id/queries",
            post(proxy_query),
        )
        .with_state(state)
}

pub async fn proxy_query(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
    token: BearerToken,
    Json(payload): Json<Value>, 
) -> impl IntoResponse {
    
    info!(
        project_id = %project_id,
        "Request for a query sent to BigQuery was intercepted"
    );

    let payload_for_dryrun = payload.clone();

    match state.google_client.simulate_query(&project_id, &token.0, payload_for_dryrun).await {
        Ok(bytes) => {
            let query_cost = calculate_cost(bytes, state.config.price_per_tib);
            let is_too_expensive = exceeds_budget(query_cost, state.config.max_cost_per_query);

            if is_too_expensive {
                warn!(
                    project_id = %project_id,
                    estimated_cost = query_cost,
                    limite = state.config.max_cost_per_query,
                    "The query has exceeded the budget"
                );

                if state.config.enforce_mode {
                    return (
                        StatusCode::FORBIDDEN,
                        format!(
                            "Blocked by BigQuery Sentinel: The query costs ${:.2}, exceeding your limit of ${:.2}",
                            query_cost, state.config.max_cost_per_query
                        ),
                    ).into_response();
                }
            }

            info!(
                project_id = %project_id,
                estimated_cost = query_cost,
                "Query validated. Executing the original query in BigQuery..."
            );

            match state.google_client.execute_query(&project_id, &token.0, &payload).await {
                Ok(data_json) => {
                    info!("Query successfully executed. Sending data to the client...");
                    (StatusCode::OK, Json(data_json)).into_response()
                }
                Err(err) => {
                    error!("Error in actual execution: {}", err);
                    (StatusCode::INTERNAL_SERVER_ERROR, err).into_response()
                }
            }
        }
        Err(err) => {
            error!(
                project_id = %project_id,
                error = %err,
                "The simulation on Google Cloud has failed"
            );
            (
                StatusCode::BAD_REQUEST,
                format!("Error simulating the query in BigQuery: {}", err),
            ).into_response()
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::google_client::BqClient;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use serde_json::json;
    use tower::ServiceExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_config(enforce_mode: bool, max_cost_per_query: f64) -> AppConfig {
        AppConfig {
            port: 8080,
            max_cost_per_query,
            price_per_tib: 6.25,
            enforce_mode,
        }
    }

    /// Registers a single mock response that serves both the dry-run call
    /// (which only reads `totalBytesProcessed`) and, if the query is let
    /// through, the "real" execution call (which just forwards whatever
    /// JSON comes back to the client).
    async fn mock_bigquery_response(mock_server: &MockServer, total_bytes_processed: &str) {
        Mock::given(method("POST"))
            .and(path("/bigquery/v2/projects/test-project/queries"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "totalBytesProcessed": total_bytes_processed,
                "rows": []
            })))
            .mount(mock_server)
            .await;
    }

    fn request_with_body(body: &str, with_auth: bool) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/bigquery/v2/projects/test-project/queries")
            .header("Content-Type", "application/json");

        if with_auth {
            builder = builder.header("Authorization", "Bearer fake-token");
        }

        builder.body(Body::from(body.to_string())).unwrap()
    }

    #[tokio::test]
    async fn rejects_requests_without_a_bearer_token() {
        let mock_server = MockServer::start().await;
        let state = Arc::new(AppState {
            config: test_config(true, 5.0),
            google_client: BqClient::with_base_url(mock_server.uri()),
        });
        let app = build_router(state);

        let response = app
            .oneshot(request_with_body(r#"{"query":"SELECT 1"}"#, false))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn blocks_an_expensive_query_when_enforce_mode_is_on() {
        let mock_server = MockServer::start().await;
        // 2 TiB scanned at $6.25/TiB (~$12.50) is well above the $5 budget.
        mock_bigquery_response(&mock_server, "2199023255552").await;

        let state = Arc::new(AppState {
            config: test_config(true, 5.0),
            google_client: BqClient::with_base_url(mock_server.uri()),
        });
        let app = build_router(state);

        let response = app
            .oneshot(request_with_body(r#"{"query":"SELECT 1"}"#, true))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let body_bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_text = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert!(body_text.contains("Blocked by BigQuery Sentinel"));
    }

    #[tokio::test]
    async fn logs_but_allows_an_expensive_query_when_enforce_mode_is_off() {
        let mock_server = MockServer::start().await;
        mock_bigquery_response(&mock_server, "2199023255552").await;

        let state = Arc::new(AppState {
            config: test_config(false, 5.0),
            google_client: BqClient::with_base_url(mock_server.uri()),
        });
        let app = build_router(state);

        let response = app
            .oneshot(request_with_body(r#"{"query":"SELECT 1"}"#, true))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn allows_a_cheap_query_through() {
        let mock_server = MockServer::start().await;
        mock_bigquery_response(&mock_server, "4096").await;

        let state = Arc::new(AppState {
            config: test_config(true, 5.0),
            google_client: BqClient::with_base_url(mock_server.uri()),
        });
        let app = build_router(state);

        let response = app
            .oneshot(request_with_body(r#"{"query":"SELECT 1"}"#, true))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn returns_bad_request_when_the_dry_run_simulation_fails() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/bigquery/v2/projects/test-project/queries"))
            .respond_with(ResponseTemplate::new(400).set_body_string("invalid query"))
            .mount(&mock_server)
            .await;

        let state = Arc::new(AppState {
            config: test_config(true, 5.0),
            google_client: BqClient::with_base_url(mock_server.uri()),
        });
        let app = build_router(state);

        let response = app
            .oneshot(request_with_body(r#"{"query":"SELECT 1"}"#, true))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}