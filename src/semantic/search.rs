use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    embedding::{cosine_similarity, EmbeddedSegment, EmbeddingModelRef},
    extract::{normalized_token_set, QueryAnalysis, SegmentKind},
    PageRevision, SemanticError, SourceDomain,
};

const SEMANTIC_WEIGHT: f32 = 0.74;
const LEXICAL_WEIGHT: f32 = 0.12;
const ENTITY_WEIGHT: f32 = 0.09;
const RECENCY_WEIGHT: f32 = 0.03;
const SOURCE_WEIGHT: f32 = 0.02;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SemanticSearchRequest {
    pub query_text: String,
    #[serde(default)]
    pub source_ids: Vec<Uuid>,
    #[serde(default = "default_threshold")]
    pub threshold: f32,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub expected_model: Option<EmbeddingModelRef>,
    #[serde(default)]
    pub alert_rule: Option<AlertRuleRevisionRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AlertRuleRevisionRef {
    pub id: Uuid,
    pub revision: u32,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SemanticSearchResponse {
    pub query_text: String,
    pub model: EmbeddingModelRef,
    pub results: Vec<SearchResult>,
    pub next_cursor: Option<String>,
    pub compared_pages: usize,
    pub skipped_cross_model_pages: usize,
    pub candidate_matches_created: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SearchResult {
    pub page_revision_id: Uuid,
    pub source_id: Uuid,
    pub canonical_url: String,
    pub title: Option<String>,
    pub summary: String,
    pub fetched_at: DateTime<Utc>,
    pub content_hash: String,
    pub model: EmbeddingModelRef,
    pub score: f32,
    pub components: ScoreComponents,
    pub evidence: Vec<MatchEvidence>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ScoreComponents {
    pub semantic: f32,
    pub lexical: f32,
    pub entity: f32,
    pub recency: f32,
    pub source_priority: f32,
    pub weights: ScoreWeights,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ScoreWeights {
    pub semantic: f32,
    pub lexical: f32,
    pub entity: f32,
    pub recency: f32,
    pub source_priority: f32,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MatchEvidence {
    pub page_segment_kind: SegmentKind,
    pub page_text: String,
    pub query_segment_kind: SegmentKind,
    pub similarity: f32,
    pub weighted_similarity: f32,
}

pub(crate) fn validate_search_request(
    request: &SemanticSearchRequest,
) -> Result<(), SemanticError> {
    if !request.threshold.is_finite() || !(0.0..=1.0).contains(&request.threshold) {
        return Err(SemanticError::invalid(
            "threshold",
            "threshold must be a finite value between 0 and 1",
        ));
    }
    if !(1..=100).contains(&request.limit) {
        return Err(SemanticError::invalid(
            "limit",
            "limit must be between 1 and 100",
        ));
    }
    if request.source_ids.len() > 100 {
        return Err(SemanticError::invalid(
            "source_ids",
            "at most 100 source IDs may be searched at once",
        ));
    }
    if request
        .alert_rule
        .as_ref()
        .is_some_and(|rule| rule.revision == 0)
    {
        return Err(SemanticError::invalid(
            "alert_rule_revision",
            "alert rule revision must be greater than zero",
        ));
    }
    Ok(())
}

pub(crate) fn score_page(
    query: &QueryAnalysis,
    query_embeddings: &[EmbeddedSegment],
    page: &PageRevision,
    source: &SourceDomain,
    now: DateTime<Utc>,
) -> Result<SearchResult, SemanticError> {
    validate_query_dimensions(query_embeddings, &page.model)?;

    let mut evidence = Vec::new();
    for query_segment in query_embeddings {
        for page_segment in &page.segments {
            let similarity = cosine_similarity(&query_segment.vector, &page_segment.vector)?;
            let normalized_similarity = ((similarity + 1.0) / 2.0).clamp(0.0, 1.0);
            let weighted_similarity = (normalized_similarity
                * query_segment.weight
                * page_segment.weight)
                .clamp(0.0, 1.0);
            evidence.push(MatchEvidence {
                page_segment_kind: page_segment.kind,
                page_text: page_segment.text.clone(),
                query_segment_kind: query_segment.kind,
                similarity: normalized_similarity,
                weighted_similarity,
            });
        }
    }
    evidence.sort_by(|left, right| {
        right
            .weighted_similarity
            .total_cmp(&left.weighted_similarity)
            .then_with(|| left.page_text.cmp(&right.page_text))
    });
    dedupe_evidence(&mut evidence);
    evidence.truncate(5);

    let semantic = top_evidence_mean(&evidence, 3);
    let page_tokens = normalized_token_set(&format!(
        "{} {} {} {}",
        page.title.as_deref().unwrap_or(""),
        page.summary,
        page.keywords.join(" "),
        page.entities.join(" ")
    ));
    let lexical = jaccard(&query.tokens, &page_tokens);
    let query_entities: BTreeSet<String> = query
        .entities
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect();
    let page_entities: BTreeSet<String> = page
        .entities
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect();
    let entity = overlap_ratio(&query_entities, &page_entities);
    let age_days = now
        .signed_duration_since(page.fetched_at)
        .num_seconds()
        .max(0) as f32
        / 86_400.0;
    let recency = 1.0 / (1.0 + age_days / 30.0);
    let source_priority = source.source_priority.clamp(0.0, 1.0);
    let score = (semantic * SEMANTIC_WEIGHT
        + lexical * LEXICAL_WEIGHT
        + entity * ENTITY_WEIGHT
        + recency * RECENCY_WEIGHT
        + source_priority * SOURCE_WEIGHT)
        .clamp(0.0, 1.0);

    Ok(SearchResult {
        page_revision_id: page.id,
        source_id: page.source_id,
        canonical_url: page.canonical_url.clone(),
        title: page.title.clone(),
        summary: page.summary.clone(),
        fetched_at: page.fetched_at,
        content_hash: page.content_hash.clone(),
        model: page.model.clone(),
        score,
        components: ScoreComponents {
            semantic,
            lexical,
            entity,
            recency,
            source_priority,
            weights: ScoreWeights {
                semantic: SEMANTIC_WEIGHT,
                lexical: LEXICAL_WEIGHT,
                entity: ENTITY_WEIGHT,
                recency: RECENCY_WEIGHT,
                source_priority: SOURCE_WEIGHT,
            },
        },
        evidence,
    })
}

fn validate_query_dimensions(
    query_embeddings: &[EmbeddedSegment],
    model: &EmbeddingModelRef,
) -> Result<(), SemanticError> {
    if query_embeddings.is_empty() {
        return Err(SemanticError::invalid(
            "query_embeddings",
            "query did not produce any embeddings",
        ));
    }
    if query_embeddings
        .iter()
        .any(|segment| segment.vector.len() != model.dimensions)
    {
        return Err(SemanticError::conflict(
            "embedding_dimensions",
            "query vector dimensions do not match page model provenance",
        ));
    }
    Ok(())
}

fn top_evidence_mean(evidence: &[MatchEvidence], count: usize) -> f32 {
    let selected = evidence.iter().take(count).collect::<Vec<_>>();
    if selected.is_empty() {
        return 0.0;
    }
    selected
        .iter()
        .map(|evidence| evidence.weighted_similarity)
        .sum::<f32>()
        / selected.len() as f32
}

fn dedupe_evidence(evidence: &mut Vec<MatchEvidence>) {
    let mut seen = BTreeSet::new();
    evidence.retain(|item| {
        seen.insert((
            item.page_segment_kind as u8,
            item.page_text.to_ascii_lowercase(),
        ))
    });
}

fn jaccard(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f32 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(right).count() as f32;
    let union = left.union(right).count() as f32;
    (intersection / union).clamp(0.0, 1.0)
}

fn overlap_ratio(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f32 {
    if left.is_empty() {
        return 0.0;
    }
    (left.intersection(right).count() as f32 / left.len() as f32).clamp(0.0, 1.0)
}

const fn default_threshold() -> f32 {
    0.72
}

const fn default_limit() -> usize {
    20
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_overlap_is_bounded() {
        let left = BTreeSet::from(["alpha".to_owned(), "beta".to_owned()]);
        let right = BTreeSet::from(["beta".to_owned(), "gamma".to_owned()]);
        assert!((jaccard(&left, &right) - (1.0 / 3.0)).abs() < 0.0001);
    }

    #[test]
    fn validation_rejects_unbounded_pagination() {
        let request = SemanticSearchRequest {
            query_text: "hello".into(),
            source_ids: Vec::new(),
            threshold: 0.7,
            limit: 101,
            cursor: None,
            expected_model: None,
            alert_rule: None,
        };
        assert!(validate_search_request(&request).is_err());
    }
}
