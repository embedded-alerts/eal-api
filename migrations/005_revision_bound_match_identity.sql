-- Bind all new logical matches to immutable rule and page revisions.
--
-- Historical rows created before this migration have no trustworthy rule-revision
-- provenance. Preserve those rows as immutable evidence instead of inventing a
-- revision from the rule's current active pointer. The NOT VALID constraints
-- enforce complete provenance for every new or subsequently updated row without
-- rewriting unknown legacy evidence.

ALTER TABLE eal_match_candidates
    ADD COLUMN IF NOT EXISTS alert_rule_revision_id UUID;

DO $$
BEGIN
    ALTER TABLE eal_match_candidates
        ADD CONSTRAINT eal_match_candidates_alert_rule_revision_fk
        FOREIGN KEY (tenant_id, alert_rule_revision_id)
        REFERENCES eal_alert_rule_revisions (tenant_id, id)
        ON DELETE RESTRICT
        NOT VALID;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

DO $$
BEGIN
    ALTER TABLE eal_match_candidates
        ADD CONSTRAINT eal_match_candidates_new_rows_have_rule_revision
        CHECK (alert_rule_revision_id IS NOT NULL)
        NOT VALID;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

CREATE INDEX IF NOT EXISTS eal_match_candidates_rule_revision_status_idx
    ON eal_match_candidates (
        tenant_id,
        alert_rule_id,
        alert_rule_revision_id,
        status,
        created_at DESC,
        id
    );

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

DO $$
BEGIN
    CREATE TRIGGER eal_match_candidates_evidence_immutable
        BEFORE UPDATE OR DELETE ON eal_match_candidates
        FOR EACH ROW
        EXECUTE FUNCTION eal_reject_match_evidence_mutation();
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

COMMENT ON COLUMN eal_match_candidates.alert_rule_revision_id IS
    'Immutable rule revision used to derive the logical match identity; null only for preserved pre-migration evidence.';

COMMENT ON CONSTRAINT eal_match_candidates_new_rows_have_rule_revision
    ON eal_match_candidates IS
    'NOT VALID preserves unknown historical rows while rejecting every new revision-less match.';
