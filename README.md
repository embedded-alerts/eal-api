# eal-api

**Embedded Alerts — Rust REST and WebSocket API server**

Embedding-native monitoring and alerting that continuously matches user intents against newly ingested documents, feeds, pages, and streams.

This repository was bootstrapped on 2026-08-04. It is designed as an independently deployable component and as a member of the `eal-monorepo` workspace.

## GitHub target

`embedded-alerts/eal-api`

## Baseline

- Rust 2024 edition for backend and native components.
- Axum HTTP/WebSocket transport.
- Supabase/PostgreSQL configuration through `DATABASE_URL`, `SUPABASE_URL`, and environment-only secrets.
- OpenTelemetry-compatible tracing hooks.
- Docker, Nix, and GitHub Actions entry points.
- Contracts live in `eal-interfaces`; shared behavior lives in `eal-libs`.

### Routes

- `/v1/alerts`
- `/v1/matches`
- `/v1/sources`
- `/v1/embeddings/search`
- `/v1/ws`

## Runtime safety boundary

Alert rule handlers currently use a process-local `HashMap`; a configured PostgreSQL
connection does not make those handlers durable. To prevent an accidental production
deployment from presenting that scaffold as a real alert service, startup now fails
when `APP_ENV` is `production` or `prod`.

Use `APP_ENV=development` or `APP_ENV=test` only for scaffold work. `/healthz` reports
`status=degraded`, `storage_mode=process_local_memory`, and
`production_ready=false` until DEN-3459 replaces the handlers with tenant-owned
SeaORM/PostgreSQL persistence.

Removing this guard is not the completion condition. Production enablement also
requires Shared Auth claim validation, tenant-scoped HTTP and WebSocket authorization,
explicit CORS origins, durable migrations, and restart/isolation canaries.

## Development

```bash
cp .env.example .env 2>/dev/null || true
nix develop  # optional
cargo fmt --check 2>/dev/null || true
cargo test 2>/dev/null || true
```

## Status

Foundation scaffold. Domain behavior, persistence migrations, authentication policy, and production secrets must be reviewed before deployment.
