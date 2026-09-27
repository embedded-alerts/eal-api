use chrono::Utc;
use eal_api::{migrations, reverse_match};
use eal_interfaces::{EmbeddingPayload, VectorNormalization};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement, Value};
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
        .execute(Statement::from_sql_and_values(
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
                'reverse alert test source',
                'https://example.com/',
                '["example.com"]'::jsonb,
                '["/"]'::jsonb,
                '["manual"]'::jsonb,
                TRUE
            )
            "#,
            vec![source_id.to_string().into(), tenant_id.to_string().into()],
        ))
        .await?;
    Ok(())
}

async fn insert_active_rule(
    database: &DatabaseConnection,
    tenant_id: Uuid,
    owner_subject: &str,
    alert_rule_id: Uuid,
    revision_id: Uuid,
    source_filter: Option<Uuid>,
) -> Result<(), sea_orm::DbErr> {
    database
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO eal_alert_rules (id, tenant_id, owner_subject)
            VALUES ($1::uuid, $2::uuid, $3)
            "#,
            vec![
                alert_rule_id.to_string().into(),
                tenant_id.to_string().into(),
                owner_subject.to_owned().into(),
            ],
        ))
        .await?;

    let source_filters = source_filter
        .map(|source_id| format!(r#"["source:{source_id}"]"#))
        .unwrap_or_else(|| "[]".to_owned());
    database
        .execute(Statement::from_sql_and_values(
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
                'reverse alert test rule',
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
                owner_subject.to_owned().into(),
                source_filters.into(),
            ],
        ))
        .await?;

    database
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE eal_alert_rules
            SET active_revision_id = $1::uuid, updated_at = NOW()
            WHERE tenant_id = $2::uuid AND id = $3::uuid
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

async fn scalar_i64(
    database: &DatabaseConnection,
    sql: &str,
    values: Vec<Value>,
) -> Result<i64, sea_orm::DbErr> {
    let row = database
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .await?
        .expect("count query must return one row");
    row.try_get("", "count")
}

#[tokio::test]
async fn transient_reverse_match_creates_durable_candidate_without_page_vector() {
    let Some(database) = test_database().await else {
        return;
    };
    let tenant_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();
    let alert_rule_id = Uuid::new_v4();
    let revision_id = Uuid::new_v4();

    insert_source(&database, tenant_id, source_id)
        .await
        .unwrap();
    insert_active_rule(
        &database,
        tenant_id,
        "reverse-alert-test-owner",
        alert_rule_id,
        revision_id,
        Some(source_id),
    )
    .await
    .unwrap();

    let alert_embedding = reverse_match::AlertRuleEmbeddingUpsertRequest {
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
        &database,
        tenant_id,
        alert_rule_id,
        &alert_embedding,
    )
    .await
    .unwrap();
    assert!(receipt.created);

    let page = reverse_match::ReverseAlertPageRequest {
        source_id,
        url: "https://example.com/rust-consensus".into(),
        final_url: "https://example.com/rust-consensus".into(),
        title: Some("Rust consensus engine".into()),
        content_text: "A new distributed consensus engine implemented in Rust uses replicated logs."
            .into(),
        content_type: "text/html".into(),
        http_status: 200,
        published_at: None,
        fetched_at: Utc::now(),
        embedding: EmbeddingPayload {
            model: "test-embedding-model".into(),
            model_version: "v1".into(),
            dimensions: 2,
            normalization: VectorNormalization::UnitLength,
            values: vec![1.0, 0.0],
        },
        max_candidates: 10,
    };

    let first = reverse_match::ingest_and_reverse_match(
        &database,
        tenant_id,
        source_id,
        &page,
        &page.url,
        &page.final_url,
    )
    .await
    .unwrap();
    assert!(first.changed);
    assert!(!first.page_vector_persisted);
    assert_eq!(first.candidates.len(), 1);
    assert_eq!(first.candidates[0].alert_rule_id, alert_rule_id);
    assert!(first.candidates[0].similarity >= 0.99);

    let page_vector_count = scalar_i64(
        &database,
        "SELECT COUNT(*)::bigint AS count FROM eal_embeddings WHERE tenant_id = $1::uuid AND revision_id = $2::uuid",
        vec![
            tenant_id.to_string().into(),
            first.page_revision_id.to_string().into(),
        ],
    )
    .await
    .unwrap();
    assert_eq!(page_vector_count, 0);

    let match_count = scalar_i64(
        &database,
        "SELECT COUNT(*)::bigint AS count FROM eal_match_candidates WHERE tenant_id = $1::uuid AND revision_id = $2::uuid AND match_mode = 'reverse_alert_stream'",
        vec![
            tenant_id.to_string().into(),
            first.page_revision_id.to_string().into(),
        ],
    )
    .await
    .unwrap();
    assert_eq!(match_count, 1);

    let second = reverse_match::ingest_and_reverse_match(
        &database,
        tenant_id,
        source_id,
        &page,
        &page.url,
        &page.final_url,
    )
    .await
    .unwrap();
    assert!(!second.changed);
    assert_eq!(second.candidates.len(), 1);
    assert_eq!(second.candidates[0].id, first.candidates[0].id);

    let retry_match_count = scalar_i64(
        &database,
        "SELECT COUNT(*)::bigint AS count FROM eal_match_candidates WHERE tenant_id = $1::uuid AND revision_id = $2::uuid AND match_mode = 'reverse_alert_stream'",
        vec![
            tenant_id.to_string().into(),
            first.page_revision_id.to_string().into(),
        ],
    )
    .await
    .unwrap();
    assert_eq!(retry_match_count, 1);
}

#[tokio::test]
async fn durable_activation_boundary_rejects_eleventh_enabled_search() {
    let Some(database) = test_database().await else {
        return;
    };
    let tenant_id = Uuid::new_v4();
    let owner = format!("active-limit-owner-{tenant_id}");

    for index in 0..10_u8 {
        insert_active_rule(
            &database,
            tenant_id,
            &owner,
            Uuid::new_v4(),
            Uuid::new_v4(),
            None,
        )
        .await
        .unwrap_or_else(|error| panic!("active rule {index} must be admitted: {error}"));
    }

    let eleventh = insert_active_rule(
        &database,
        tenant_id,
        &owner,
        Uuid::new_v4(),
        Uuid::new_v4(),
        None,
    )
    .await;
    assert!(eleventh.is_err());

    let active_count = scalar_i64(
        &database,
        r#"
        SELECT COUNT(*)::bigint AS count
        FROM eal_alert_rules AS rule
        JOIN eal_alert_rule_revisions AS revision
          ON revision.tenant_id = rule.tenant_id
         AND revision.id = rule.active_revision_id
        WHERE rule.tenant_id = $1::uuid
          AND rule.owner_subject = $2
          AND revision.enabled = TRUE
        "#,
        vec![tenant_id.to_string().into(), owner.into()],
    )
    .await
    .unwrap();
    assert_eq!(active_count, 10);
}
