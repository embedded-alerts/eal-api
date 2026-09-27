use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use eal_interfaces::{EmbeddingPayload, PageIngestRequest, VectorNormalization};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, QueryResult, Statement,
    TransactionTrait, Value,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::HttpError;

const DEFAULT_MAX_CANDIDATES: u16 = 100;
const MAX_CANDIDATES: u16 = 500;

#[derive(Debug, Clone, Deserialize)]
pub struct AlertRuleEmbeddingUpsertRequest {
    pub alert_rule_revision_id: Uuid,
    pub embedding: EmbeddingPayload,
}

impl AlertRuleEmbeddingUpsertRequest {
    pub fn validate(&self) -> Result<(), eal_interfaces::ValidationError> {
        self.embedding.validate()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AlertRuleEmbeddingReceipt {
    pub id: Uuid,
    pub alert_rule_id: Uuid,
    pub alert_rule_revision_id: Uuid,
    pub embedding_sha256: String,
    pub created: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReverseAlertPageRequest {
    pub source_id: Uuid,
    pub url: String,
    pub final_url: String,
    pub title: Option<String>,
    pub content_text: String,
    pub content_type: String,
    pub http_status: u16,
    pub published_at: Option<DateTime<Utc>>,
    pub fetched_at: DateTime<Utc>,
    pub embedding: EmbeddingPayload,
    #[serde(default = "default_max_candidates")]
    pub max_candidates: u16,
}

impl ReverseAlertPageRequest {
    pub fn validate(&self) -> Result<(), eal_interfaces::ValidationError> {
        let page = PageIngestRequest {
            source_id: self.source_id,
            url: self.url.clone(),
            final_url: self.final_url.clone(),
            title: self.title.clone(),
            content_text: self.content_text.clone(),
            content_type: self.content_type.clone(),
            http_status: self.http_status,
            published_at: self.published_at,
            fetched_at: self.fetched_at,
            embedding: self.embedding.clone(),
        };
        page.validate()?;
        if !(1..=MAX_CANDIDATES).contains(&self.max_candidates) {
            return Err(eal_interfaces::ValidationError(format!(
                "max_candidates must be between 1 and {MAX_CANDIDATES}"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReverseAlertCandidate {
    pub id: Uuid,
    pub alert_rule_id: Uuid,
    pub alert_rule_revision_id: Uuid,
    pub page_revision_id: Uuid,
    pub alert_embedding_id: Uuid,
    pub similarity: f64,
    pub threshold: f64,
    pub canonical_match_key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReverseAlertMatchResponse {
    pub page_id: Uuid,
    pub page_revision_id: Uuid,
    pub content_sha256: String,
    pub changed: bool,
    pub page_vector_persisted: bool,
    pub candidates: Vec<ReverseAlertCandidate>,
}

#[derive(Debug, Deserialize)]
struct AlertEmbeddingWire {
    id: Uuid,
    alert_rule_id: Uuid,
    alert_rule_revision_id: Uuid,
    embedding_sha256: String,
    created: bool,
}

#[derive(Debug, Deserialize)]
struct PageWire {
    page_id: Uuid,
    page_revision_id: Uuid,
    content_sha256: String,
    changed: bool,
}

#[derive(Debug, Deserialize)]
struct CandidateWire {
    alert_rule_id: Uuid,
    alert_rule_revision_id: Uuid,
    alert_embedding_id: Uuid,
    similarity: f64,
    threshold: f64,
}

pub async fn upsert_alert_rule_embedding(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    alert_rule_id: Uuid,
    request: &AlertRuleEmbeddingUpsertRequest,
) -> Result<AlertRuleEmbeddingReceipt, HttpError> {
    request.validate().map_err(HttpError::validation)?;
    let vector_sha256 = embedding_sha256(&request.embedding.values);
    let generated_at = Utc::now().to_rfc3339();
    let values = vec![
        tenant_id.to_string().into(),
        alert_rule_id.to_string().into(),
        request.alert_rule_revision_id.to_string().into(),
        request.embedding.model.clone().into(),
        request.embedding.model_version.clone().into(),
        (request.embedding.dimensions as i32).into(),
        normalization_name(request.embedding.normalization)
            .to_owned()
            .into(),
        vector_literal(&request.embedding.values).into(),
        vector_sha256.clone().into(),
        generated_at.into(),
    ];
    let row = db
        .query_one_raw(statement(
            r#"
            WITH target AS (
                SELECT rule.id AS alert_rule_id, revision.id AS alert_rule_revision_id
                FROM eal_alert_rules AS rule
                JOIN eal_alert_rule_revisions AS revision
                  ON revision.tenant_id = rule.tenant_id
                 AND revision.id = rule.active_revision_id
                 AND revision.alert_rule_id = rule.id
                WHERE rule.tenant_id = $1::uuid
                  AND rule.id = $2::uuid
                  AND revision.id = $3::uuid
                  AND revision.enabled = TRUE
                  AND revision.embedding_model = $4
            ),
            inserted AS (
                INSERT INTO eal_alert_rule_embeddings (
                    tenant_id,
                    alert_rule_id,
                    alert_rule_revision_id,
                    model,
                    model_version,
                    dimensions,
                    normalization,
                    embedding,
                    embedding_sha256,
                    generated_at
                )
                SELECT
                    $1::uuid,
                    target.alert_rule_id,
                    target.alert_rule_revision_id,
                    $4,
                    $5,
                    $6,
                    $7,
                    CAST($8 AS vector),
                    $9,
                    $10::timestamptz
                FROM target
                ON CONFLICT (
                    tenant_id,
                    alert_rule_revision_id,
                    model,
                    model_version,
                    dimensions,
                    normalization
                ) DO NOTHING
                RETURNING id, alert_rule_id, alert_rule_revision_id, embedding_sha256
            ),
            selected AS (
                SELECT id, alert_rule_id, alert_rule_revision_id, embedding_sha256, TRUE AS created
                FROM inserted
                UNION ALL
                SELECT
                    embedding.id,
                    embedding.alert_rule_id,
                    embedding.alert_rule_revision_id,
                    embedding.embedding_sha256,
                    FALSE AS created
                FROM eal_alert_rule_embeddings AS embedding
                JOIN target
                  ON target.alert_rule_revision_id = embedding.alert_rule_revision_id
                 AND target.alert_rule_id = embedding.alert_rule_id
                WHERE embedding.tenant_id = $1::uuid
                  AND embedding.model = $4
                  AND embedding.model_version = $5
                  AND embedding.dimensions = $6
                  AND embedding.normalization = $7
                  AND NOT EXISTS (SELECT 1 FROM inserted)
                LIMIT 1
            )
            SELECT row_to_json(selected)::text AS data
            FROM selected
            "#,
            values,
        ))
        .await?;
    let wire: AlertEmbeddingWire = row.map(decode_json_row).transpose()?.ok_or_else(|| {
        HttpError::validation(
            "alert embedding must target the enabled active rule revision and its configured model",
        )
    })?;
    if wire.embedding_sha256 != vector_sha256 {
        return Err(HttpError::conflict(
            "an immutable embedding already exists for this alert revision and model space",
        ));
    }
    Ok(AlertRuleEmbeddingReceipt {
        id: wire.id,
        alert_rule_id: wire.alert_rule_id,
        alert_rule_revision_id: wire.alert_rule_revision_id,
        embedding_sha256: wire.embedding_sha256,
        created: wire.created,
    })
}

pub async fn ingest_and_reverse_match(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    source_id: Uuid,
    request: &ReverseAlertPageRequest,
    canonical_original_url: &str,
    canonical_final_url: &str,
) -> Result<ReverseAlertMatchResponse, HttpError> {
    request.validate().map_err(HttpError::validation)?;
    let transaction = db.begin().await?;
    let page = upsert_page_revision(
        &transaction,
        tenant_id,
        source_id,
        request,
        canonical_original_url,
        canonical_final_url,
    )
    .await?;
    let page_vector_sha256 = embedding_sha256(&request.embedding.values);
    let candidate_rows = find_alert_candidates(&transaction, tenant_id, source_id, request).await?;
    let mut candidates = Vec::with_capacity(candidate_rows.len());
    for candidate in candidate_rows {
        if !candidate.similarity.is_finite() || candidate.similarity < candidate.threshold {
            continue;
        }
        let key = match_key(MatchIdentity {
            tenant_id,
            alert_rule_id: candidate.alert_rule_id,
            alert_rule_revision_id: candidate.alert_rule_revision_id,
            page_revision_id: page.page_revision_id,
            content_sha256: &page.content_sha256,
            model: &request.embedding.model,
            model_version: &request.embedding.model_version,
            dimensions: request.embedding.dimensions,
            normalization: request.embedding.normalization,
        });
        let explanation = json!({
            "match_mode": "reverse_alert_stream",
            "semantic_similarity": candidate.similarity,
            "threshold": candidate.threshold,
            "alert_rule_revision_id": candidate.alert_rule_revision_id,
            "page_revision_id": page.page_revision_id,
            "model": request.embedding.model,
            "model_version": request.embedding.model_version,
            "dimensions": request.embedding.dimensions,
            "normalization": normalization_name(request.embedding.normalization),
            "canonical_url": canonical_final_url,
            "content_sha256": page.content_sha256,
            "page_embedding_sha256": page_vector_sha256,
            "page_vector_persisted": false,
        });
        let durable = persist_reverse_candidate(
            &transaction,
            tenant_id,
            page.page_revision_id,
            &page.content_sha256,
            request,
            &page_vector_sha256,
            candidate,
            &key,
            &explanation,
        )
        .await?;
        candidates.push(durable);
    }
    transaction.commit().await?;
    Ok(ReverseAlertMatchResponse {
        page_id: page.page_id,
        page_revision_id: page.page_revision_id,
        content_sha256: page.content_sha256,
        changed: page.changed,
        page_vector_persisted: false,
        candidates,
    })
}

async fn upsert_page_revision(
    transaction: &DatabaseTransaction,
    tenant_id: Uuid,
    source_id: Uuid,
    request: &ReverseAlertPageRequest,
    canonical_original_url: &str,
    canonical_final_url: &str,
) -> Result<PageWire, HttpError> {
    let normalized_content = normalize_content(&request.content_text);
    let content_sha256 = sha256_hex(normalized_content.as_bytes());
    let title = request.title.clone().unwrap_or_default();
    let published_at = request
        .published_at
        .as_ref()
        .map(DateTime::to_rfc3339)
        .unwrap_or_default();
    let fetched_at = request.fetched_at.to_rfc3339();
    let values = vec![
        tenant_id.to_string().into(),
        source_id.to_string().into(),
        canonical_final_url.to_owned().into(),
        canonical_original_url.to_owned().into(),
        canonical_final_url.to_owned().into(),
        title.into(),
        normalized_content.into(),
        content_sha256.into(),
        request.content_type.clone().into(),
        (request.http_status as i16).into(),
        published_at.into(),
        fetched_at.into(),
    ];
    let row = transaction
        .query_one_raw(statement(
            r#"
            WITH upserted_page AS (
                INSERT INTO eal_pages (
                    tenant_id,
                    source_id,
                    canonical_url,
                    first_seen_at,
                    last_seen_at
                )
                VALUES ($1::uuid, $2::uuid, $3, $12::timestamptz, $12::timestamptz)
                ON CONFLICT (tenant_id, source_id, canonical_url)
                DO UPDATE SET
                    last_seen_at = EXCLUDED.last_seen_at,
                    updated_at = now()
                RETURNING id
            ),
            inserted_revision AS (
                INSERT INTO eal_page_revisions (
                    tenant_id,
                    page_id,
                    predecessor_revision_id,
                    original_url,
                    final_url,
                    title,
                    content_text,
                    content_sha256,
                    content_type,
                    http_status,
                    published_at,
                    fetched_at
                )
                SELECT
                    $1::uuid,
                    page.id,
                    current_page.latest_revision_id,
                    $4,
                    $5,
                    NULLIF($6, ''),
                    $7,
                    $8,
                    $9,
                    $10::smallint,
                    NULLIF($11, '')::timestamptz,
                    $12::timestamptz
                FROM upserted_page AS page
                JOIN eal_pages AS current_page ON current_page.id = page.id
                ON CONFLICT (tenant_id, page_id, content_sha256) DO NOTHING
                RETURNING id
            ),
            selected_revision AS (
                SELECT id, TRUE AS changed
                FROM inserted_revision
                UNION ALL
                SELECT revision.id, FALSE AS changed
                FROM eal_page_revisions AS revision
                JOIN upserted_page AS page ON page.id = revision.page_id
                WHERE revision.tenant_id = $1::uuid
                  AND revision.content_sha256 = $8
                  AND NOT EXISTS (SELECT 1 FROM inserted_revision)
                LIMIT 1
            ),
            updated_page AS (
                UPDATE eal_pages AS page
                SET
                    latest_revision_id = revision.id,
                    last_seen_at = $12::timestamptz,
                    updated_at = now()
                FROM selected_revision AS revision
                WHERE page.id = (SELECT id FROM upserted_page)
                RETURNING page.id
            )
            SELECT json_build_object(
                'page_id', page.id,
                'page_revision_id', revision.id,
                'content_sha256', $8,
                'changed', revision.changed
            )::text AS data
            FROM updated_page AS page
            CROSS JOIN selected_revision AS revision
            "#,
            values,
        ))
        .await?;
    row.map(decode_json_row)
        .transpose()?
        .ok_or_else(|| HttpError::internal("transient page ingestion returned no durable revision"))
}

async fn find_alert_candidates(
    transaction: &DatabaseTransaction,
    tenant_id: Uuid,
    source_id: Uuid,
    request: &ReverseAlertPageRequest,
) -> Result<Vec<CandidateWire>, HttpError> {
    let values = vec![
        tenant_id.to_string().into(),
        source_id.to_string().into(),
        request.embedding.model.clone().into(),
        request.embedding.model_version.clone().into(),
        (request.embedding.dimensions as i32).into(),
        normalization_name(request.embedding.normalization)
            .to_owned()
            .into(),
        vector_literal(&request.embedding.values).into(),
        (request.max_candidates as i64).into(),
    ];
    let rows = transaction
        .query_all_raw(statement(
            r#"
            SELECT row_to_json(candidate)::text AS data
            FROM (
                SELECT
                    embedding.alert_rule_id,
                    embedding.alert_rule_revision_id,
                    embedding.id AS alert_embedding_id,
                    1.0 - (embedding.embedding <=> CAST($7 AS vector)) AS similarity,
                    revision.similarity_threshold::double precision AS threshold
                FROM eal_alert_rule_embeddings AS embedding
                JOIN eal_alert_rules AS rule
                  ON rule.tenant_id = embedding.tenant_id
                 AND rule.id = embedding.alert_rule_id
                 AND rule.active_revision_id = embedding.alert_rule_revision_id
                JOIN eal_alert_rule_revisions AS revision
                  ON revision.tenant_id = embedding.tenant_id
                 AND revision.id = embedding.alert_rule_revision_id
                 AND revision.alert_rule_id = embedding.alert_rule_id
                WHERE embedding.tenant_id = $1::uuid
                  AND embedding.model = $3
                  AND embedding.model_version = $4
                  AND embedding.dimensions = $5
                  AND embedding.normalization = $6
                  AND revision.enabled = TRUE
                  AND revision.embedding_model = $3
                  AND (
                      jsonb_array_length(revision.source_filters) = 0
                      OR revision.source_filters ? ('source:' || $2::uuid::text)
                  )
                ORDER BY embedding.embedding <=> CAST($7 AS vector), embedding.id
                LIMIT $8
            ) AS candidate
            "#,
            values,
        ))
        .await?;
    rows.into_iter().map(decode_json_row).collect()
}

#[allow(clippy::too_many_arguments)]
async fn persist_reverse_candidate(
    transaction: &DatabaseTransaction,
    tenant_id: Uuid,
    page_revision_id: Uuid,
    content_sha256: &str,
    request: &ReverseAlertPageRequest,
    page_embedding_sha256: &str,
    candidate: CandidateWire,
    canonical_match_key: &str,
    explanation: &serde_json::Value,
) -> Result<ReverseAlertCandidate, HttpError> {
    let values = vec![
        tenant_id.to_string().into(),
        candidate.alert_rule_id.to_string().into(),
        candidate.alert_rule_revision_id.to_string().into(),
        page_revision_id.to_string().into(),
        candidate.alert_embedding_id.to_string().into(),
        page_embedding_sha256.to_owned().into(),
        request.embedding.model.clone().into(),
        request.embedding.model_version.clone().into(),
        (request.embedding.dimensions as i32).into(),
        normalization_name(request.embedding.normalization)
            .to_owned()
            .into(),
        canonical_match_key.to_owned().into(),
        candidate.similarity.into(),
        candidate.threshold.into(),
        serde_json::to_string(explanation)?.into(),
        content_sha256.to_owned().into(),
    ];
    let row = transaction
        .query_one_raw(statement(
            r#"
            WITH inserted AS (
                INSERT INTO eal_match_candidates (
                    tenant_id,
                    alert_rule_id,
                    alert_rule_revision_id,
                    revision_id,
                    embedding_id,
                    match_mode,
                    alert_embedding_id,
                    page_embedding_sha256,
                    page_embedding_model,
                    page_embedding_model_version,
                    page_embedding_dimensions,
                    page_embedding_normalization,
                    canonical_match_key,
                    similarity,
                    threshold,
                    score_explanation
                )
                VALUES (
                    $1::uuid,
                    $2::uuid,
                    $3::uuid,
                    $4::uuid,
                    NULL,
                    'reverse_alert_stream',
                    $5::uuid,
                    $6,
                    $7,
                    $8,
                    $9,
                    $10,
                    $11,
                    $12,
                    $13,
                    $14::jsonb
                )
                ON CONFLICT (tenant_id, canonical_match_key)
                DO NOTHING
                RETURNING id, alert_rule_id, alert_rule_revision_id, revision_id,
                          alert_embedding_id, similarity, threshold, canonical_match_key
            ),
            selected AS (
                SELECT * FROM inserted
                UNION ALL
                SELECT
                    existing.id,
                    existing.alert_rule_id,
                    existing.alert_rule_revision_id,
                    existing.revision_id,
                    existing.alert_embedding_id,
                    existing.similarity,
                    existing.threshold,
                    existing.canonical_match_key
                FROM eal_match_candidates AS existing
                WHERE existing.tenant_id = $1::uuid
                  AND existing.canonical_match_key = $11
                  AND existing.alert_rule_id = $2::uuid
                  AND existing.alert_rule_revision_id = $3::uuid
                  AND existing.revision_id = $4::uuid
                  AND existing.match_mode = 'reverse_alert_stream'
                  AND existing.alert_embedding_id = $5::uuid
                  AND existing.page_embedding_sha256 = $6
                  AND existing.page_embedding_model = $7
                  AND existing.page_embedding_model_version = $8
                  AND existing.page_embedding_dimensions = $9
                  AND existing.page_embedding_normalization = $10
                  AND EXISTS (
                      SELECT 1
                      FROM eal_page_revisions AS revision
                      WHERE revision.tenant_id = $1::uuid
                        AND revision.id = existing.revision_id
                        AND revision.content_sha256 = $15
                  )
                  AND NOT EXISTS (SELECT 1 FROM inserted)
                LIMIT 1
            )
            SELECT json_build_object(
                'id', id,
                'alert_rule_id', alert_rule_id,
                'alert_rule_revision_id', alert_rule_revision_id,
                'page_revision_id', revision_id,
                'alert_embedding_id', alert_embedding_id,
                'similarity', similarity,
                'threshold', threshold,
                'canonical_match_key', canonical_match_key
            )::text AS data
            FROM selected
            "#,
            values,
        ))
        .await?;
    row.map(decode_json_row).transpose()?.ok_or_else(|| {
        HttpError::conflict("canonical match identity exists with different evidence")
    })
}

fn default_max_candidates() -> u16 {
    DEFAULT_MAX_CANDIDATES
}

fn statement(sql: impl Into<String>, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

fn decode_json_row<T: for<'de> Deserialize<'de>>(row: QueryResult) -> Result<T, HttpError> {
    let data: String = row.try_get("", "data")?;
    Ok(serde_json::from_str(&data)?)
}

fn normalization_name(normalization: VectorNormalization) -> &'static str {
    match normalization {
        VectorNormalization::None => "none",
        VectorNormalization::L2 => "l2",
        VectorNormalization::UnitLength => "unit_length",
    }
}

fn normalize_content(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn vector_literal(values: &[f32]) -> String {
    let mut vector = String::with_capacity(values.len().saturating_mul(12) + 2);
    vector.push('[');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            vector.push(',');
        }
        write!(&mut vector, "{value}").expect("writing to a String cannot fail");
    }
    vector.push(']');
    vector
}

fn embedding_sha256(values: &[f32]) -> String {
    let mut digest = Sha256::new();
    for value in values {
        let canonical = if *value == 0.0 { 0.0_f32 } else { *value };
        digest.update(canonical.to_bits().to_be_bytes());
    }
    hex::encode(digest.finalize())
}

struct MatchIdentity<'a> {
    tenant_id: Uuid,
    alert_rule_id: Uuid,
    alert_rule_revision_id: Uuid,
    page_revision_id: Uuid,
    content_sha256: &'a str,
    model: &'a str,
    model_version: &'a str,
    dimensions: u32,
    normalization: VectorNormalization,
}

fn match_key(identity: MatchIdentity<'_>) -> String {
    let canonical_fields = [
        identity.tenant_id.to_string(),
        identity.alert_rule_id.to_string(),
        identity.alert_rule_revision_id.to_string(),
        identity.page_revision_id.to_string(),
        identity.content_sha256.to_owned(),
        identity.model.to_owned(),
        identity.model_version.to_owned(),
        identity.dimensions.to_string(),
        normalization_name(identity.normalization).to_owned(),
    ];
    let canonical_json = serde_json::to_vec(&canonical_fields)
        .expect("serializing canonical match identity fields cannot fail");
    sha256_hex(&canonical_json)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_fingerprint_canonicalizes_signed_zero() {
        assert_eq!(
            embedding_sha256(&[0.0, 1.0]),
            embedding_sha256(&[-0.0, 1.0])
        );
    }

    #[test]
    fn reverse_match_key_is_revision_and_model_space_bound() {
        let base = match_key(MatchIdentity {
            tenant_id: Uuid::nil(),
            alert_rule_id: Uuid::from_u128(1),
            alert_rule_revision_id: Uuid::from_u128(2),
            page_revision_id: Uuid::from_u128(3),
            content_sha256: "content",
            model: "model",
            model_version: "v1",
            dimensions: 3,
            normalization: VectorNormalization::UnitLength,
        });
        let changed = match_key(MatchIdentity {
            tenant_id: Uuid::nil(),
            alert_rule_id: Uuid::from_u128(1),
            alert_rule_revision_id: Uuid::from_u128(2),
            page_revision_id: Uuid::from_u128(3),
            content_sha256: "content",
            model: "model",
            model_version: "v2",
            dimensions: 3,
            normalization: VectorNormalization::UnitLength,
        });
        assert_ne!(base, changed);
    }

    #[test]
    fn reverse_request_bounds_candidate_fanout() {
        let request = ReverseAlertPageRequest {
            source_id: Uuid::nil(),
            url: "https://example.com/a".into(),
            final_url: "https://example.com/a".into(),
            title: None,
            content_text: "material page content".into(),
            content_type: "text/html".into(),
            http_status: 200,
            published_at: None,
            fetched_at: Utc::now(),
            embedding: EmbeddingPayload {
                model: "model".into(),
                model_version: "v1".into(),
                dimensions: 2,
                normalization: VectorNormalization::UnitLength,
                values: vec![1.0, 0.0],
            },
            max_candidates: MAX_CANDIDATES + 1,
        };
        assert!(request.validate().is_err());
    }
}
