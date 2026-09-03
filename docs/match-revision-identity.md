# Revision-bound logical match identity

This change is a stacked dependency on the durable alert-rule and authorization
work in draft PR #8. It does not replace that branch or make sense without its
tenant-owned immutable rule revisions.

## Canonical identity

Every newly evaluated candidate is identified by a SHA-256 digest over the
canonical text representation of:

1. tenant ID;
2. alert-rule identity;
3. immutable alert-rule revision ID;
4. immutable page revision ID;
5. normalized content SHA-256;
6. embedding model and model version;
7. embedding dimensions; and
8. vector-normalization policy.

All fields are serialized as a fixed-order JSON string array before hashing, so
embedded punctuation cannot create an ambiguous boundary. A retry with the same
provenance produces the same key. A change to the rule revision, page revision,
normalized content, model version, dimensions, or normalization policy produces
a different key. The active rule is loaded through the existing authorized
owner-or-tenant-admin path, and that exact revision ID is passed into candidate
persistence.

The similarity threshold is not a separate hash field because it is immutable
input owned by the rule revision. Candidate evaluation may raise the effective
request threshold but cannot lower the stored revision threshold.

## Retry and evidence behavior

Candidate insertion uses the unique `(tenant_id, canonical_match_key)` boundary.
On conflict, the API selects the existing row only when every stored provenance
identifier also matches. It does not update similarity, threshold, explanation,
or timestamps. A cryptographic-key collision with different provenance returns
no row and fails closed.

The database trigger rejects deletion and changes to identity or score evidence.
It deliberately permits later status and operational timestamp transitions so a
separate DEN-3460 delivery state machine can advance a candidate without
rewriting why the match originally existed.

## Legacy migration policy

Pre-migration candidates do not record which rule revision was active. Assigning
the rule's current active revision would manufacture historical evidence, so the
migration adds a nullable column and leaves those rows untouched.

The tenant-bound foreign key and non-null check are both `NOT VALID`. PostgreSQL
therefore preserves existing unknown rows but enforces both constraints for
every new or subsequently updated row. The immutability trigger freezes legacy
unknowns as retained evidence. Operators may classify them through a future,
audited migration plan; this migration never guesses.

Deployment is additive and roll-forward only:

1. apply migrations 002 through 005 under the existing advisory lock;
2. verify the revision column and evidence trigger through the startup schema
   readiness check;
3. deploy the API revision that always supplies `alert_rule_revision_id`;
4. observe candidate insert failures before enabling any delivery worker.

Do not remove the column, constraint, foreign key, or trigger as a rollback.
Roll forward with a reviewed migration while retaining match evidence.

## Explicit remaining work

This slice establishes logical-match provenance and retry deduplication only. It
does not implement cooldowns, grouping, escalation, notification outboxes,
provider attempts and receipts, dead letters, manual replay, or alert-storm
limits. It also does not implement DEN-3462 change classification, dry-run impact
estimation, approval thresholds, backfill plans, checkpoints, pause/resume,
cancel, or rollback orchestration. No external delivery is enabled.
