# eal-api

**Embedded Alerts — Rust REST and WebSocket API server**

Embedded Alerts indexes newly published public pages from explicitly configured domains,
stores model-versioned vectors per page revision, and creates explainable match candidates
for registered users. It does not use Next.js.

## Indexing strategy

The authoritative index is owned by Embedded Alerts and restricted by tenant-owned source
policies. RSS/Atom feeds, sitemaps, manual submissions, and external search indexes may
suggest candidate URLs. A candidate is never trusted as a match: it must still pass exact
host/path policy, redirect revalidation, robots policy in the crawler, canonicalization,
content hashing, local embedding generation, and tenant-scoped semantic scoring.

This repository owns the API and durable PostgreSQL/pgvector state. The crawler is a
separate Rust worker so fetch concurrency, DNS rebinding checks, per-host budgets, retry
leases, and content extraction cannot block request-serving tasks.

## Routes

All non-health routes currently require `X-Eal-Tenant-Id: <uuid>`. This is a development
compatibility boundary only; Shared Auth claims must replace it before production.

- `GET|POST /v1/alerts`
- `GET /v1/alerts/{id}`
- `GET|POST /v1/sources`
- `GET /v1/sources/{source_id}`
- `POST /v1/sources/{source_id}/pages`
- `POST /v1/embeddings/search`
- `POST /v1/matches/evaluate`
- `GET /v1/ws`

`/v1/matches/evaluate` creates deterministic candidates only. It never sends a webhook,
email, Slack message, or other notification. DEN-3460 owns cooldowns, grouping, approvals,
retry schedules, provider receipts, and dead-letter replay.

## Database

`migrations/002_domain_scoped_indexing.sql` adds tenant-owned sources, crawl leases, pages,
immutable content revisions, model/version/dimension/normalization-specific embeddings,
and match candidates. It leaves the legacy `alert_documents` prototype untouched.

Set `MIGRATE_ON_STARTUP=true` only for controlled development. Production should run the
same SQL through the deployment migration job before rolling the API.

## Runtime safety boundary

Alert-rule CRUD still uses a process-local map. Startup therefore fails for
`APP_ENV=production` or `APP_ENV=prod`, even when PostgreSQL is connected. The health route
reports `production_ready=false` until DEN-3459 adds durable alert-rule storage and Shared
Auth tenant authorization.

Production enablement additionally requires crawler SSRF/DNS-rebinding canaries, explicit
registered-client claims, restart/isolation tests, and DEN-3460 delivery-state certification.

## Development

```bash
cp .env.example .env
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```
