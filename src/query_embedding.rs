use eal_interfaces::EmbeddingSpaceConfig;
use eal_semantic_contracts::EmbeddingInput;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::{env, error::Error, fmt, time::Duration};
use url::Url;

const MAX_PROVIDER_ERROR_CHARS: usize = 500;

#[derive(Clone)]
pub struct QueryEmbeddingService {
    client: Client,
    endpoint: Url,
    api_key: Option<String>,
    model: String,
    dimensions: usize,
}

impl QueryEmbeddingService {
    pub fn from_env(
        embedding_space: &EmbeddingSpaceConfig,
        environment: &str,
    ) -> Result<Option<Self>, QueryEmbeddingError> {
        let Some(endpoint_value) = nonempty_env("EAL_EMBEDDING_ENDPOINT") else {
            return Ok(None);
        };
        let endpoint = Url::parse(&endpoint_value).map_err(|error| {
            QueryEmbeddingError::new(
                "invalid_embedding_endpoint",
                format!("EAL_EMBEDDING_ENDPOINT is invalid: {error}"),
                false,
            )
        })?;
        validate_endpoint(&endpoint, environment)?;
        let client = Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(25))
            .user_agent("embedded-alerts/eal-api-query-embedding")
            .build()
            .map_err(|error| {
                QueryEmbeddingError::new(
                    "embedding_client_build_failed",
                    format!("could not build embedding HTTP client: {error}"),
                    false,
                )
            })?;
        Ok(Some(Self {
            client,
            endpoint,
            api_key: nonempty_env("EAL_EMBEDDING_API_KEY"),
            model: embedding_space.model.clone(),
            dimensions: embedding_space.dimensions as usize,
        }))
    }

    pub async fn embed_inputs(
        &self,
        inputs: &[EmbeddingInput],
    ) -> Result<Vec<f32>, QueryEmbeddingError> {
        if inputs.is_empty() {
            return Err(QueryEmbeddingError::new(
                "empty_embedding_inputs",
                "at least one query embedding input is required",
                false,
            ));
        }
        for input in inputs {
            input.validate().map_err(|error| {
                QueryEmbeddingError::new(error.code, error.message, false)
            })?;
        }

        let payload = ProviderRequest {
            model: &self.model,
            input: inputs.iter().map(|input| input.text.as_str()).collect(),
            dimensions: self.dimensions,
            encoding_format: "float",
        };
        let mut request = self.client.post(self.endpoint.clone()).json(&payload);
        if let Some(api_key) = self.api_key.as_deref() {
            request = request.bearer_auth(api_key);
        }
        let response = request.send().await.map_err(|error| {
            QueryEmbeddingError::new(
                "embedding_provider_unreachable",
                format!("embedding provider request failed: {error}"),
                error.is_timeout() || error.is_connect(),
            )
        })?;
        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(QueryEmbeddingError::new(
                provider_error_code(status),
                format!(
                    "embedding provider returned HTTP {}: {}",
                    status.as_u16(),
                    truncate(&message, MAX_PROVIDER_ERROR_CHARS)
                ),
                status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error(),
            ));
        }
        let provider_response = response.json::<ProviderResponse>().await.map_err(|error| {
            QueryEmbeddingError::new(
                "invalid_embedding_provider_response",
                format!("embedding provider response was invalid JSON: {error}"),
                false,
            )
        })?;
        validate_provider_model(provider_response.model.as_deref(), &self.model)?;

        let mut data = provider_response.data;
        data.sort_by_key(|item| item.index);
        if data.len() != inputs.len() {
            return Err(QueryEmbeddingError::new(
                "embedding_count_mismatch",
                format!(
                    "embedding provider returned {} vectors for {} inputs",
                    data.len(),
                    inputs.len()
                ),
                false,
            ));
        }
        let vectors = data
            .into_iter()
            .enumerate()
            .map(|(expected_index, item)| {
                if item.index != expected_index {
                    return Err(QueryEmbeddingError::new(
                        "embedding_index_mismatch",
                        "embedding provider returned non-contiguous indices",
                        false,
                    ));
                }
                validate_vector(item.embedding, self.dimensions)
            })
            .collect::<Result<Vec<_>, _>>()?;
        weighted_centroid(inputs, &vectors, self.dimensions)
    }
}

#[derive(Debug, Serialize)]
struct ProviderRequest<'a> {
    model: &'a str,
    input: Vec<&'a str>,
    dimensions: usize,
    encoding_format: &'static str,
}

#[derive(Debug, Deserialize)]
struct ProviderResponse {
    data: Vec<ProviderEmbedding>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProviderEmbedding {
    index: usize,
    embedding: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryEmbeddingError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl QueryEmbeddingError {
    fn new(code: &'static str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
        }
    }
}

impl fmt::Display for QueryEmbeddingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for QueryEmbeddingError {}

fn validate_endpoint(endpoint: &Url, environment: &str) -> Result<(), QueryEmbeddingError> {
    if endpoint.username() != ""
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(QueryEmbeddingError::new(
            "invalid_embedding_endpoint",
            "embedding endpoint must not contain credentials, a query string, or a fragment",
            false,
        ));
    }
    let secure = endpoint.scheme() == "https";
    let development_loopback = environment != "production"
        && endpoint.scheme() == "http"
        && matches!(
            endpoint.host_str(),
            Some("localhost" | "127.0.0.1" | "::1")
        );
    if !secure && !development_loopback {
        return Err(QueryEmbeddingError::new(
            "insecure_embedding_endpoint",
            "embedding endpoint must use HTTPS; development may use HTTP loopback only",
            false,
        ));
    }
    Ok(())
}

fn validate_provider_model(
    returned_model: Option<&str>,
    configured_model: &str,
) -> Result<(), QueryEmbeddingError> {
    if let Some(returned_model) = returned_model
        && returned_model != configured_model
    {
        return Err(QueryEmbeddingError::new(
            "embedding_model_mismatch",
            format!(
                "embedding provider returned model {returned_model:?}; expected {configured_model:?}"
            ),
            false,
        ));
    }
    Ok(())
}

fn validate_vector(
    vector: Vec<f32>,
    expected_dimensions: usize,
) -> Result<Vec<f32>, QueryEmbeddingError> {
    if vector.len() != expected_dimensions {
        return Err(QueryEmbeddingError::new(
            "embedding_dimension_mismatch",
            format!(
                "embedding provider returned {} dimensions; expected {expected_dimensions}",
                vector.len()
            ),
            false,
        ));
    }
    if vector.iter().any(|value| !value.is_finite()) {
        return Err(QueryEmbeddingError::new(
            "invalid_embedding_values",
            "embedding provider returned non-finite values",
            false,
        ));
    }
    let magnitude = vector
        .iter()
        .map(|value| f64::from(*value).powi(2))
        .sum::<f64>()
        .sqrt();
    if magnitude <= f64::EPSILON {
        return Err(QueryEmbeddingError::new(
            "zero_embedding_vector",
            "embedding provider returned a zero vector",
            false,
        ));
    }
    Ok(vector)
}

fn weighted_centroid(
    inputs: &[EmbeddingInput],
    vectors: &[Vec<f32>],
    dimensions: usize,
) -> Result<Vec<f32>, QueryEmbeddingError> {
    if inputs.len() != vectors.len() || dimensions == 0 {
        return Err(QueryEmbeddingError::new(
            "invalid_embedding_aggregation",
            "embedding inputs and vectors must have matching non-zero dimensions",
            false,
        ));
    }
    let mut aggregate = vec![0.0_f64; dimensions];
    let mut total_weight = 0.0_f64;
    for (input, vector) in inputs.iter().zip(vectors) {
        if vector.len() != dimensions {
            return Err(QueryEmbeddingError::new(
                "embedding_dimension_mismatch",
                "embedding vectors did not share one dimension",
                false,
            ));
        }
        let magnitude = vector
            .iter()
            .map(|value| f64::from(*value).powi(2))
            .sum::<f64>()
            .sqrt();
        if magnitude <= f64::EPSILON {
            return Err(QueryEmbeddingError::new(
                "zero_embedding_vector",
                "embedding provider returned a zero vector",
                false,
            ));
        }
        let weight = f64::from(input.weight);
        for (index, value) in vector.iter().enumerate() {
            aggregate[index] += f64::from(*value) / magnitude * weight;
        }
        total_weight += weight;
    }
    if total_weight <= f64::EPSILON {
        return Err(QueryEmbeddingError::new(
            "invalid_embedding_weight",
            "query embedding weights must have a positive sum",
            false,
        ));
    }
    for value in &mut aggregate {
        *value /= total_weight;
    }
    let magnitude = aggregate
        .iter()
        .map(|value| value.powi(2))
        .sum::<f64>()
        .sqrt();
    if magnitude <= f64::EPSILON {
        return Err(QueryEmbeddingError::new(
            "zero_embedding_centroid",
            "weighted query embedding collapsed to a zero vector",
            false,
        ));
    }
    Ok(aggregate
        .into_iter()
        .map(|value| (value / magnitude) as f32)
        .collect())
}

fn provider_error_code(status: StatusCode) -> &'static str {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => "embedding_provider_auth_failed",
        StatusCode::TOO_MANY_REQUESTS => "embedding_provider_throttled",
        status if status.is_server_error() => "embedding_provider_unavailable",
        _ => "embedding_provider_rejected_request",
    }
}

fn nonempty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn truncate(value: &str, max_characters: usize) -> String {
    let mut characters = value.chars();
    let mut output = characters.by_ref().take(max_characters).collect::<String>();
    if characters.next().is_some() {
        output.push('…');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use eal_semantic_contracts::EmbeddingInputKind;

    fn input(weight: f32, ordinal: u16) -> EmbeddingInput {
        EmbeddingInput {
            kind: EmbeddingInputKind::Query,
            ordinal,
            text: "renewable energy launch".into(),
            weight,
        }
    }

    #[test]
    fn weighted_centroid_is_normalized() {
        let result = weighted_centroid(
            &[input(1.0, 0), input(0.5, 1)],
            &[vec![1.0, 0.0], vec![0.0, 1.0]],
            2,
        )
        .unwrap();
        let magnitude = result
            .iter()
            .map(|value| f64::from(*value).powi(2))
            .sum::<f64>()
            .sqrt();
        assert!((magnitude - 1.0).abs() < 1e-6);
        assert!(result[0] > result[1]);
    }

    #[test]
    fn production_rejects_plain_http_endpoints() {
        let endpoint = Url::parse("http://embedding.example/v1/embeddings").unwrap();
        assert!(validate_endpoint(&endpoint, "production").is_err());
    }

    #[test]
    fn development_allows_loopback_http() {
        let endpoint = Url::parse("http://127.0.0.1:8081/v1/embeddings").unwrap();
        validate_endpoint(&endpoint, "development").unwrap();
    }

    #[test]
    fn vectors_must_match_configured_dimensions() {
        assert!(validate_vector(vec![1.0, 2.0], 3).is_err());
    }
}
