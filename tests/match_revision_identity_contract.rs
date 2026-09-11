const MIGRATION: &str = include_str!("../migrations/005_revision_bound_match_identity.sql");
const STORE: &str = include_str!("../src/store.rs");
const ROUTES: &str = include_str!("../src/server_routes.rs");

#[test]
fn new_matches_require_a_tenant_bound_immutable_rule_revision() {
    assert!(MIGRATION.contains("ADD COLUMN IF NOT EXISTS alert_rule_revision_id UUID"));
    assert!(MIGRATION.contains("FOREIGN KEY (tenant_id, alert_rule_revision_id)"));
    assert!(MIGRATION.contains("REFERENCES eal_alert_rule_revisions (tenant_id, id)"));
    assert!(MIGRATION.contains("CHECK (alert_rule_revision_id IS NOT NULL)"));
    assert!(MIGRATION.contains("NOT VALID"));
    assert!(MIGRATION.contains("null only for preserved pre-migration evidence"));
    assert!(!MIGRATION.contains("DROP "));
}

#[test]
fn canonical_identity_uses_authorized_rule_page_content_and_model_provenance() {
    for component in [
        "tenant_id",
        "alert_rule_id",
        "alert_rule_revision_id",
        "page_revision_id",
        "content_sha256",
        "model_version",
        "dimensions",
        "normalization",
    ] {
        assert!(
            STORE.contains(component),
            "canonical identity is missing {component}"
        );
    }
    assert!(ROUTES.contains("rule.revision_id"));
}

#[test]
fn retries_select_existing_evidence_instead_of_rewriting_it() {
    assert!(STORE.contains("ON CONFLICT (tenant_id, canonical_match_key)"));
    assert!(STORE.contains("DO NOTHING"));
    assert!(STORE.contains("SELECT candidate.*"));
    assert!(!STORE.contains("DO UPDATE SET\n                        similarity"));

    assert!(MIGRATION.contains("eal_match_candidates_evidence_immutable"));
    assert!(MIGRATION.contains("match evidence cannot be deleted"));
    assert!(MIGRATION.contains("match identity and score evidence are immutable"));
    for protected_column in [
        "NEW.alert_rule_revision_id IS DISTINCT FROM OLD.alert_rule_revision_id",
        "NEW.revision_id IS DISTINCT FROM OLD.revision_id",
        "NEW.embedding_id IS DISTINCT FROM OLD.embedding_id",
        "NEW.similarity IS DISTINCT FROM OLD.similarity",
        "NEW.threshold IS DISTINCT FROM OLD.threshold",
        "NEW.score_explanation IS DISTINCT FROM OLD.score_explanation",
    ] {
        assert!(
            MIGRATION.contains(protected_column),
            "immutable evidence trigger is missing {protected_column}"
        );
    }
}
