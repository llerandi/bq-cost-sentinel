use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::Value;

const DEFAULT_BIGQUERY_BASE_URL: &str = "https://bigquery.googleapis.com";

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BqDryRunResponse {
    pub total_bytes_processed: Option<String>,
}

#[derive(Clone)]
pub struct BqClient {
    http_client: Client,
    base_url: String,
}

impl BqClient {
    pub fn new() -> Self {
        Self {
            http_client: Client::new(),
            base_url: DEFAULT_BIGQUERY_BASE_URL.to_string(),
        }
    }

    /// Creates a client pointed at a custom base URL instead of the real
    /// BigQuery API. Used in tests to redirect requests to a local mock
    /// server (see the `tests` module below, and `server.rs`).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            http_client: Client::new(),
            base_url: base_url.into(),
        }
    }

    fn queries_url(&self, project_id: &str) -> String {
        format!(
            "{}/bigquery/v2/projects/{}/queries",
            self.base_url, project_id
        )
    }

    /// Shared HTTP logic for both the dry-run simulation and the real query
    /// execution: both hit the same BigQuery endpoint and only differ in
    /// the payload (dryRun flag) and how the caller interprets the result.
    async fn post_query(
        &self,
        project_id: &str,
        token: &str,
        payload: &Value,
        context: &str,
    ) -> Result<Value, String> {
        let url = self.queries_url(project_id);

        let response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", token))
            .json(payload)
            .send()
            .await
            .map_err(|e| format!("Network error while connecting to Google Cloud: {}", e))?;

        if response.status() != StatusCode::OK {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown Google Cloud error".to_string());
            return Err(format!("Google Cloud rejected {}: {}", context, error_body));
        }

        response
            .json()
            .await
            .map_err(|e| format!("Error reading Google Cloud JSON: {}", e))
    }

    pub async fn simulate_query(
        &self,
        project_id: &str,
        token: &str,
        mut payload: Value,
    ) -> Result<u64, String> {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("dryRun".to_string(), Value::Bool(true));
        }

        let response_json = self
            .post_query(project_id, token, &payload, "the simulation")
            .await?;

        let bq_response: BqDryRunResponse = serde_json::from_value(response_json)
            .map_err(|e| format!("Error reading Google Cloud JSON: {}", e))?;

        let bytes_str = bq_response
            .total_bytes_processed
            .ok_or("Google Cloud did not return the totalBytesProcessed field")?;

        bytes_str
            .parse::<u64>()
            .map_err(|_| "The totalBytesProcessed field is not a valid number".to_string())
    }

    pub async fn execute_query(
        &self,
        project_id: &str,
        token: &str,
        payload: &Value, // referencing to the original payload
    ) -> Result<Value, String> {
        self.post_query(project_id, token, payload, "the real query")
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn simulate_query_parses_total_bytes_processed() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/bigquery/v2/projects/my-project/queries"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "totalBytesProcessed": "123456789"
            })))
            .mount(&mock_server)
            .await;

        let client = BqClient::with_base_url(mock_server.uri());
        let bytes = client
            .simulate_query("my-project", "fake-token", json!({ "query": "SELECT 1" }))
            .await
            .expect("simulate_query should succeed");

        assert_eq!(bytes, 123_456_789);
    }

    #[tokio::test]
    async fn simulate_query_injects_dry_run_true_even_if_caller_did_not_set_it() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/bigquery/v2/projects/my-project/queries"))
            .and(body_json(json!({
                "query": "SELECT 1",
                "dryRun": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "totalBytesProcessed": "0"
            })))
            .mount(&mock_server)
            .await;

        let client = BqClient::with_base_url(mock_server.uri());
        let bytes = client
            .simulate_query("my-project", "fake-token", json!({ "query": "SELECT 1" }))
            .await
            .expect("simulate_query should succeed when dryRun was injected correctly");

        assert_eq!(bytes, 0);
    }

    #[tokio::test]
    async fn simulate_query_fails_when_total_bytes_processed_is_missing() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/bigquery/v2/projects/my-project/queries"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&mock_server)
            .await;

        let client = BqClient::with_base_url(mock_server.uri());
        let result = client
            .simulate_query("my-project", "fake-token", json!({ "query": "SELECT 1" }))
            .await;

        let err = result.expect_err("missing totalBytesProcessed should be an error");
        assert!(err.contains("totalBytesProcessed"));
    }

    #[tokio::test]
    async fn simulate_query_surfaces_google_cloud_errors() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/bigquery/v2/projects/my-project/queries"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad query syntax"))
            .mount(&mock_server)
            .await;

        let client = BqClient::with_base_url(mock_server.uri());
        let result = client
            .simulate_query("my-project", "fake-token", json!({ "query": "SELECT 1" }))
            .await;

        let err = result.expect_err("a non-200 response should be an error");
        assert!(err.contains("bad query syntax"));
    }

    #[tokio::test]
    async fn execute_query_returns_the_raw_json_response() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/bigquery/v2/projects/my-project/queries"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "rows": [{ "f": [{ "v": "42" }] }]
            })))
            .mount(&mock_server)
            .await;

        let client = BqClient::with_base_url(mock_server.uri());
        let result = client
            .execute_query("my-project", "fake-token", &json!({ "query": "SELECT 42" }))
            .await
            .expect("execute_query should succeed");

        assert_eq!(result["rows"][0]["f"][0]["v"], "42");
    }
}
