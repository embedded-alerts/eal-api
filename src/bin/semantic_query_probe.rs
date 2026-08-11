//! Operator-only semantic query probe.
//!
//! This binary certifies query decomposition and the configured embedding provider
//! without exposing raw vectors or creating delivery side effects.

#[path = "../query_embedding.rs"]
mod query_embedding;

use eal_interfaces::EmbeddingSpaceConfig;
use eal_query::analyze_query;
use query_embedding::QueryEmbeddingService;
use serde_json::json;
use std::{env, error::Error};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let query = required_env("EAL_QUERY_TEXT")?;
    let embedding_space_json = required_env("EAL_EMBEDDING_SPACE_JSON")?;
    let embedding_space: EmbeddingSpaceConfig = serde_json::from_str(&embedding_space_json)?;
    let environment = env::var("APP_ENV").unwrap_or_else(|_| "development".into());

    let views = analyze_query(&query)?;
    let service = QueryEmbeddingService::from_env(&embedding_space, &environment)?
        .ok_or("EAL_EMBEDDING_ENDPOINT is required for semantic_query_probe")?;
    let vector = service.embed_inputs(&views.embedding_inputs).await?;
    let norm = vector
        .iter()
        .map(|value| f64::from(*value).powi(2))
        .sum::<f64>()
        .sqrt();

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "query": views.query_text,
            "keywords": views.keywords,
            "entities": views.entities,
            "embedding_inputs": views.embedding_inputs.iter().map(|input| json!({
                "kind": input.kind.wire_name(),
                "ordinal": input.ordinal,
                "characters": input.text.chars().count(),
                "weight": input.weight,
            })).collect::<Vec<_>>(),
            "vector": {
                "dimensions": vector.len(),
                "l2_norm": norm,
                "values_exposed": false,
            }
        }))?
    );
    Ok(())
}

fn required_env(name: &str) -> Result<String, Box<dyn Error>> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{name} is required").into())
}
