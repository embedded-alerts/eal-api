use eal_api::{alert_store, alerts::CreateAlertRule, migrations};
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement, TransactionTrait};
use uuid::Uuid;

fn input(name: &str) -> CreateAlertRule {
    CreateAlertRule {
        name: name.into(),
        query_text: "Notify me when Acme launches renewable energy tools.".into(),
        embedding_model: "fixture-model".into(),
        similarity_threshold: 0.8,
        source_filters: vec!["approved-source".into()],
        delivery_channels: vec!["in_app".into()],
        enabled: true,
    }
    .normalized()
    .expect("valid alert rule")
}

async fn configure_application_role(admin: &sea_orm::DatabaseConnection) {
    admin
        .execute_unprepared(
            r#"
            DO $$
            BEGIN
                CREATE ROLE eal_app_test LOGIN PASSWORD 'eal_app_test';
            EXCEPTION
                WHEN duplicate_object THEN NULL;
            END
            $$;
            ALTER ROLE eal_app_test
                NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOBYPASSRLS;
            GRANT CONNECT ON DATABASE eal_test TO eal_app_test;
            GRANT USAGE ON SCHEMA public TO eal_app_test;
            GRANT SELECT, INSERT, UPDATE, DELETE
                ON ALL TABLES IN SCHEMA public TO eal_app_test;
            "#,
        )
        .await
        .expect("configure non-superuser application role");
}

async fn set_context(
    transaction: &sea_orm::DatabaseTransaction,
    tenant_id: Uuid,
    subject: &str,
    tenant_admin: bool,
) {
    let context_statement = Statement::from_sql_and_values(
        DbBackend::Postgres,
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
    transaction
        .execute_raw(context_statement)
        .await
        .expect("set transaction-local authorization context");
}

#[tokio::test]
async fn alert_rules_survive_restart_and_enforce_tenant_owner_boundaries() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            eprintln!("DATABASE_URL is not configured; skipping PostgreSQL integration test");
            return;
        }
    };
    let app_database_url = std::env::var("APP_DATABASE_URL")
        .expect("APP_DATABASE_URL must name the non-superuser test role");
    let admin = Database::connect(&database_url)
        .await
        .expect("connect migration database");

    let (first, second) = tokio::join!(
        migrations::migrate_all(&admin),
        migrations::migrate_all(&admin),
    );
    first.expect("first migration invocation");
    second.expect("second migration invocation");
    assert!(
        migrations::schema_ready(&admin)
            .await
            .expect("verify migrated schema")
    );
    admin
        .execute_unprepared(
            "TRUNCATE TABLE eal_match_candidates, eal_alert_rule_revisions, eal_alert_rules CASCADE",
        )
        .await
        .expect("reset durable alert-rule tables");
    configure_application_role(&admin).await;

    let database = Database::connect(&app_database_url)
        .await
        .expect("connect through non-superuser application role");
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let owned =
        alert_store::create_alert_rule(&database, tenant_a, "user-a", &input("Owned by user A"))
            .await
            .expect("create first alert rule");
    let other_owner =
        alert_store::create_alert_rule(&database, tenant_a, "user-b", &input("Owned by user B"))
            .await
            .expect("create second alert rule");
    let other_tenant =
        alert_store::create_alert_rule(&database, tenant_b, "user-a", &input("Different tenant"))
            .await
            .expect("create cross-tenant alert rule");

    drop(database);
    let restarted = Database::connect(&app_database_url)
        .await
        .expect("reconnect after simulated restart");

    let user_a = alert_store::list_alert_rules(&restarted, tenant_a, "user-a", false)
        .await
        .expect("list user-owned rules");
    assert_eq!(user_a.len(), 1);
    assert_eq!(user_a[0].id, owned.id);
    assert_eq!(user_a[0].revision_id, owned.revision_id);

    let user_b = alert_store::list_alert_rules(&restarted, tenant_a, "user-b", false)
        .await
        .expect("list second user's rules");
    assert_eq!(user_b.len(), 1);
    assert_eq!(user_b[0].id, other_owner.id);

    let tenant_admin = alert_store::list_alert_rules(&restarted, tenant_a, "admin-a", true)
        .await
        .expect("list tenant rules as administrator");
    assert_eq!(tenant_admin.len(), 2);

    assert!(
        alert_store::get_alert_rule(&restarted, tenant_a, other_owner.id, "user-a", false)
            .await
            .expect("owner-scoped lookup")
            .is_none()
    );
    assert!(
        alert_store::get_alert_rule(&restarted, tenant_b, owned.id, "admin-b", true)
            .await
            .expect("cross-tenant lookup")
            .is_none()
    );
    assert!(
        alert_store::get_alert_rule(&restarted, tenant_b, other_tenant.id, "user-a", false)
            .await
            .expect("same subject in another tenant")
            .is_some()
    );

    let invisible_without_context = restarted
        .query_all_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT id FROM eal_alert_rules".to_owned(),
        ))
        .await
        .expect("query forced-RLS table without tenant context");
    assert!(invisible_without_context.is_empty());

    let member_transaction = restarted.begin().await.expect("begin member RLS test");
    set_context(&member_transaction, tenant_a, "user-a", false).await;
    let member_count: i64 = member_transaction
        .query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT count(*)::bigint AS count FROM eal_alert_rules".to_owned(),
        ))
        .await
        .expect("query member-visible rules")
        .expect("member count row")
        .try_get("", "count")
        .expect("decode member count");
    assert_eq!(member_count, 1);
    member_transaction
        .rollback()
        .await
        .expect("rollback member RLS test");

    let admin_transaction = restarted.begin().await.expect("begin admin RLS test");
    set_context(&admin_transaction, tenant_a, "admin-a", true).await;
    let admin_count: i64 = admin_transaction
        .query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT count(*)::bigint AS count FROM eal_alert_rules".to_owned(),
        ))
        .await
        .expect("query administrator-visible rules")
        .expect("administrator count row")
        .try_get("", "count")
        .expect("decode administrator count");
    assert_eq!(admin_count, 2);
    admin_transaction
        .rollback()
        .await
        .expect("rollback administrator RLS test");

    let transaction = restarted.begin().await.expect("begin immutability test");
    set_context(&transaction, tenant_a, "user-a", false).await;
    let mutation_statement = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE eal_alert_rule_revisions SET name = 'mutated' WHERE id = $1::uuid",
        vec![owned.revision_id.to_string().into()],
    );
    let mutation = transaction.execute_raw(mutation_statement).await;
    assert!(mutation.is_err(), "immutable revisions must reject updates");
    transaction
        .rollback()
        .await
        .expect("rollback immutability test");
}
