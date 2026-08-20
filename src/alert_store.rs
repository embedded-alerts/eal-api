use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, QueryResult, Statement,
    TransactionTrait, Value,
};
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::{
    alerts::{AlertRule, CreateAlertRule},
    error::HttpError,
};

pub async fn list_alert_rules(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    subject: &str,
    tenant_admin: bool,
) -> Result<Vec<AlertRule>, HttpError> {
    let transaction = db.begin().await?;
    set_request_context(&transaction, tenant_id, subject, tenant_admin).await?;
    let rows = transaction
        .query_all_raw(statement(
            format!(
                r#"
                SELECT {} AS data
                FROM eal_alert_rules AS rule
                JOIN eal_alert_rule_revisions AS revision
                  ON revision.id = rule.active_revision_id
                 AND revision.tenant_id = rule.tenant_id
                WHERE rule.tenant_id = $1::uuid
                  AND ($2::boolean OR rule.owner_subject = $3)
                ORDER BY rule.updated_at DESC, rule.id ASC
                LIMIT 500
                "#,
                alert_rule_json("rule", "revision")
            ),
            vec![
                tenant_id.to_string().into(),
                tenant_admin.into(),
                subject.to_owned().into(),
            ],
        ))
        .await?;
    let result = rows.into_iter().map(decode_json_row).collect();
    transaction.commit().await?;
    result
}

pub async fn get_alert_rule(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    rule_id: Uuid,
    subject: &str,
    tenant_admin: bool,
) -> Result<Option<AlertRule>, HttpError> {
    let transaction = db.begin().await?;
    set_request_context(&transaction, tenant_id, subject, tenant_admin).await?;
    let row = transaction
        .query_one_raw(statement(
            format!(
                r#"
                SELECT {} AS data
                FROM eal_alert_rules AS rule
                JOIN eal_alert_rule_revisions AS revision
                  ON revision.id = rule.active_revision_id
                 AND revision.tenant_id = rule.tenant_id
                WHERE rule.tenant_id = $1::uuid
                  AND rule.id = $2::uuid
                  AND ($3::boolean OR rule.owner_subject = $4)
                "#,
                alert_rule_json("rule", "revision")
            ),
            vec![
                tenant_id.to_string().into(),
                rule_id.to_string().into(),
                tenant_admin.into(),
                subject.to_owned().into(),
            ],
        ))
        .await?;
    let result = row.map(decode_json_row).transpose();
    transaction.commit().await?;
    result
}

pub async fn create_alert_rule(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    subject: &str,
    input: &CreateAlertRule,
) -> Result<AlertRule, HttpError> {
    let rule_id = Uuid::new_v4();
    let revision_id = Uuid::new_v4();
    let source_filters = serde_json::to_string(&input.source_filters)?;
    let delivery_channels = serde_json::to_string(&input.delivery_channels)?;
    let transaction = db.begin().await?;
    set_request_context(&transaction, tenant_id, subject, false).await?;
    let row = transaction
        .query_one_raw(statement(
            format!(
                r#"
                WITH inserted_rule AS (
                    INSERT INTO eal_alert_rules (
                        id,
                        tenant_id,
                        owner_subject
                    )
                    VALUES ($1::uuid, $2::uuid, $3)
                    RETURNING *
                ),
                inserted_revision AS (
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
                    SELECT
                        $4::uuid,
                        rule.tenant_id,
                        rule.id,
                        1,
                        $3,
                        $5,
                        $6,
                        $7,
                        $8::real,
                        $9::jsonb,
                        $10::jsonb,
                        $11
                    FROM inserted_rule AS rule
                    RETURNING *
                ),
                activated_rule AS (
                    UPDATE eal_alert_rules AS rule
                    SET
                        active_revision_id = revision.id,
                        updated_at = now()
                    FROM inserted_revision AS revision
                    WHERE rule.id = $1::uuid
                      AND rule.tenant_id = $2::uuid
                    RETURNING rule.*
                )
                SELECT {} AS data
                FROM activated_rule AS rule
                JOIN inserted_revision AS revision
                  ON revision.id = rule.active_revision_id
                "#,
                alert_rule_json("rule", "revision")
            ),
            vec![
                rule_id.to_string().into(),
                tenant_id.to_string().into(),
                subject.to_owned().into(),
                revision_id.to_string().into(),
                input.name.clone().into(),
                input.query_text.clone().into(),
                input.embedding_model.clone().into(),
                input.similarity_threshold.into(),
                source_filters.into(),
                delivery_channels.into(),
                input.enabled.into(),
            ],
        ))
        .await?;
    let result = row
        .map(decode_json_row)
        .transpose()?
        .ok_or_else(|| HttpError::internal("alert-rule creation returned no durable record"));
    transaction.commit().await?;
    result
}

pub async fn require_alert_rule(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    rule_id: Uuid,
    subject: &str,
    tenant_admin: bool,
) -> Result<AlertRule, HttpError> {
    get_alert_rule(db, tenant_id, rule_id, subject, tenant_admin)
        .await?
        .ok_or_else(|| HttpError::not_found("alert rule was not found"))
}

async fn set_request_context(
    transaction: &DatabaseTransaction,
    tenant_id: Uuid,
    subject: &str,
    tenant_admin: bool,
) -> Result<(), sea_orm::DbErr> {
    let context_statement = statement(
        r#"
        SELECT
            set_config('app.tenant_id', $1, true),
            set_config('app.subject', $2, true),
            set_config('app.is_tenant_admin', $3, true)
        "#,
        vec![
            tenant_id.to_string().into(),
            subject.to_owned().into(),
            tenant_admin.to_string().into(),
        ],
    );
    transaction.execute(&context_statement).await?;
    Ok(())
}

fn alert_rule_json(rule: &str, revision: &str) -> String {
    format!(
        r#"json_build_object(
            'id', {rule}.id,
            'tenant_id', {rule}.tenant_id,
            'owner_subject', {rule}.owner_subject,
            'revision_id', {revision}.id,
            'revision_number', {revision}.revision_number,
            'created_at', {rule}.created_at,
            'updated_at', {rule}.updated_at,
            'revision_created_at', {revision}.created_at,
            'name', {revision}.name,
            'query_text', {revision}.query_text,
            'embedding_model', {revision}.embedding_model,
            'similarity_threshold', {revision}.similarity_threshold,
            'source_filters', {revision}.source_filters,
            'delivery_channels', {revision}.delivery_channels,
            'enabled', {revision}.enabled
        )::text"#
    )
}

fn statement(sql: impl Into<String>, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

fn decode_json_row<T: DeserializeOwned>(row: QueryResult) -> Result<T, HttpError> {
    let data: String = row.try_get("", "data")?;
    Ok(serde_json::from_str(&data)?)
}
