use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAX_NAME_BYTES: usize = 256;
const MAX_QUERY_CHARS: usize = 700;
const MAX_MODEL_BYTES: usize = 160;
const MAX_SOURCE_FILTERS: usize = 128;
const MAX_SOURCE_FILTER_BYTES: usize = 512;
const MAX_DELIVERY_CHANNELS: usize = 32;
const MAX_DELIVERY_CHANNEL_BYTES: usize = 120;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlertRule {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub owner_subject: String,
    pub revision_id: Uuid,
    pub revision_number: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub revision_created_at: DateTime<Utc>,
    pub name: String,
    pub query_text: String,
    pub embedding_model: String,
    pub similarity_threshold: f32,
    pub source_filters: Vec<String>,
    pub delivery_channels: Vec<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateAlertRule {
    pub name: String,
    pub query_text: String,
    pub embedding_model: String,
    pub similarity_threshold: f32,
    #[serde(default)]
    pub source_filters: Vec<String>,
    #[serde(default)]
    pub delivery_channels: Vec<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

impl CreateAlertRule {
    pub fn normalized(mut self) -> Result<Self, String> {
        self.name = self.name.trim().to_owned();
        self.query_text = collapse_whitespace(&self.query_text);
        self.embedding_model = self.embedding_model.trim().to_owned();
        normalize_list(
            &mut self.source_filters,
            MAX_SOURCE_FILTERS,
            MAX_SOURCE_FILTER_BYTES,
            "source_filters",
        )?;
        normalize_list(
            &mut self.delivery_channels,
            MAX_DELIVERY_CHANNELS,
            MAX_DELIVERY_CHANNEL_BYTES,
            "delivery_channels",
        )?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() || self.name.len() > MAX_NAME_BYTES {
            return Err(format!(
                "name must contain between 1 and {MAX_NAME_BYTES} bytes"
            ));
        }
        let query_chars = self.query_text.chars().count();
        if !(3..=MAX_QUERY_CHARS).contains(&query_chars) {
            return Err(format!(
                "query_text must contain between 3 and {MAX_QUERY_CHARS} characters"
            ));
        }
        if self.embedding_model.is_empty() || self.embedding_model.len() > MAX_MODEL_BYTES {
            return Err(format!(
                "embedding_model must contain between 1 and {MAX_MODEL_BYTES} bytes"
            ));
        }
        if !self.similarity_threshold.is_finite()
            || !(0.0..=1.0).contains(&self.similarity_threshold)
        {
            return Err("similarity_threshold must be finite and between 0 and 1".into());
        }
        validate_list(
            &self.source_filters,
            MAX_SOURCE_FILTERS,
            MAX_SOURCE_FILTER_BYTES,
            "source_filters",
        )?;
        validate_list(
            &self.delivery_channels,
            MAX_DELIVERY_CHANNELS,
            MAX_DELIVERY_CHANNEL_BYTES,
            "delivery_channels",
        )?;
        Ok(())
    }
}

fn default_enabled() -> bool {
    true
}

fn normalize_list(
    values: &mut Vec<String>,
    max_items: usize,
    max_bytes: usize,
    name: &str,
) -> Result<(), String> {
    for value in values.iter_mut() {
        *value = value.trim().to_owned();
    }
    values.sort();
    values.dedup();
    validate_list(values, max_items, max_bytes, name)
}

fn validate_list(
    values: &[String],
    max_items: usize,
    max_bytes: usize,
    name: &str,
) -> Result<(), String> {
    if values.len() > max_items {
        return Err(format!("{name} must contain at most {max_items} values"));
    }
    if values
        .iter()
        .any(|value| value.is_empty() || value.len() > max_bytes)
    {
        return Err(format!(
            "every {name} value must contain between 1 and {max_bytes} bytes"
        ));
    }
    Ok(())
}

fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_input() -> CreateAlertRule {
        CreateAlertRule {
            name: " Renewable launches ".into(),
            query_text: " Notify me when Acme launches renewable tools. ".into(),
            embedding_model: " text-embedding-v1 ".into(),
            similarity_threshold: 0.82,
            source_filters: vec!["source-b".into(), " source-a ".into(), "source-b".into()],
            delivery_channels: vec!["in_app".into()],
            enabled: true,
        }
    }

    #[test]
    fn normalizes_bounded_rule_input() {
        let normalized = valid_input().normalized().expect("valid rule");
        assert_eq!(normalized.name, "Renewable launches");
        assert_eq!(
            normalized.query_text,
            "Notify me when Acme launches renewable tools."
        );
        assert_eq!(normalized.source_filters, ["source-a", "source-b"]);
    }

    #[test]
    fn rejects_non_finite_thresholds_and_oversized_queries() {
        let mut input = valid_input();
        input.similarity_threshold = f32::NAN;
        assert!(input.normalized().is_err());

        let mut input = valid_input();
        input.query_text = "x".repeat(MAX_QUERY_CHARS + 1);
        assert!(input.normalized().is_err());
    }
}
