use chrono::Utc;
use eal_api::{migrations, reverse_match};
use eal_interfaces::{EmbeddingPayload, VectorNormalization};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use uuid::Uuid;

const MODEL: &str = "test-embedding-model";
const MODEL_VERSION: &str = "v1";

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
                format!("threshold-source-{source_id}").into(),
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
    similarity_threshold: f64,
    embedding: Vec<f32>,
) -> Result<(), Box<dyn std::error::Error>> {
    let owner_subject = format!("threshold-owner-{alert_rule_id}");
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
                'threshold admission rule',
                'semantic threshold admission',
                $5,
                $6,
                '[]'::jsonb,
                '["in_app"]'::jsonb,
                TRUE
            )
            "#,
            vec![
                revision_id.to_string().into(),
                tenant_id.to_string().into(),
                alert_rule_id.to_string().into(),
                owner_subject.into(),
                MODEL.into(),
                similarity_threshold.into(),
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

    let request = reverse_match::AlertRuleEmbeddingUpsertRequest {
        alert_rule_revision_id: revision_id,
        embedding: EmbeddingPayload {
            model: MODEL.into(),
            model_version: MODEL_VERSION.into(),
            dimensions: 2,
            normalization: VectorNormalization::UnitLength,
            values: embedding,
        },
    };
    reverse_match::upsert_alert_rule_embedding(database, tenant_id, alert_rule_id, &request)
        .await?;
    Ok(())
}

#[tokio::test]
async fn nonqualifying_nearest_neighbor_cannot_consume_top_k_slot() {
    let Some(database) = test_database().await else {
        return;
    };

    let tenant_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();
    insert_source(&database, tenant_id, source_id)
        .await
        .unwrap();

    let nearer_but_rejected_rule_id = Uuid::new_v4();
    insert_active_rule(
        &database,
        tenant_id,
        nearer_but_rejected_rule_id,
        Uuid::new_v4(),
        0.99,
        vec![0.95, 0.312_249_9],
    )
    .await
    .unwrap();

    let qualifying_rule_id = Uuid::new_v4();
    insert_active_rule(
        &database,
        tenant_id,
        qualifying_rule_id,
        Uuid::new_v4(),
        0.85,
        vec![0.90, 0.435_889_9],
    )
    .await
    .unwrap();

    let url = format!("https://example.com/{source_id}/threshold-admission");
    let page = reverse_match::ReverseAlertPageRequest {
        source_id,
        url: url.clone(),
        final_url: url,
        title: Some("Threshold admission".into()),
        content_text: "A semantic page that should reach the qualifying saved alert.".into(),
        content_type: "text/html".into(),
        http_status: 200,
        published_at: None,
        fetched_at: Utc::now(),
        embedding: EmbeddingPayload {
            model: MODEL.into(),
            model_version: MODEL_VERSION.into(),
            dimensions: 2,
            normalization: VectorNormalization::UnitLength,
            values: vec![1.0, 0.0],
        },
        max_candidates: 1,
    };

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

    assert_eq!(response.candidates.len(), 1);
    assert_eq!(response.candidates[0].alert_rule_id, qualifying_rule_id);
    assert!(response.candidates[0].similarity >= response.candidates[0].threshold);
    assert_ne!(
        response.candidates[0].alert_rule_id,
        nearer_but_rejected_rule_id
    );
}
