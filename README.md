# eal-api

Axum REST and WebSocket API server for Embedded Alerts.

**Product:** Embedded Alerts — Embedding-based alerting for semantically relevant new information.

Define semantic alert rules, ingest source documents, compare embeddings, rank matches, and deliver explainable notifications.

## Safety and production boundary

Similarity scores are ranking signals, not truth guarantees. Production ingestion must respect source terms, robots rules, privacy requirements, retention limits, and notification consent.

This repository is an executable bootstrap, not a production deployment. Before live
use, add authentication, tenant authorization, rate limits, durable migrations,
observability, backups, incident response, dependency review, and secret management.
## Routes

- `GET /healthz`, `GET /readyz`, `GET /metrics`
- `GET|POST /api/v1/alert-rules`
- `GET /api/v1/alert-rules/{id}`
- `GET /ws` for JSON event envelopes

The bootstrap uses bounded in-memory state so transport behavior is immediately
testable. Replace it with SeaORM/PostgreSQL transactions before production and keep
`eal-interfaces` as the tagged wire-contract authority.

```bash
cargo run
```
