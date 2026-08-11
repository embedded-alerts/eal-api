# eal-api

**Embedded Alerts — Rust REST and WebSocket API server**

Embedding-native monitoring that matches user intent against newly discovered public pages without requiring literal keyword equality.

This repository is an independently deployable component and a member of the `eal-monorepo` workspace. It uses Rust 2024, Axum, SeaORM/PostgreSQL, pgvector, Supabase/Shared Auth integration points, and versioned contracts shared with the MASH/HTMX, Leptos, Dioxus, CLI, and SDK clients. There is no Next.js runtime.

## GitHub target

`embedded-alerts/eal-api`

## DEN-3461 semantic-index slice

The current branch adds an executable development/test slice for:

- explicit tenant-owned domain allowlists;
- seeds, robots-declared sitemaps, conventional sitemaps, and bounded same-domain link discovery;
- public-network, redirect, content-type, response-size, and canonical-URL policy;
- immutable page revisions with unchanged-content no-op behavior;
- title, heading, summary, complete-sentence, keyword, proper-noun/entity, and URL-signal embedding views;
- remote model-versioned embeddings plus an explicitly development-only deterministic fallback;
- hybrid semantic/lexical/entity/recency/source-priority ranking with evidence;
- deterministic candidate matches that never notify directly;
- an OpenAPI 3.1 document at `/openapi.json`;
- a pgvector/PostgreSQL migration contract for the durable repository.

See [`docs/semantic-indexing.md`](docs/semantic-indexing.md) for the data flow, policy, scoring, and production boundary.

### Routes

- `GET /healthz`
- `GET /openapi.json`
- `GET|POST /v1/alerts`
- `GET /v1/alerts/{id}`
- `GET|POST /v1/sources`
- `POST /v1/sources/{id}/scan`
- `POST /v1/sources/{id}/ingest`
- `GET /v1/pages`
- `POST /v1/embeddings/search`
- `GET /v1/matches`
- `GET /v1/ws`

## Example development flow

Register only the domain that may be indexed:

```bash
curl -sS http://localhost:8080/v1/sources \
  -H 'content-type: application/json' \
  -H 'x-eal-tenant-id: 00000000-0000-0000-0000-000000000001' \
  -d '{
    "name": "Example engineering news",
    "domain": "example.com",
    "include_subdomains": false,
    "seed_urls": ["https://example.com/news"],
    "discovery_modes": ["seed", "robots_sitemap", "sitemap", "link_crawl"],
    "max_pages_per_scan": 25,
    "respect_robots": true
  }'
```

Run one bounded scan using the returned source ID:

```bash
curl -sS -X POST http://localhost:8080/v1/sources/SOURCE_UUID/scan \
  -H 'x-eal-tenant-id: 00000000-0000-0000-0000-000000000001'
```

Search with a complete natural-language intent. Supplying `alert_rule` records deduplicated candidates but does not deliver notifications:

```bash
curl -sS http://localhost:8080/v1/embeddings/search \
  -H 'content-type: application/json' \
  -H 'x-eal-tenant-id: 00000000-0000-0000-0000-000000000001' \
  -d '{
    "query_text": "Notify me when a Colombian renewable-energy project launches a new monitoring platform.",
    "source_ids": ["SOURCE_UUID"],
    "threshold": 0.72,
    "limit": 20,
    "alert_rule": {"id": "ALERT_RULE_UUID", "revision": 1}
  }'
```

## Embedding configuration

Without provider settings, development and tests use `embedded-alerts:feature-hash:development-v1`. This fallback validates extraction, revision, pagination, model-compatibility, and candidate-dedupe behavior; it is not described as a deep semantic model.

Configure an HTTPS embeddings endpoint for actual semantic vectors:

```dotenv
EMBEDDING_ENDPOINT=https://provider.example/v1/embeddings
EMBEDDING_API_KEY=replace-me
EMBEDDING_PROVIDER=provider-name
EMBEDDING_MODEL=model-name
EMBEDDING_MODEL_VERSION=immutable-provider-version
EMBEDDING_DIMENSIONS=1536
```

The endpoint is expected to accept an OpenAI-compatible `{model,input,dimensions,encoding_format}` request and return indexed float vectors. Every vector is validated, L2-normalized, and stored with its model provenance. Cross-model comparison fails rather than silently producing a score.

## Runtime safety boundary

Alert rules, source registrations, page revisions, vectors, and candidate matches are still process-local on this stacked development branch. A configured PostgreSQL connection does not make those handlers durable. Startup fails when `APP_ENV` is `production` or `prod`.

`x-eal-tenant-id` is a development selector, not authentication. Production enablement requires DEN-3459 to provide Shared Auth claim validation, tenant-scoped SeaORM repositories, explicit CORS origins, tenant-filtered WebSocket events, durable migration execution, and restart/isolation canaries. The SQL migration in `migrations/20260810223000_semantic_page_index.sql` is the target persistence contract, not evidence that persistence is already active.

DEN-3460 owns notification suppression, cooldown, grouping, retries, receipts, and dead letters. DEN-3462 owns audited model/threshold migration and historical re-evaluation. This search path creates candidates only.

## Development

```bash
cp .env.example .env
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
python3 scripts/verify_semantic_contract.py
```
