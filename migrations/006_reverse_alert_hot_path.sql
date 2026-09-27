-- Reverse semantic alerting hot path.
--
-- Real-time Embedded Alerts indexes immutable active alert-rule embeddings and uses
-- each incoming page embedding as a bounded query. Historical Embedded Search may
-- continue to persist page vectors in eal_embeddings, but reverse-stream match
-- evidence must not require a durable page-vector row.

CREATE TABLE IF NOT EXISTS eal_alert_rule_embeddings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    alert_rule_id UUID NOT NULL,
    alert_rule_revision_id UUID NOT NULL,
    model TEXT NOT NULL CHECK (char_length(model) BETWEEN 1 AND 256),
    model_version TEXT NOT NULL CHECK (char_length(model_version) BETWEEN 1 AND 256),
    dimensions INTEGER NOT NULL CHECK (dimensions BETWEEN 1 AND 65535),
    normalization TEXT NOT NULL CHECK (normalization IN ('none', 'l2', 'unit_length')),
    embedding VECTOR NOT NULL,
    embedding_sha256 TEXT NOT NULL CHECK (embedding_sha256 ~ '^[0-9a-f]{64}$'),
    generated_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (tenant_id, id),
    UNIQUE (
        tenant_id,
        alert_rule_revision_id,
        model,
        model_version,
        dimensions,
        normalization
    ),
    CONSTRAINT eal_alert_rule_embeddings_rule_fk
        FOREIGN KEY (tenant_id, alert_rule_id)
        REFERENCES eal_alert_rules (tenant_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT eal_alert_rule_embeddings_revision_fk
        FOREIGN KEY (tenant_id, alert_rule_revision_id)
        REFERENCES eal_alert_rule_revisions (tenant_id, id)
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS eal_alert_rule_embeddings_hot_lookup_idx
    ON eal_alert_rule_embeddings (
        tenant_id,
        model,
        model_version,
        dimensions,
        normalization,
        alert_rule_revision_id,
        id
    );

-- Do not create one global approximate index over mixed model spaces. Dedicated
-- ANN indexes/shards are admitted per certified model space; exact ranking remains
-- the correctness oracle for candidate reranking.
COMMENT ON INDEX eal_alert_rule_embeddings_hot_lookup_idx IS
    'Provenance/routing index for active alert embeddings; ANN indexes are per certified model space, never global across mixed dimensions/models.';

CREATE OR REPLACE FUNCTION eal_reject_alert_embedding_mutation()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'alert-rule embeddings are immutable; create a new rule revision or model-space row';
END
$$;

DO $$
BEGIN
    CREATE TRIGGER eal_alert_rule_embeddings_immutable
        BEFORE UPDATE OR DELETE ON eal_alert_rule_embeddings
        FOR EACH ROW
        EXECUTE FUNCTION eal_reject_alert_embedding_mutation();
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

ALTER TABLE eal_alert_rule_embeddings ENABLE ROW LEVEL SECURITY;
ALTER TABLE eal_alert_rule_embeddings FORCE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS eal_alert_rule_embeddings_tenant_isolation
    ON eal_alert_rule_embeddings;
CREATE POLICY eal_alert_rule_embeddings_tenant_isolation
    ON eal_alert_rule_embeddings
    USING (
        tenant_id = NULLIF(current_setting('app.tenant_id', TRUE), '')::UUID
        AND EXISTS (
            SELECT 1
            FROM eal_alert_rules AS rule
            WHERE rule.tenant_id = eal_alert_rule_embeddings.tenant_id
              AND rule.id = eal_alert_rule_embeddings.alert_rule_id
              AND (
                  rule.owner_subject = NULLIF(current_setting('app.subject', TRUE), '')
                  OR COALESCE(
                      NULLIF(current_setting('app.is_tenant_admin', TRUE), '')::BOOLEAN,
                      FALSE
                  )
              )
        )
    )
    WITH CHECK (
        tenant_id = NULLIF(current_setting('app.tenant_id', TRUE), '')::UUID
        AND EXISTS (
            SELECT 1
            FROM eal_alert_rules AS rule
            WHERE rule.tenant_id = eal_alert_rule_embeddings.tenant_id
              AND rule.id = eal_alert_rule_embeddings.alert_rule_id
              AND (
                  rule.owner_subject = NULLIF(current_setting('app.subject', TRUE), '')
                  OR COALESCE(
                      NULLIF(current_setting('app.is_tenant_admin', TRUE), '')::BOOLEAN,
                      FALSE
                  )
              )
        )
    );

ALTER TABLE eal_match_candidates
    ALTER COLUMN embedding_id DROP NOT NULL;

ALTER TABLE eal_match_candidates
    ADD COLUMN IF NOT EXISTS match_mode TEXT NOT NULL DEFAULT 'historical_page_search'
        CHECK (match_mode IN ('historical_page_search', 'reverse_alert_stream')),
    ADD COLUMN IF NOT EXISTS alert_embedding_id UUID,
    ADD COLUMN IF NOT EXISTS page_embedding_sha256 TEXT,
    ADD COLUMN IF NOT EXISTS page_embedding_model TEXT,
    ADD COLUMN IF NOT EXISTS page_embedding_model_version TEXT,
    ADD COLUMN IF NOT EXISTS page_embedding_dimensions INTEGER,
    ADD COLUMN IF NOT EXISTS page_embedding_normalization TEXT;

DO $$
BEGIN
    ALTER TABLE eal_match_candidates
        ADD CONSTRAINT eal_match_candidates_alert_embedding_fk
        FOREIGN KEY (tenant_id, alert_embedding_id)
        REFERENCES eal_alert_rule_embeddings (tenant_id, id)
        ON DELETE RESTRICT
        NOT VALID;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE eal_match_candidates
        ADD CONSTRAINT eal_match_candidates_embedding_evidence_shape
        CHECK (
            (
                match_mode = 'historical_page_search'
                AND embedding_id IS NOT NULL
            )
            OR
            (
                match_mode = 'reverse_alert_stream'
                AND alert_embedding_id IS NOT NULL
                AND page_embedding_sha256 ~ '^[0-9a-f]{64}$'
                AND char_length(page_embedding_model) BETWEEN 1 AND 256
                AND char_length(page_embedding_model_version) BETWEEN 1 AND 256
                AND page_embedding_dimensions BETWEEN 1 AND 65535
                AND page_embedding_normalization IN ('none', 'l2', 'unit_length')
            )
        )
        NOT VALID;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

CREATE INDEX IF NOT EXISTS eal_match_candidates_reverse_evidence_idx
    ON eal_match_candidates (
        tenant_id,
        alert_embedding_id,
        alert_rule_revision_id,
        revision_id,
        created_at DESC,
        id
    )
    WHERE match_mode = 'reverse_alert_stream';

CREATE OR REPLACE FUNCTION eal_reject_match_evidence_mutation()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'match evidence cannot be deleted';
    END IF;

    IF NEW.tenant_id IS DISTINCT FROM OLD.tenant_id
        OR NEW.alert_rule_id IS DISTINCT FROM OLD.alert_rule_id
        OR NEW.alert_rule_revision_id IS DISTINCT FROM OLD.alert_rule_revision_id
        OR NEW.revision_id IS DISTINCT FROM OLD.revision_id
        OR NEW.embedding_id IS DISTINCT FROM OLD.embedding_id
        OR NEW.match_mode IS DISTINCT FROM OLD.match_mode
        OR NEW.alert_embedding_id IS DISTINCT FROM OLD.alert_embedding_id
        OR NEW.page_embedding_sha256 IS DISTINCT FROM OLD.page_embedding_sha256
        OR NEW.page_embedding_model IS DISTINCT FROM OLD.page_embedding_model
        OR NEW.page_embedding_model_version IS DISTINCT FROM OLD.page_embedding_model_version
        OR NEW.page_embedding_dimensions IS DISTINCT FROM OLD.page_embedding_dimensions
        OR NEW.page_embedding_normalization IS DISTINCT FROM OLD.page_embedding_normalization
        OR NEW.canonical_match_key IS DISTINCT FROM OLD.canonical_match_key
        OR NEW.similarity IS DISTINCT FROM OLD.similarity
        OR NEW.threshold IS DISTINCT FROM OLD.threshold
        OR NEW.score_explanation IS DISTINCT FROM OLD.score_explanation
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION 'match identity and score evidence are immutable';
    END IF;

    RETURN NEW;
END
$$;

COMMENT ON TABLE eal_alert_rule_embeddings IS
    'Immutable model-versioned alert-rule embeddings used by the real-time reverse semantic alert hot path.';

COMMENT ON COLUMN eal_match_candidates.match_mode IS
    'historical_page_search binds to a persisted page embedding; reverse_alert_stream can bind to transient page-vector provenance instead.';

COMMENT ON COLUMN eal_match_candidates.page_embedding_sha256 IS
    'SHA-256 of the canonical transient page-vector float32 byte representation; evidence only, not the vector itself.';
