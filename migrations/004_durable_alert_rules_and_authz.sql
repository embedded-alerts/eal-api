-- Durable, tenant-owned alert rules and immutable revisions.
-- Application queries set app.tenant_id, app.subject, and app.is_tenant_admin
-- inside a transaction before touching these tables.

CREATE TABLE IF NOT EXISTS eal_alert_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    owner_subject TEXT NOT NULL CHECK (char_length(owner_subject) BETWEEN 1 AND 256),
    active_revision_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (tenant_id, id)
);

CREATE TABLE IF NOT EXISTS eal_alert_rule_revisions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    alert_rule_id UUID NOT NULL,
    revision_number INTEGER NOT NULL CHECK (revision_number > 0),
    created_by_subject TEXT NOT NULL CHECK (char_length(created_by_subject) BETWEEN 1 AND 256),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 256),
    query_text TEXT NOT NULL CHECK (char_length(query_text) BETWEEN 3 AND 700),
    embedding_model TEXT NOT NULL CHECK (char_length(embedding_model) BETWEEN 1 AND 160),
    similarity_threshold REAL NOT NULL CHECK (
        similarity_threshold >= 0.0 AND similarity_threshold <= 1.0
    ),
    source_filters JSONB NOT NULL DEFAULT '[]'::JSONB CHECK (
        jsonb_typeof(source_filters) = 'array'
        AND jsonb_array_length(source_filters) <= 128
    ),
    delivery_channels JSONB NOT NULL DEFAULT '[]'::JSONB CHECK (
        jsonb_typeof(delivery_channels) = 'array'
        AND jsonb_array_length(delivery_channels) <= 32
    ),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, alert_rule_id, revision_number),
    CONSTRAINT eal_alert_rule_revisions_rule_fk
        FOREIGN KEY (tenant_id, alert_rule_id)
        REFERENCES eal_alert_rules (tenant_id, id)
        ON DELETE RESTRICT
);

DO $$
BEGIN
    ALTER TABLE eal_alert_rules
        ADD CONSTRAINT eal_alert_rules_active_revision_fk
        FOREIGN KEY (tenant_id, active_revision_id)
        REFERENCES eal_alert_rule_revisions (tenant_id, id)
        ON DELETE RESTRICT;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

CREATE INDEX IF NOT EXISTS eal_alert_rules_tenant_owner_idx
    ON eal_alert_rules (tenant_id, owner_subject, updated_at DESC, id);

CREATE INDEX IF NOT EXISTS eal_alert_rule_revisions_rule_idx
    ON eal_alert_rule_revisions (
        tenant_id,
        alert_rule_id,
        revision_number DESC,
        created_at DESC,
        id
    );

CREATE OR REPLACE FUNCTION eal_reject_alert_rule_revision_mutation()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'alert-rule revisions are immutable';
END
$$;

DROP TRIGGER IF EXISTS eal_alert_rule_revisions_immutable
    ON eal_alert_rule_revisions;
CREATE TRIGGER eal_alert_rule_revisions_immutable
    BEFORE UPDATE OR DELETE ON eal_alert_rule_revisions
    FOR EACH ROW
    EXECUTE FUNCTION eal_reject_alert_rule_revision_mutation();

ALTER TABLE eal_alert_rules ENABLE ROW LEVEL SECURITY;
ALTER TABLE eal_alert_rule_revisions ENABLE ROW LEVEL SECURITY;
ALTER TABLE eal_alert_rules FORCE ROW LEVEL SECURITY;
ALTER TABLE eal_alert_rule_revisions FORCE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS eal_alert_rules_tenant_isolation ON eal_alert_rules;
CREATE POLICY eal_alert_rules_tenant_isolation
    ON eal_alert_rules
    USING (
        tenant_id = NULLIF(current_setting('app.tenant_id', TRUE), '')::UUID
        AND (
            owner_subject = NULLIF(current_setting('app.subject', TRUE), '')
            OR COALESCE(
                NULLIF(current_setting('app.is_tenant_admin', TRUE), '')::BOOLEAN,
                FALSE
            )
        )
    )
    WITH CHECK (
        tenant_id = NULLIF(current_setting('app.tenant_id', TRUE), '')::UUID
        AND (
            owner_subject = NULLIF(current_setting('app.subject', TRUE), '')
            OR COALESCE(
                NULLIF(current_setting('app.is_tenant_admin', TRUE), '')::BOOLEAN,
                FALSE
            )
        )
    );

DROP POLICY IF EXISTS eal_alert_rule_revisions_tenant_isolation
    ON eal_alert_rule_revisions;
CREATE POLICY eal_alert_rule_revisions_tenant_isolation
    ON eal_alert_rule_revisions
    USING (
        tenant_id = NULLIF(current_setting('app.tenant_id', TRUE), '')::UUID
        AND EXISTS (
            SELECT 1
            FROM eal_alert_rules AS rule
            WHERE rule.tenant_id = eal_alert_rule_revisions.tenant_id
              AND rule.id = eal_alert_rule_revisions.alert_rule_id
        )
    )
    WITH CHECK (
        tenant_id = NULLIF(current_setting('app.tenant_id', TRUE), '')::UUID
        AND EXISTS (
            SELECT 1
            FROM eal_alert_rules AS rule
            WHERE rule.tenant_id = eal_alert_rule_revisions.tenant_id
              AND rule.id = eal_alert_rule_revisions.alert_rule_id
        )
    );

DO $$
BEGIN
    ALTER TABLE eal_match_candidates
        ADD CONSTRAINT eal_match_candidates_alert_rule_fk
        FOREIGN KEY (tenant_id, alert_rule_id)
        REFERENCES eal_alert_rules (tenant_id, id)
        ON DELETE RESTRICT
        NOT VALID;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

COMMENT ON TABLE eal_alert_rules IS
    'Tenant-owned alert-rule identities with an explicit active immutable revision.';

COMMENT ON TABLE eal_alert_rule_revisions IS
    'Immutable natural-language alert-rule revisions, including model and delivery-policy inputs.';
