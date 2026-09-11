const MIGRATION: &str = include_str!("../migrations/004_durable_alert_rules_and_authz.sql");

#[test]
fn durable_alert_rules_have_ownership_revisions_and_forced_rls() {
    for table in ["eal_alert_rules", "eal_alert_rule_revisions"] {
        assert!(
            MIGRATION.contains(&format!("CREATE TABLE IF NOT EXISTS {table}")),
            "missing table {table}"
        );
        assert!(
            MIGRATION.contains(&format!("ALTER TABLE {table} ENABLE ROW LEVEL SECURITY")),
            "missing RLS for {table}"
        );
        assert!(
            MIGRATION.contains(&format!("ALTER TABLE {table} FORCE ROW LEVEL SECURITY")),
            "missing forced RLS for {table}"
        );
    }
    assert!(MIGRATION.contains("owner_subject"));
    assert!(MIGRATION.contains("active_revision_id"));
    assert!(MIGRATION.contains("revision_number"));
    assert!(MIGRATION.contains("created_by_subject"));
    assert!(MIGRATION.contains("alert-rule revisions are immutable"));
    assert!(MIGRATION.contains("current_setting('app.tenant_id', TRUE)"));
    assert!(MIGRATION.contains("current_setting('app.subject', TRUE)"));
    assert!(MIGRATION.contains("current_setting('app.is_tenant_admin', TRUE)"));
    assert!(MIGRATION.contains("eal_match_candidates_alert_rule_fk"));
    assert!(MIGRATION.contains("FOREIGN KEY (tenant_id, alert_rule_id)"));
}
