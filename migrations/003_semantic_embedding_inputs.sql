-- Durable multi-view semantic evidence for page revisions and alert-rule revisions.
--
-- The existing page embedding remains the aggregate compatibility vector used by
-- current search paths. These tables preserve the inputs and per-input vectors
-- required for explainable/native multi-vector ranking without recrawling pages.

CREATE TABLE IF NOT EXISTS eal_semantic_input_sets (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    subject_kind TEXT NOT NULL CHECK (
        subject_kind IN ('page_revision', 'alert_rule_revision')
    ),
    subject_id UUID NOT NULL,
    embedding_space_id UUID NOT NULL,
    extractor_name TEXT NOT NULL CHECK (char_length(extractor_name) BETWEEN 1 AND 120),
    extractor_version TEXT NOT NULL CHECK (char_length(extractor_version) BETWEEN 1 AND 120),
    source_content_sha256 TEXT NOT NULL CHECK (
        source_content_sha256 ~ '^[0-9a-f]{64}$'
    ),
    provenance JSONB NOT NULL DEFAULT '{}'::JSONB CHECK (
        jsonb_typeof(provenance) = 'object'
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (
        tenant_id,
        subject_kind,
        subject_id,
        embedding_space_id,
        extractor_name,
        extractor_version,
        source_content_sha256
    )
);

CREATE TABLE IF NOT EXISTS eal_semantic_inputs (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    input_set_id UUID NOT NULL REFERENCES eal_semantic_input_sets(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (
        kind IN (
            'title',
            'heading',
            'summary',
            'sentence',
            'entity',
            'keyword',
            'url_signal',
            'document',
            'query'
        )
    ),
    ordinal SMALLINT NOT NULL CHECK (ordinal BETWEEN 0 AND 95),
    input_text TEXT NOT NULL CHECK (
        char_length(input_text) BETWEEN 1 AND 700
    ),
    input_text_sha256 TEXT NOT NULL CHECK (
        input_text_sha256 ~ '^[0-9a-f]{64}$'
    ),
    weight REAL NOT NULL CHECK (
        isfinite(weight) AND weight BETWEEN 0.1 AND 2.0
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (input_set_id, ordinal),
    UNIQUE (input_set_id, kind, input_text_sha256)
);

CREATE TABLE IF NOT EXISTS eal_semantic_input_vectors (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    semantic_input_id UUID NOT NULL REFERENCES eal_semantic_inputs(id) ON DELETE CASCADE,
    embedding_space_id UUID NOT NULL,
    embedding VECTOR NOT NULL,
    provider_request_id TEXT,
    generated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (semantic_input_id, embedding_space_id)
);

CREATE INDEX IF NOT EXISTS eal_semantic_input_sets_subject_idx
    ON eal_semantic_input_sets (
        tenant_id,
        subject_kind,
        subject_id,
        embedding_space_id,
        created_at DESC
    );

CREATE INDEX IF NOT EXISTS eal_semantic_inputs_set_kind_idx
    ON eal_semantic_inputs (tenant_id, input_set_id, kind, ordinal);

CREATE INDEX IF NOT EXISTS eal_semantic_input_vectors_space_idx
    ON eal_semantic_input_vectors (
        tenant_id,
        embedding_space_id,
        generated_at DESC
    );

ALTER TABLE eal_semantic_input_sets ENABLE ROW LEVEL SECURITY;
ALTER TABLE eal_semantic_inputs ENABLE ROW LEVEL SECURITY;
ALTER TABLE eal_semantic_input_vectors ENABLE ROW LEVEL SECURITY;

ALTER TABLE eal_semantic_input_sets FORCE ROW LEVEL SECURITY;
ALTER TABLE eal_semantic_inputs FORCE ROW LEVEL SECURITY;
ALTER TABLE eal_semantic_input_vectors FORCE ROW LEVEL SECURITY;

DO $$
BEGIN
    CREATE POLICY eal_semantic_input_sets_tenant_isolation
        ON eal_semantic_input_sets
        USING (
            tenant_id = NULLIF(current_setting('app.tenant_id', TRUE), '')::UUID
        )
        WITH CHECK (
            tenant_id = NULLIF(current_setting('app.tenant_id', TRUE), '')::UUID
        );
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    CREATE POLICY eal_semantic_inputs_tenant_isolation
        ON eal_semantic_inputs
        USING (
            tenant_id = NULLIF(current_setting('app.tenant_id', TRUE), '')::UUID
        )
        WITH CHECK (
            tenant_id = NULLIF(current_setting('app.tenant_id', TRUE), '')::UUID
        );
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    CREATE POLICY eal_semantic_input_vectors_tenant_isolation
        ON eal_semantic_input_vectors
        USING (
            tenant_id = NULLIF(current_setting('app.tenant_id', TRUE), '')::UUID
        )
        WITH CHECK (
            tenant_id = NULLIF(current_setting('app.tenant_id', TRUE), '')::UUID
        );
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

COMMENT ON TABLE eal_semantic_input_sets IS
    'Versioned semantic decompositions for page revisions and immutable alert-rule revisions.';

COMMENT ON TABLE eal_semantic_inputs IS
    'Bounded title, heading, summary, sentence, entity, keyword, URL, document, and query inputs retained as matching evidence.';

COMMENT ON TABLE eal_semantic_input_vectors IS
    'Provider/model-space-specific vectors for individual semantic inputs; aggregate compatibility vectors remain in the existing page embedding table.';
