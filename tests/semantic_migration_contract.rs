const MIGRATION: &str = include_str!("../migrations/003_semantic_embedding_inputs.sql");

#[test]
fn migration_preserves_page_and_query_embedding_evidence() {
    for table in [
        "eal_semantic_input_sets",
        "eal_semantic_inputs",
        "eal_semantic_input_vectors",
    ] {
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

    for semantic_kind in [
        "'title'",
        "'heading'",
        "'summary'",
        "'sentence'",
        "'entity'",
        "'keyword'",
        "'url_signal'",
        "'document'",
        "'query'",
    ] {
        assert!(
            MIGRATION.contains(semantic_kind),
            "missing semantic input kind {semantic_kind}"
        );
    }

    assert!(MIGRATION.contains("subject_kind IN ('page_revision', 'alert_rule_revision')"));
    assert!(MIGRATION.contains("source_content_sha256"));
    assert!(MIGRATION.contains("extractor_version"));
    assert!(MIGRATION.contains("embedding_space_id"));
    assert!(MIGRATION.contains("input_text_sha256"));
    assert!(MIGRATION.contains("current_setting('app.tenant_id', TRUE)"));
}

#[test]
fn semantic_inputs_are_bounded_and_deterministic() {
    assert!(MIGRATION.contains("ordinal BETWEEN 0 AND 95"));
    assert!(MIGRATION.contains("char_length(input_text) BETWEEN 1 AND 700"));
    assert!(MIGRATION.contains("weight BETWEEN 0.1 AND 2.0"));
    assert!(MIGRATION.contains("UNIQUE (input_set_id, ordinal)"));
    assert!(MIGRATION.contains("UNIQUE (input_set_id, kind, input_text_sha256)"));
}
