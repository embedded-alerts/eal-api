-- Product guardrail: one owner subject may have at most ten enabled active
-- embedded searches. Enforce this at the durable active-revision pointer so
-- every API/client path shares the same concurrency-safe rule.

CREATE OR REPLACE FUNCTION eal_enforce_active_alert_limit()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
DECLARE
    new_revision_enabled BOOLEAN;
    active_count INTEGER;
BEGIN
    IF NEW.active_revision_id IS NULL THEN
        RETURN NEW;
    END IF;

    -- Serialize activations for one tenant/owner without globally locking rules.
    PERFORM pg_advisory_xact_lock(
        hashtextextended(NEW.tenant_id::TEXT || ':' || NEW.owner_subject, 0)
    );

    SELECT revision.enabled
    INTO new_revision_enabled
    FROM eal_alert_rule_revisions AS revision
    WHERE revision.tenant_id = NEW.tenant_id
      AND revision.id = NEW.active_revision_id
      AND revision.alert_rule_id = NEW.id;

    IF new_revision_enabled IS DISTINCT FROM TRUE THEN
        RETURN NEW;
    END IF;

    SELECT COUNT(*)::INTEGER
    INTO active_count
    FROM eal_alert_rules AS rule
    JOIN eal_alert_rule_revisions AS revision
      ON revision.tenant_id = rule.tenant_id
     AND revision.id = rule.active_revision_id
     AND revision.alert_rule_id = rule.id
    WHERE rule.tenant_id = NEW.tenant_id
      AND rule.owner_subject = NEW.owner_subject
      AND rule.id <> NEW.id
      AND revision.enabled = TRUE;

    IF active_count >= 10 THEN
        RAISE EXCEPTION USING
            ERRCODE = 'check_violation',
            MESSAGE = 'an owner may have at most 10 enabled active embedded searches';
    END IF;

    RETURN NEW;
END
$$;

DROP TRIGGER IF EXISTS eal_alert_rules_active_limit
    ON eal_alert_rules;
CREATE TRIGGER eal_alert_rules_active_limit
    BEFORE INSERT OR UPDATE OF active_revision_id
    ON eal_alert_rules
    FOR EACH ROW
    EXECUTE FUNCTION eal_enforce_active_alert_limit();

COMMENT ON FUNCTION eal_enforce_active_alert_limit() IS
    'Concurrency-safe ten-active-alert product guardrail keyed by tenant and owner subject.';
