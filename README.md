# eal-api

**Embedded Alerts — Rust REST and WebSocket API server**

Embedded Alerts indexes newly published public pages from explicitly configured domains,
stores model-versioned vectors per immutable page revision, and creates explainable match
candidates for tenant-owned alert rules. It does not use Next.js.

## Durable ownership and authorization

Alert rules are durable PostgreSQL identities with an explicit active immutable revision.
Every rule records its tenant and owner subject. Normal members can list, read, evaluate,
and receive events only for rules they own; configured tenant administrators can operate
across their tenant. Cross-tenant lookups return no record.

User and browser routes accept a verified HS256 JWT from either a single `Authorization:
Bearer` header or the configured cookie. Verification is fail-closed and checks the
signature, exact algorithm, issuer, audience, subject, expiration, optional not-before and
issued-at times, tenant claim, role claim, and allowed/admin role sets. Supplying conflicting
header and cookie credentials is rejected. Browser deployments should set the JWT cookie as
`HttpOnly`, `Secure`, and an appropriate `SameSite` value at the authentication boundary.

`ALLOW_INSECURE_TENANT_HEADER=true` is an explicit development/test compatibility mode only.
It requires all three headers on each non-health request:

- `X-Eal-Tenant-Id: <uuid>`
- `X-Eal-Subject-Id: <stable authenticated subject>`
- `X-Eal-Roles: member` or a configured administrator role

Production never falls back to those headers. Startup requires a database connection, the
complete durable schema, verified JWT configuration, authenticated crawler-ingest digest,
and explicit browser origins.

## WebSocket boundary

`GET /v1/ws` uses the same verified identity as HTTP routes and independently validates the
WebSocket `Origin` against `CORS_ALLOWED_ORIGINS` before upgrading. Events carry an internal
audience and are emitted only to the owning subject or tenant administrators. Tenant-admin
operational events, such as source or page-index changes, are not sent to ordinary members.
Cross-tenant events are discarded before serialization to the socket.

The in-process broadcast channel is an authorized live-update transport, not durable event
evidence. Durable rules, revisions, pages, embeddings, and match candidates remain in
PostgreSQL; a future multi-replica event transport must preserve the same tenant/subject
audience contract.

## Indexing strategy

The authoritative index is owned by Embedded Alerts and restricted by tenant-owned source
policies. RSS/Atom feeds, sitemaps, manual submissions, and external search indexes may
suggest candidate URLs. A candidate is never trusted as a match: it must still pass exact
host/path policy, redirect revalidation, robots policy in the crawler, canonicalization,
content hashing, local embedding generation, and tenant-scoped semantic scoring.

This repository owns the API and durable PostgreSQL/pgvector state. The crawler is a
separate Rust worker so fetch concurrency, DNS-rebinding checks, per-host budgets, retry
leases, and content extraction cannot block request-serving tasks. Its page-ingest route
uses a separate high-entropy worker credential; the API stores only its SHA-256 digest.

## Routes

- `GET|POST /v1/alerts`
- `GET /v1/alerts/{id}`
- `GET|POST /v1/sources`
- `GET /v1/sources/{source_id}`
- `POST /v1/sources/{source_id}/pages`
- `POST /v1/embeddings/search`
- `POST /v1/matches/evaluate`
- `GET /v1/ws`

Creating source policies requires a tenant-administrator role. Match evaluation first loads
an authorized durable rule, requires the search model to match the rule’s active revision,
and cannot lower the stored similarity threshold. It creates deterministic candidates only;
it never sends a webhook, email, Slack message, or other notification. DEN-3460 owns
cooldowns, grouping, approvals, retry schedules, provider receipts, and dead-letter replay.

## Database

The migration stack is serialized with a PostgreSQL transaction advisory lock:

- `migrations/002_domain_scoped_indexing.sql` adds sources, crawl leases, pages, immutable
  page revisions, provenance-specific embeddings, and match candidates.
- `migrations/003_semantic_embedding_inputs.sql` preserves bounded multi-view semantic
  inputs and vectors for pages and alert-rule revisions.
- `migrations/004_durable_alert_rules_and_authz.sql` adds tenant-owned alert-rule identities,
  immutable revisions, owner/admin RLS policies, and a tenant-bound candidate foreign key.

Set `MIGRATE_ON_STARTUP=true` only for controlled development. Production should run the same
SQL through the deployment migration job before rolling the API. Startup verifies the
required tables and immutability trigger instead of treating a successful TCP database
connection as proof that durable storage exists.

The database integration suite runs migrations concurrently, reconnects to simulate a
restart, and exercises ownership and forced RLS through a non-superuser application role.
It also proves that immutable revisions reject mutation.

## Development

```bash
cp .env.example .env
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

## Environment secrets

Secrets live in this repo **encrypted** with [sops](https://github.com/getsops/sops) + [age](https://github.com/FiloSottile/age):
`env/enc/<dev|prod>.env.enc` is committed; `just env-use <name>` decrypts it to
`env/dec/<name>.env` (gitignored, mode 0600) and symlinks `./.env` to it. The
Nix dev shell provides the tooling, `just env-audit` runs keyless in CI, and
containers decrypt at `docker run` — never at build. See [`env/README.md`](env/README.md).
