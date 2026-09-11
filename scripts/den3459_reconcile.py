#!/usr/bin/env python3
"""One-shot, idempotent reconciliation for DEN-3459 on the existing PR branch."""

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if new in text:
        return text
    if old not in text:
        raise SystemExit(f"missing reconciliation marker: {label}")
    return text.replace(old, new, 1)


def patch_alert_store() -> None:
    path = ROOT / "src" / "alert_store.rs"
    text = path.read_text()
    text = replace_once(
        text,
        "                revision_number,\n                created_by_subject,\n                name,",
        "                revision_number,\n                created_by_subject,\n                owner_subject,\n                name,",
        "revision owner column",
    )
    text = replace_once(
        text,
        "                1,\n                $4,\n                $5,",
        "                1,\n                $4,\n                $4,\n                $5,",
        "revision owner value",
    )
    path.write_text(text)


def patch_migration() -> None:
    path = ROOT / "migrations" / "004_durable_alert_rules_and_authz.sql"
    text = path.read_text()
    if "owner_subject" not in text.split("CREATE TABLE IF NOT EXISTS eal_alert_rule_revisions", 1)[1].split(");", 1)[0]:
        markers = [
            (
                "created_by_subject text not null,",
                "created_by_subject text not null,\n    owner_subject text not null check (char_length(owner_subject) between 1 and 256),",
            ),
            (
                "created_by_subject TEXT NOT NULL,",
                "created_by_subject TEXT NOT NULL,\n    owner_subject TEXT NOT NULL CHECK (char_length(owner_subject) BETWEEN 1 AND 256),",
            ),
        ]
        for old, new in markers:
            if old in text:
                text = text.replace(old, new, 1)
                break
        else:
            raise SystemExit("missing created_by_subject migration marker")

    if "eal_alert_rule_revisions_tenant_owner_idx" not in text:
        text = text.rstrip() + r'''

-- Immutable revisions carry direct ownership evidence so forced RLS evaluation does
-- not depend on a cross-table lookup during the revision INSERT statement snapshot.
CREATE INDEX IF NOT EXISTS eal_alert_rule_revisions_tenant_owner_idx
    ON eal_alert_rule_revisions (
        tenant_id,
        owner_subject,
        alert_rule_id,
        revision_number DESC,
        id
    );

DO $$
DECLARE
    policy_record RECORD;
BEGIN
    FOR policy_record IN
        SELECT policyname
        FROM pg_policies
        WHERE schemaname = current_schema()
          AND tablename = 'eal_alert_rule_revisions'
    LOOP
        EXECUTE format(
            'DROP POLICY %I ON eal_alert_rule_revisions',
            policy_record.policyname
        );
    END LOOP;
END
$$;

CREATE POLICY eal_alert_rule_revisions_tenant_owner_access
    ON eal_alert_rule_revisions
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

ALTER TABLE eal_alert_rule_revisions ENABLE ROW LEVEL SECURITY;
ALTER TABLE eal_alert_rule_revisions FORCE ROW LEVEL SECURITY;

COMMENT ON COLUMN eal_alert_rule_revisions.owner_subject IS
    'Immutable copy of the owning subject used for direct forced-RLS evaluation.';
''' + "\n"
    path.write_text(text)


def write_contract_test() -> None:
    path = ROOT / "tests" / "alert_revision_owner_rls_contract.rs"
    path.write_text(
        r'''const MIGRATION: &str = include_str!("../migrations/004_durable_alert_rules_and_authz.sql");
const STORE: &str = include_str!("../src/alert_store.rs");

#[test]
fn immutable_revisions_carry_direct_owner_evidence_for_forced_rls() {
    assert!(MIGRATION.contains("owner_subject"));
    assert!(MIGRATION.contains("CREATE POLICY eal_alert_rule_revisions_tenant_owner_access"));
    assert!(MIGRATION.contains("owner_subject = NULLIF(current_setting('app.subject', TRUE), '')"));
    assert!(MIGRATION.contains("ALTER TABLE eal_alert_rule_revisions FORCE ROW LEVEL SECURITY"));
    assert!(STORE.contains("created_by_subject,\n                owner_subject,"));
    assert!(STORE.contains("1,\n                $4,\n                $4,"));
}
'''
    )


patch_alert_store()
patch_migration()
write_contract_test()
