use chrono::Utc;
use eal_api::{migrations, reverse_match};
use eal_interfaces::{EmbeddingPayload, VectorNormalization};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use uuid::Uuid;

async fn test_database() -> Option<DatabaseConnection> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let database = Database::connect(url).await.ok()?;
    migrations::migrate_all(&database).await.ok()?;
    Some(database)
}

async fn insert_source(
    database: &DatabaseConnection,
    tenant_id: Uuid,
    source_id: Uuid,
) -> Result<(), sea_orm::DbErr> {
    database
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO eal_sources (
                id,
                tenant_id,
                name,
                root_url,
                allowed_hosts,
                allowed_path_prefixes,
                discovery_modes,
                enabled
            )
            VALUES (
                $1::uuid,
                $2::uuid,
                $3,
                $4,
                '["example.com"]'::jsonb,
                '["/"]'::jsonb,
                '["manual"]'::jsonb,
                TRUE
            )
            "#,
            vec![
                source_id.to_string().into(),
                tenant_id.to_string().into(),
                format!("isolation-source-{source_id}").into(),
                format!("https://example.com/{source_id}/").into(),
            ],
        ))
        .await?;
    Ok(())
}

async fn insert_active_rule(
    database: &DatabaseConnection,
    tenant_id: Uuid,
    alert_rule_id: Uuid,
    revision_id: Uuid,
    source_filter: Option<Uuid>,
) -> Result<(), sea_orm::DbErr> {
    let owner_subject = format!("isolation-owner-{tenant_id}-{alert_rule_id}");
    database
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO eal_alert_rules (id, tenant_id, owner_subject)
            VALUES ($1::uuid, $2::uuid, $3)
            "#,
            vec![
                alert_rule_id.to_string().into(),
                tenant_id.to_string().into(),
                owner_subject.clone().into(),
            ],
        ))
        .await?;

    let source_filters = source_filter
        .map(|source_id| format!(r#"["source:{source_id}"]"#))
        .unwrap_or_else(|| "[]".to_owned());
    database
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO eal_alert_rule_revisions (
                id,
                tenant_id,
                alert_rule_id,
                revision_number,
                created_by_subject,
                name,
                query_text,
                embedding_model,
                similarity_threshold,
                source_filters,
                delivery_channels,
                enabled
            )
            VALUES (
                $1::uuid,
                $2::uuid,
                $3::uuid,
                1,
                $4,
                'isolation rule',
                'rust distributed systems',
                'test-embedding-model',
                0.80,
                $5::jsonb,
                '["in_app"]'::jsonb,
                TRUE
            )
            "#,
            vec![
                revision_id.to_string().into(),
                tenant_id.to_string().into(),
                alert_rule_id.to_string().into(),
                owner_subject.into(),
                source_filters.into(),
            ],
        ))
        .await?;

    database
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE eal_alert_rules
            SET active_revision_id = $1::uuid, updated_at = NOW()
            WHERE tenant_id = $2::uuid
              AND id = $3::uuid
            "#,
            vec![
                revision_id.to_string().into(),
                tenant_id.to_string().into(),
                alert_rule_id.to_string().into(),
            ],
        ))
        .await?;
    Ok(())
}

async fn upsert_alert_embedding(
    database: &DatabaseConnection,
    tenant_id: Uuid,
    alert_rule_id: Uuid,
    revision_id: Uuid,
) {
    let request = reverse_match::AlertRuleEmbeddingUpsertRequest {
        alert_rule_revision_id: revision_id,
        embedding: EmbeddingPayload {
            model: "test-embedding-model".into(),
            model_version: "v1".into(),
            dimensions: 2,
            normalization: VectorNormalization::UnitLength,
            values: vec![1.0, 0.0],
        },
    };
    let receipt = reverse_match::upsert_alert_rule_embedding(
        database,
        tenant_id,
        alert_rule_id,
        &request,
    )
    .await
    .unwrap();
    assert!(receipt.created);
}

fn page_request(source_id: Uuid, model_version: &str) -> reverse_match::ReverseAlertPageRequest {
    let url = format!("https://example.com/{source_id}/rust-consensus");
    reverse_match::ReverseAlertPageRequest {
        source_id,
        url: url.clone(),
        final_url: url,
        title: Some("Rust consensus engine".into()),
        content_text:
            "A distributed consensus engine implemented in Rust uses replicated logs.".into(),
        content_type: "text/html".into(),
        http_status: 200,
        published_at: None,
        fetched_at: Utc::now(),
        embedding: EmbeddingPayload {
            model: "test-embedding-model".into(),
            model_version: model_version.into(),
            dimensions: 2,
            normalization: VectorNormalization::UnitLength,
            values: vec![1.0, 0.0],
        },
        max_candidates: 10,
    }
}

#[tokio::test]
async fn reverse_match_never_crosses_tenant_boundary() {
    let Some(database) = test_database().await else {
        return;
    };
    let alert_tenant_id = Uuid::new_v4();
    let page_tenant_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();
    let alert_rule_id = Uuid::new_v4();
    let revision_id = Uuid::new_v4();

    insert_source(&database, page_tenant_id, source_id)
        .await
        .unwrap();
    insert_active_rule(
        &database,
        alert_tenant_id,
        alert_rule_id,
        revision_id,
        None,
    )
    .await
    .unwrap();
    upsert_alert_embedding(
        &database,
        alert_tenant_id,
        alert_rule_id,
        revision_id,
    )
    .await;

    let page = page_request(source_id, "v1");
    let response = reverse_match::ingest_and_reverse_match(
        &database,
        page_tenant_id,
        source_id,
        &page,
        &page.url,
        &page.final_url,
    )
    .await
    .unwrap();

    assert!(response.changed);
    assert!(response.candidates.is_empty());
}

#[tokio::test]
async fn reverse_match_applies_source_filter_before_candidate_admission() {
    let Some(database) = test_database().await else {
        return;
    };
    let tenant_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();
    let filtered_source_id = Uuid::new_v4();
    let alert_rule_id = Uuid::new_v4();
    let revision_id = Uuid::new_v4();

    insert_source(&database, tenant_id, source_id).await.unwrap();
    insert_active_rule(
        &database,
        tenant_id,
        alert_rule_id,
        revision_id,
        Some(filtered_source_id),
    )
    .await
    .unwrap();
    upsert_alert_embedding(&database, tenant_id, alert_rule_id, revision_id).await;

    let page = page_request(source_id, "v1");
    let response = reverse_match::ingest_and_reverse_match(
        &database,
        tenant_id,
        source_id,
        &page,
        &page.url,
        &page.final_url,
    )
    .await
    .unwrap();

    assert!(response.changed);
    assert!(response.candidates.is_empty());
}

#[tokio::test]
async fn reverse_match_never_compares_incompatible_model_versions() {
    let Some(database) = test_database().await else {
        return;
    };
    let tenant_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();
    let alert_rule_id = Uuid::new_v4();
    let revision_id = Uuid::new_v4();

    insert_source(&database, tenant_id, source_id).await.unwrap();
    insert_active_rule(&database, tenant_id, alert_rule_id, revision_id, None)
        .await
        .unwrap();
    upsert_alert_embedding(&database, tenant_id, alert_rule_id, revision_id).await;

    let page = page_request(source_id, "v2");
    let response = reverse_match::ingest_and_reverse_match(
        &database,
        tenant_id,
        source_id,
        &page,
        &page.url,
        &page.final_url,
    )
    .await
    .unwrap();

    assert!(response.changed);
    assert!(response.candidates.is_empty());
}
