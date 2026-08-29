use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};

const INDEXING_MIGRATION: &str = include_str!("../migrations/002_domain_scoped_indexing.sql");
const SEMANTIC_INPUT_MIGRATION: &str =
    include_str!("../migrations/003_semantic_embedding_inputs.sql");
const ALERT_RULE_MIGRATION: &str =
    include_str!("../migrations/004_durable_alert_rules_and_authz.sql");
const MATCH_IDENTITY_MIGRATION: &str =
    include_str!("../migrations/005_revision_bound_match_identity.sql");

pub async fn migrate_all(db: &DatabaseConnection) -> Result<(), sea_orm::DbErr> {
    let transaction = db.begin().await?;
    transaction
        .execute_unprepared(
            "SELECT pg_advisory_xact_lock(hashtext('embedded-alerts:eal-api:migrations'))",
        )
        .await?;
    transaction.execute_unprepared(INDEXING_MIGRATION).await?;
    transaction
        .execute_unprepared(SEMANTIC_INPUT_MIGRATION)
        .await?;
    transaction.execute_unprepared(ALERT_RULE_MIGRATION).await?;
    transaction
        .execute_unprepared(MATCH_IDENTITY_MIGRATION)
        .await?;
    transaction.commit().await?;
    Ok(())
}

pub async fn schema_ready(db: &DatabaseConnection) -> Result<bool, sea_orm::DbErr> {
    let row = db
        .query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            r#"
            SELECT
                to_regclass('public.eal_sources') IS NOT NULL
                AND to_regclass('public.eal_page_revisions') IS NOT NULL
                AND to_regclass('public.eal_embeddings') IS NOT NULL
                AND to_regclass('public.eal_match_candidates') IS NOT NULL
                AND to_regclass('public.eal_semantic_input_sets') IS NOT NULL
                AND to_regclass('public.eal_alert_rules') IS NOT NULL
                AND to_regclass('public.eal_alert_rule_revisions') IS NOT NULL
                AND EXISTS (
                    SELECT 1
                    FROM pg_attribute
                    WHERE attrelid = 'public.eal_match_candidates'::regclass
                      AND attname = 'alert_rule_revision_id'
                      AND NOT attisdropped
                )
                AND EXISTS (
                    SELECT 1
                    FROM pg_trigger
                    WHERE tgname = 'eal_alert_rule_revisions_immutable'
                      AND NOT tgisinternal
                )
                AND EXISTS (
                    SELECT 1
                    FROM pg_trigger
                    WHERE tgname = 'eal_match_candidates_evidence_immutable'
                      AND NOT tgisinternal
                ) AS ready
            "#
            .to_owned(),
        ))
        .await?;
    match row {
        Some(row) => row.try_get("", "ready"),
        None => Ok(false),
    }
}
