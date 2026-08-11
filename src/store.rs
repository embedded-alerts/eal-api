use std::fmt::Write as _;

use eal_interfaces::{
    CreateSourcePolicy, EmbeddingSearchHit, EmbeddingSearchRequest, EmbeddingSearchResponse,
    MatchCandidate, PageIngestRequest, PageRevision, SearchCursor, SourcePolicy,
    VectorNormalization,
};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, QueryResult, Statement, Value,
};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::HttpError;

const INDEXING_MIGRATION: &str = include_str!("../migrations/002_domain_scoped_indexing.sql");

#[derive(Debug)]
pub struct SearchPage {
    pub response: EmbeddingSearchResponse,
    records: Vec<SearchRecord>,
}

#[derive(Debug, Clone)]
struct SearchRecord {
    hit: EmbeddingSearchHit,
    content_sha256: String,
}

#[derive(Debug, Deserialize)]
struct SearchWire {
    embedding_id: Uuid,
    revision_id: Uuid,
    page_id: Uuid,
    source_id: Uuid,
    canonical_url: String,
    content_sha256: String,
    title: Option<String>,
    excerpt: String,
    similarity: f64,
    distance: f64,
    model: String,
    model_version: String,
    dimensions: u32,
    normalization: VectorNormalization,
    fetched_at: chrono::DateTime<chrono::Utc>,
}

impl SearchWire {
    fn into_record(self) -> SearchRecord {
        SearchRecord {
            content_sha256: self.content_sha256,
            hit: EmbeddingSearchHit {
                embedding_id: self.embedding_id,
                revision_id: self.revision_id,
                page_id: self.page_id,
                source_id: self.source_id,
                canonical_url: self.canonical_url,
                title: self.title,
                excerpt: self.excerpt,
                similarity: self.similarity,
                distance: self.distance,
                model: self.model,
                model_version: self.model_version,
                dimensions: self.dimensions,
                normalization: self.normalization,
                fetched_at: self.fetched_at,
            },
        }
    }
}

pub async fn migrate(db: &DatabaseConnection) -> Result<(), sea_orm::DbErr> {
    db.execute_unprepared(INDEXING_MIGRATION).await?;
    Ok(())
}

pub async fn list_sources(
    db: &DatabaseConnection,
    tenant_id: Uuid,
) -> Result<Vec<SourcePolicy>, HttpError> {
    let rows = db
        .query_all_raw(statement(
            r#"
            SELECT row_to_json(source_row)::text AS data
            FROM (
                SELECT *
                FROM eal_sources
                WHERE tenant_id = $1::uuid
                ORDER BY updated_at DESC, id ASC
                LIMIT 500
            ) AS source_row
            "#,
            vec![tenant_id.to_string().into()],
        ))
        .await?;

    rows.into_iter().map(decode_json_row).collect()
}

pub async fn get_source(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    source_id: Uuid,
) -> Result<Option<SourcePolicy>, HttpError> {
    let row = db
        .query_one_raw(statement(
            r#"
            SELECT row_to_json(source_row)::text AS data
            FROM (
                SELECT *
                FROM eal_sources
                WHERE tenant_id = $1::uuid AND id = $2::uuid
            ) AS source_row
            "#,
            vec![tenant_id.to_string().into(), source_id.to_string().into()],
        ))
        .await?;

    row.map(decode_json_row).transpose()
}

pub async fn create_source(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    input: &CreateSourcePolicy,
    canonical_root_url: &str,
) -> Result<SourcePolicy, HttpError> {
    let allowed_hosts = serde_json::to_string(&input.allowed_hosts)?;
    let allowed_paths = serde_json::to_string(&input.allowed_path_prefixes)?;
    let discovery_modes = serde_json::to_string(&input.discovery_modes)?;
    let values = vec![
        tenant_id.to_string().into(),
        input.name.clone().into(),
        canonical_root_url.to_owned().into(),
        allowed_hosts.into(),
        allowed_paths.into(),
        input.include_subdomains.into(),
        discovery_modes.into(),
        (input.crawl_interval_seconds as i32).into(),
        (input.max_depth as i16).into(),
        (input.max_pages_per_run as i32).into(),
        input.obey_robots.into(),
        input.enabled.into(),
    ];
    let row = db
        .query_one_raw(statement(
            r#"
            WITH inserted AS (
                INSERT INTO eal_sources (
                    tenant_id,
                    name,
                    root_url,
                    allowed_hosts,
                    allowed_path_prefixes,
                    include_subdomains,
                    discovery_modes,
                    crawl_interval_seconds,
                    max_depth,
                    max_pages_per_run,
                    obey_robots,
                    enabled
                )
                VALUES (
                    $1::uuid,
                    $2,
                    $3,
                    $4::jsonb,
                    $5::jsonb,
                    $6,
                    $7::jsonb,
                    $8,
                    $9,
                    $10,
                    $11,
                    $12
                )
                ON CONFLICT (tenant_id, root_url) DO NOTHING
                RETURNING *
            )
            SELECT row_to_json(inserted)::text AS data
            FROM inserted
            "#,
            values,
        ))
        .await?;

    match row {
        Some(row) => decode_json_row(row),
        None => Err(HttpError::conflict(
            "a source with this canonical root URL already exists",
        )),
    }
}

pub async fn ingest_page(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    source_id: Uuid,
    input: &PageIngestRequest,
    canonical_original_url: &str,
    canonical_final_url: &str,
) -> Result<PageRevision, HttpError> {
    let normalized_content = normalize_content(&input.content_text);
    let content_sha256 = sha256_hex(normalized_content.as_bytes());
    let vector = vector_literal(&input.embedding.values);
    let title = input.title.clone().unwrap_or_default();
    let published_at = input
        .published_at
        .as_ref()
        .map(chrono::DateTime::to_rfc3339)
        .unwrap_or_default();
    let fetched_at = input.fetched_at.to_rfc3339();
    let normalization = normalization_name(input.embedding.normalization);

    let values = vec![
        tenant_id.to_string().into(),
        source_id.to_string().into(),
        canonical_final_url.to_owned().into(),
        canonical_original_url.to_owned().into(),
        canonical_final_url.to_owned().into(),
        title.into(),
        normalized_content.into(),
        content_sha256.clone().into(),
        input.content_type.clone().into(),
        (input.http_status as i16).into(),
        published_at.into(),
        fetched_at.into(),
        input.embedding.model.clone().into(),
        input.embedding.model_version.clone().into(),
        (input.embedding.dimensions as i32).into(),
        normalization.to_owned().into(),
        vector.into(),
    ];

    let row = db
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
                SELECT id, true AS changed
                FROM inserted_revision
                UNION ALL
                SELECT revision.id, false AS changed
                FROM eal_page_revisions AS revision
                JOIN upserted_page AS page ON page.id = revision.page_id
                WHERE revision.tenant_id = $1::uuid
                  AND revision.content_sha256 = $8
                  AND NOT EXISTS (SELECT 1 FROM inserted_revision)
                LIMIT 1
            ),
            inserted_embedding AS (
                INSERT INTO eal_embeddings (
                    tenant_id,
                    revision_id,
                    model,
                    model_version,
                    dimensions,
                    normalization,
                    embedding,
                    generated_at
                )
                SELECT
                    $1::uuid,
                    revision.id,
                    $13,
                    $14,
                    $15,
                    $16,
                    CAST($17 AS vector),
                    $12::timestamptz
                FROM selected_revision AS revision
                ON CONFLICT (
                    tenant_id,
                    revision_id,
                    model,
                    model_version,
                    dimensions,
                    normalization
                ) DO NOTHING
                RETURNING id
            ),
            selected_embedding AS (
                SELECT id
                FROM inserted_embedding
                UNION ALL
                SELECT embedding.id
                FROM eal_embeddings AS embedding
                JOIN selected_revision AS revision
                  ON revision.id = embedding.revision_id
                WHERE embedding.tenant_id = $1::uuid
                  AND embedding.model = $13
                  AND embedding.model_version = $14
                  AND embedding.dimensions = $15
                  AND embedding.normalization = $16
                  AND NOT EXISTS (SELECT 1 FROM inserted_embedding)
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
                'revision_id', revision.id,
                'embedding_id', embedding.id,
                'source_id', $2::uuid,
                'tenant_id', $1::uuid,
                'canonical_url', $3,
                'content_sha256', $8,
                'changed', revision.changed,
                'fetched_at', $12::timestamptz
            )::text AS data
            FROM updated_page AS page
            CROSS JOIN selected_revision AS revision
            CROSS JOIN selected_embedding AS embedding
            "#,
            values,
        ))
        .await?;

    row.map(decode_json_row)
        .transpose()?
        .ok_or_else(|| HttpError::internal("page ingestion returned no durable record"))
}

pub async fn search_embeddings(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    request: &EmbeddingSearchRequest,
) -> Result<SearchPage, HttpError> {
    let mut values: Vec<Value> = vec![
        tenant_id.to_string().into(),
        request.embedding.model.clone().into(),
        request.embedding.model_version.clone().into(),
        (request.embedding.dimensions as i32).into(),
        normalization_name(request.embedding.normalization)
            .to_owned()
            .into(),
        vector_literal(&request.embedding.values).into(),
        (request.min_similarity as f64).into(),
    ];

    let source_clause = if request.source_ids.is_empty() {
        String::new()
    } else {
        let mut placeholders = Vec::with_capacity(request.source_ids.len());
        for source_id in &request.source_ids {
            values.push(source_id.to_string().into());
            placeholders.push(format!("${}::uuid", values.len()));
        }
        format!("AND page.source_id IN ({})", placeholders.join(", "))
    };

    let cursor_clause = if let Some(cursor) = &request.cursor {
        values.push(cursor.distance.into());
        let distance_placeholder = values.len();
        values.push(cursor.embedding_id.to_string().into());
        let id_placeholder = values.len();
        format!(
            "AND (distance > ${distance_placeholder} OR (distance = ${distance_placeholder} AND embedding_id > ${id_placeholder}::uuid))"
        )
    } else {
        String::new()
    };

    values.push((i64::from(request.limit) + 1).into());
    let limit_placeholder = values.len();
    let sql = format!(
        r#"
        WITH ranked AS (
            SELECT
                embedding.id AS embedding_id,
                revision.id AS revision_id,
                page.id AS page_id,
                page.source_id,
                page.canonical_url,
                revision.content_sha256,
                revision.title,
                left(revision.content_text, 600) AS excerpt,
                embedding.embedding <=> CAST($6 AS vector) AS distance,
                embedding.model,
                embedding.model_version,
                embedding.dimensions,
                embedding.normalization,
                revision.fetched_at
            FROM eal_embeddings AS embedding
            JOIN eal_page_revisions AS revision
              ON revision.id = embedding.revision_id
             AND revision.tenant_id = embedding.tenant_id
            JOIN eal_pages AS page
              ON page.id = revision.page_id
             AND page.tenant_id = revision.tenant_id
            WHERE embedding.tenant_id = $1::uuid
              AND embedding.model = $2
              AND embedding.model_version = $3
              AND embedding.dimensions = $4
              AND embedding.normalization = $5
              {source_clause}
        ),
        filtered AS (
            SELECT *, 1.0 - distance AS similarity
            FROM ranked
            WHERE 1.0 - distance >= $7
        )
        SELECT json_build_object(
            'embedding_id', embedding_id,
            'revision_id', revision_id,
            'page_id', page_id,
            'source_id', source_id,
            'canonical_url', canonical_url,
            'content_sha256', content_sha256,
            'title', title,
            'excerpt', excerpt,
            'similarity', similarity,
            'distance', distance,
            'model', model,
            'model_version', model_version,
            'dimensions', dimensions,
            'normalization', normalization,
            'fetched_at', fetched_at
        )::text AS data
        FROM filtered
        WHERE true
          {cursor_clause}
        ORDER BY distance ASC, embedding_id ASC
        LIMIT ${limit_placeholder}
        "#
    );

    let rows = db.query_all_raw(statement(sql, values)).await?;
    let mut records: Vec<SearchRecord> = rows
        .into_iter()
        .map(decode_json_row::<SearchWire>)
        .map(|result| result.map(SearchWire::into_record))
        .collect::<Result<_, _>>()?;

    let requested = request.limit as usize;
    let has_more = records.len() > requested;
    if has_more {
        records.truncate(requested);
    }
    let next_cursor = if has_more {
        records.last().map(|record| SearchCursor {
            distance: record.hit.distance,
            embedding_id: record.hit.embedding_id,
        })
    } else {
        None
    };
    let response = EmbeddingSearchResponse {
        hits: records.iter().map(|record| record.hit.clone()).collect(),
        next_cursor,
    };

    Ok(SearchPage { response, records })
}

pub async fn evaluate_matches(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    alert_rule_id: Uuid,
    threshold: f64,
    mut search: EmbeddingSearchRequest,
) -> Result<Vec<MatchCandidate>, HttpError> {
    search.min_similarity = search.min_similarity.max(threshold as f32);
    let page = search_embeddings(db, tenant_id, &search).await?;
    let mut candidates = Vec::with_capacity(page.records.len());

    for record in page.records {
        let hit = record.hit;
        let canonical_match_key = match_key(
            tenant_id,
            alert_rule_id,
            &record.content_sha256,
            &hit.model,
            &hit.model_version,
            hit.dimensions,
            hit.normalization,
        );
        let explanation = json!({
            "semantic_similarity": hit.similarity,
            "threshold": threshold,
            "model": hit.model,
            "model_version": hit.model_version,
            "dimensions": hit.dimensions,
            "normalization": normalization_name(hit.normalization),
            "canonical_url": hit.canonical_url,
            "content_sha256": record.content_sha256,
        });
        let values = vec![
            tenant_id.to_string().into(),
            alert_rule_id.to_string().into(),
            hit.revision_id.to_string().into(),
            hit.embedding_id.to_string().into(),
            canonical_match_key.into(),
            hit.similarity.into(),
            threshold.into(),
            serde_json::to_string(&explanation)?.into(),
        ];
        let row = db
            .query_one_raw(statement(
                r#"
                WITH upserted AS (
                    INSERT INTO eal_match_candidates (
                        tenant_id,
                        alert_rule_id,
                        revision_id,
                        embedding_id,
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
                        $5,
                        $6,
                        $7,
                        $8::jsonb
                    )
                    ON CONFLICT (tenant_id, canonical_match_key)
                    DO UPDATE SET
                        similarity = EXCLUDED.similarity,
                        threshold = EXCLUDED.threshold,
                        score_explanation = EXCLUDED.score_explanation,
                        updated_at = now()
                    RETURNING *
                )
                SELECT row_to_json(upserted)::text AS data
                FROM upserted
                "#,
                values,
            ))
            .await?;
        let candidate = row
            .map(decode_json_row)
            .transpose()?
            .ok_or_else(|| HttpError::internal("match upsert returned no durable record"))?;
        candidates.push(candidate);
    }

    Ok(candidates)
}

fn statement(sql: impl Into<String>, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

fn decode_json_row<T: DeserializeOwned>(row: QueryResult) -> Result<T, HttpError> {
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

fn match_key(
    tenant_id: Uuid,
    alert_rule_id: Uuid,
    content_sha256: &str,
    model: &str,
    model_version: &str,
    dimensions: u32,
    normalization: VectorNormalization,
) -> String {
    sha256_hex(
        format!(
            "{tenant_id}\n{alert_rule_id}\n{content_sha256}\n{model}\n{model_version}\n{dimensions}\n{}",
            normalization_name(normalization)
        )
        .as_bytes(),
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_hash_ignores_inconsequential_whitespace() {
        let one = sha256_hex(normalize_content("alpha\n beta\t gamma").as_bytes());
        let two = sha256_hex(normalize_content(" alpha beta gamma ").as_bytes());
        assert_eq!(one, two);
    }

    #[test]
    fn vector_literal_is_pgvector_compatible() {
        assert_eq!(vector_literal(&[0.25, -0.5, 1.0]), "[0.25,-0.5,1]");
    }

    #[test]
    fn match_identity_changes_with_content_or_model_provenance() {
        let tenant = Uuid::nil();
        let rule = Uuid::from_u128(1);
        let base = match_key(
            tenant,
            rule,
            "a",
            "model",
            "v1",
            3,
            VectorNormalization::L2,
        );
        assert_ne!(
            base,
            match_key(
                tenant,
                rule,
                "b",
                "model",
                "v1",
                3,
                VectorNormalization::L2,
            )
        );
        assert_ne!(
            base,
            match_key(
                tenant,
                rule,
                "a",
                "model",
                "v2",
                3,
                VectorNormalization::L2,
            )
        );
    }
}
