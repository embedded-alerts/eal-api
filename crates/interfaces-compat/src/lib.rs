pub use eal_interfaces_upstream::*;

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingSpaceConfig {
    pub provider: String,
    pub model: String,
    pub model_version: String,
    pub dimensions: u32,
    pub normalization: VectorNormalization,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEmbeddingSpaceConfig {
    provider: String,
    model: String,
    model_version: String,
    dimensions: u32,
    normalization: VectorNormalization,
}

impl<'de> Deserialize<'de> for EmbeddingSpaceConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawEmbeddingSpaceConfig::deserialize(deserializer)?;
        let config = Self {
            provider: raw.provider,
            model: raw.model,
            model_version: raw.model_version,
            dimensions: raw.dimensions,
            normalization: raw.normalization,
        };
        config.validate().map_err(serde::de::Error::custom)?;
        Ok(config)
    }
}

impl EmbeddingSpaceConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.provider.trim().is_empty() || self.provider.len() > 128 {
            return Err("embedding provider must contain between 1 and 128 bytes");
        }
        if self.model.trim().is_empty() || self.model.len() > 256 {
            return Err("embedding model must contain between 1 and 256 bytes");
        }
        if self.model_version.trim().is_empty() || self.model_version.len() > 256 {
            return Err("embedding model_version must contain between 1 and 256 bytes");
        }
        if !(1..=32_768).contains(&self.dimensions) {
            return Err("embedding dimensions must be between 1 and 32768");
        }
        if self.normalization == VectorNormalization::None {
            return Err("query embedding spaces must use l2 or unit_length normalization");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_or_non_normalized_embedding_spaces() {
        let none = r#"{
            "provider":"local",
            "model":"mini",
            "model_version":"1",
            "dimensions":384,
            "normalization":"none"
        }"#;
        assert!(serde_json::from_str::<EmbeddingSpaceConfig>(none).is_err());
    }
}
