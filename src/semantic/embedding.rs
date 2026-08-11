use std::env;

use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{extract::TextSegment, SemanticError};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EmbeddingModelRef {
    pub provider: String,
    pub model: String,
    pub version: String,
    pub dimensions: usize,
    pub normalization: String,
}

impl EmbeddingModelRef {
    pub(crate) fn fingerprint(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.provider, self.model, self.version, self.dimensions, self.normalization
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct EmbeddedSegment {
    pub kind: super::extract::SegmentKind,
    pub text: String,
    pub weight: f32,
    pub ordinal: usize,
    pub vector: Vec<f32>,
}

#[derive(Clone)]
pub(crate) enum Embedder {
    DevelopmentHash {
        model: EmbeddingModelRef,
    },
    Remote {
        client: reqwest::Client,
        endpoint: Url,
        api_key: Option<String>,
        model: EmbeddingModelRef,
    },
}

impl Embedder {
    pub(crate) fn from_env() -> Result<Self, SemanticError> {
        let endpoint = env::var("EMBEDDING_ENDPOINT")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let Some(endpoint) = endpoint else {
            return Ok(Self::development_hash());
        };

        let endpoint = Url::parse(endpoint.trim()).map_err(|error| {
            SemanticError::configuration(format!("invalid EMBEDDING_ENDPOINT: {error}"))
        })?;
        if endpoint.scheme() != "https"
            && !(cfg!(test)
                && endpoint.scheme() == "http"
                && matches!(endpoint.host_str(), Some("127.0.0.1" | "localhost")))
        {
            return Err(SemanticError::configuration(
                "EMBEDDING_ENDPOINT must use https outside local tests",
            ));
        }

        let model = EmbeddingModelRef {
            provider: required_env("EMBEDDING_PROVIDER")?,
            model: required_env("EMBEDDING_MODEL")?,
            version: required_env("EMBEDDING_MODEL_VERSION")?,
            dimensions: required_env("EMBEDDING_DIMENSIONS")?
                .parse::<usize>()
                .map_err(|error| {
                    SemanticError::configuration(format!(
                        "EMBEDDING_DIMENSIONS must be an integer: {error}"
                    ))
                })?,
            normalization: "l2".into(),
        };
        validate_dimensions(model.dimensions)?;

        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(45))
            .user_agent("embedded-alerts-embedding-client/0.1")
            .build()
            .map_err(|error| {
                SemanticError::configuration(format!("build embedding client: {error}"))
            })?;

        Ok(Self::Remote {
            client,
            endpoint,
            api_key: env::var("EMBEDDING_API_KEY")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            model,
        })
    }

    pub(crate) fn development_hash() -> Self {
        Self::DevelopmentHash {
            model: EmbeddingModelRef {
                provider: "embedded-alerts".into(),
                model: "feature-hash".into(),
                version: "development-v1".into(),
                dimensions: 384,
                normalization: "l2".into(),
            },
        }
    }

    pub(crate) fn model(&self) -> &EmbeddingModelRef {
        match self {
            Self::DevelopmentHash { model } | Self::Remote { model, .. } => model,
        }
    }

    pub(crate) fn mode(&self) -> &'static str {
        match self {
            Self::DevelopmentHash { .. } => "development_feature_hash",
            Self::Remote { .. } => "remote_semantic_model",
        }
    }

    pub(crate) async fn embed_segments(
        &self,
        segments: &[TextSegment],
    ) -> Result<Vec<EmbeddedSegment>, SemanticError> {
        if segments.is_empty() {
            return Err(SemanticError::invalid(
                "embedding_inputs",
                "at least one embedding input is required",
            ));
        }
        if segments.len() > 128 {
            return Err(SemanticError::invalid(
                "embedding_inputs",
                "at most 128 embedding inputs may be submitted per request",
            ));
        }
        let vectors = match self {
            Self::DevelopmentHash { model } => segments
                .iter()
                .map(|segment| feature_hash_embedding(&segment.text, model.dimensions))
                .collect::<Result<Vec<_>, _>>()?,
            Self::Remote {
                client,
                endpoint,
                api_key,
                model,
            } => remote_embeddings(client, endpoint, api_key.as_deref(), model, segments).await?,
        };

        Ok(segments
            .iter()
            .zip(vectors)
            .map(|(segment, vector)| EmbeddedSegment {
                kind: segment.kind,
                text: segment.text.clone(),
                weight: segment.weight,
                ordinal: segment.ordinal,
                vector,
            })
            .collect())
    }
}

#[derive(Serialize)]
struct RemoteEmbeddingRequest<'a> {
    model: &'a str,
    input: Vec<&'a str>,
    dimensions: usize,
    encoding_format: &'static str,
}

#[derive(Deserialize)]
struct RemoteEmbeddingResponse {
    data: Vec<RemoteEmbeddingDatum>,
    #[serde(default, rename = "model")]
    _model: Option<String>,
}

#[derive(Deserialize)]
struct RemoteEmbeddingDatum {
    index: usize,
    embedding: Vec<f32>,
}

async fn remote_embeddings(
    client: &reqwest::Client,
    endpoint: &Url,
    api_key: Option<&str>,
    model: &EmbeddingModelRef,
    segments: &[TextSegment],
) -> Result<Vec<Vec<f32>>, SemanticError> {
    let request = RemoteEmbeddingRequest {
        model: &model.model,
        input: segments.iter().map(|segment| segment.text.as_str()).collect(),
        dimensions: model.dimensions,
        encoding_format: "float",
    };
    let mut builder = client.post(endpoint.clone()).json(&request);
    if let Some(api_key) = api_key {
        builder = builder.bearer_auth(api_key);
    }
    let response = builder
        .send()
        .await
        .map_err(|error| SemanticError::provider(format!("embedding request failed: {error}")))?;
    let response = response.error_for_status().map_err(|error| {
        SemanticError::provider(format!("embedding provider rejected request: {error}"))
    })?;
    let response: RemoteEmbeddingResponse = response.json().await.map_err(|error| {
        SemanticError::provider(format!("decode embedding provider response: {error}"))
    })?;
    if response.data.len() != segments.len() {
        return Err(SemanticError::provider(format!(
            "embedding provider returned {} vectors for {} inputs",
            response.data.len(),
            segments.len()
        )));
    }

    let mut ordered: Vec<Option<Vec<f32>>> = vec![None; segments.len()];
    for datum in response.data {
        if datum.index >= ordered.len() || ordered[datum.index].is_some() {
            return Err(SemanticError::provider(
                "embedding provider returned invalid or duplicate indexes",
            ));
        }
        ordered[datum.index] = Some(normalize_vector(datum.embedding, model.dimensions)?);
    }
    ordered
        .into_iter()
        .map(|vector| {
            vector.ok_or_else(|| SemanticError::provider("embedding provider omitted an index"))
        })
        .collect()
}

pub(crate) fn cosine_similarity(left: &[f32], right: &[f32]) -> Result<f32, SemanticError> {
    if left.len() != right.len() || left.is_empty() {
        return Err(SemanticError::conflict(
            "embedding_dimensions",
            "embedding vectors must have identical non-zero dimensions",
        ));
    }
    if left.iter().chain(right).any(|value| !value.is_finite()) {
        return Err(SemanticError::invalid(
            "embedding_values",
            "embedding vectors must contain only finite values",
        ));
    }
    let dot = left
        .iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum::<f32>();
    Ok(dot.clamp(-1.0, 1.0))
}

fn feature_hash_embedding(text: &str, dimensions: usize) -> Result<Vec<f32>, SemanticError> {
    validate_dimensions(dimensions)?;
    let tokens = feature_tokens(text);
    if tokens.is_empty() {
        return Err(SemanticError::invalid(
            "embedding_text",
            "embedding input must contain at least one alphanumeric token",
        ));
    }
    let mut vector = vec![0.0_f32; dimensions];
    for feature in &tokens {
        add_feature(&mut vector, feature, 1.0);
    }
    for window in tokens.windows(2) {
        let bigram = format!("{}::{}", window[0], window[1]);
        add_feature(&mut vector, &bigram, 0.65);
    }
    normalize_vector(vector, dimensions)
}

fn add_feature(vector: &mut [f32], feature: &str, weight: f32) {
    let digest = Sha256::digest(feature.as_bytes());
    let mut index_bytes = [0_u8; 8];
    index_bytes.copy_from_slice(&digest[..8]);
    let index = (u64::from_be_bytes(index_bytes) as usize) % vector.len();
    let sign = if digest[8] & 1 == 0 { 1.0 } else { -1.0 };
    vector[index] += weight * sign;
}

fn feature_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() {
            current.extend(character.to_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn normalize_vector(mut vector: Vec<f32>, dimensions: usize) -> Result<Vec<f32>, SemanticError> {
    validate_dimensions(dimensions)?;
    if vector.len() != dimensions {
        return Err(SemanticError::provider(format!(
            "embedding dimensions mismatch: expected {dimensions}, received {}",
            vector.len()
        )));
    }
    if vector.iter().any(|value| !value.is_finite()) {
        return Err(SemanticError::provider(
            "embedding provider returned a non-finite value",
        ));
    }
    let norm = vector
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt();
    if !norm.is_finite() || norm <= f32::EPSILON {
        return Err(SemanticError::provider(
            "embedding provider returned a zero-length vector",
        ));
    }
    for value in &mut vector {
        *value /= norm;
    }
    Ok(vector)
}

fn validate_dimensions(dimensions: usize) -> Result<(), SemanticError> {
    if !(8..=16_384).contains(&dimensions) {
        return Err(SemanticError::configuration(
            "embedding dimensions must be between 8 and 16,384",
        ));
    }
    Ok(())
}

fn required_env(name: &str) -> Result<String, SemanticError> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_owned())
        .ok_or_else(|| SemanticError::configuration(format!("{name} is required")))
}

pub(crate) fn sha256_hex(value: impl AsRef<[u8]>) -> String {
    let digest = Sha256::digest(value.as_ref());
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::extract::{SegmentKind, TextSegment};

    #[tokio::test]
    async fn development_embeddings_are_deterministic_and_normalized() {
        let embedder = Embedder::development_hash();
        let segments = vec![TextSegment {
            kind: SegmentKind::Query,
            text: "renewable energy monitoring".into(),
            weight: 1.0,
            ordinal: 0,
        }];
        let first = embedder.embed_segments(&segments).await.unwrap();
        let second = embedder.embed_segments(&segments).await.unwrap();
        assert_eq!(first[0].vector, second[0].vector);
        let norm = first[0]
            .vector
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        assert!((norm - 1.0).abs() < 0.0001);
    }

    #[test]
    fn cosine_rejects_dimension_mismatch() {
        let error = cosine_similarity(&[1.0, 0.0], &[1.0]).unwrap_err();
        assert_eq!(error.code(), "embedding_dimensions");
    }

    #[test]
    fn model_fingerprint_includes_version_and_dimensions() {
        let model = EmbeddingModelRef {
            provider: "provider".into(),
            model: "model".into(),
            version: "2026-08-01".into(),
            dimensions: 768,
            normalization: "l2".into(),
        };
        assert_eq!(
            model.fingerprint(),
            "provider:model:2026-08-01:768:l2"
        );
    }
}
