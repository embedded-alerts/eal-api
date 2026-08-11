# Architecture

Rust Axum, SeaORM, and WebSocket API for embedding-driven alert rules, domain-scoped public-page ingestion, explainable semantic matching, and delivery state.

## Runtime planes

1. **Configuration plane** — MASH/HTMX, Leptos, Dioxus, CLI, and SDK clients register immutable alert-rule revisions and explicit source-domain policies.
2. **Discovery plane** — bounded Rust workers inspect seeds, robots-declared sitemaps, conventional sitemaps, feeds, same-domain links, and optional external-index candidate adapters.
3. **Content plane** — pages are canonicalized, public-network checked, fetched, content-hashed, and stored as linked immutable revisions.
4. **Embedding plane** — title, heading, summary, complete-sentence, keyword, proper-noun/entity, and URL-signal views are embedded with explicit model provenance.
5. **Matching plane** — model-compatible vectors are combined with lexical, entity, recency, and source-priority signals; evidence is retained with each candidate.
6. **Delivery plane** — DEN-3460 applies suppression, cooldown, grouping, retries, receipts, and dead-letter behavior before any user notification.

## Fleet

- `eal-interfaces`
- `eal-api`
- `eal-mash-web`
- `eal-leptos-web`
- `eal-dioxus-web`
- `eal-sync`
- `eal-cli`
- `eal-infra`
- `embedded-alerts-clients`
- `embedded-alerts-libs`
- `embedded-alerts.github.io`
- `embedded-alerts-monorepo`

Interfaces own wire formats; libraries own reusable domain behavior; clients consume versioned contracts; runtimes own deployment behavior; monorepos coordinate pinned revisions. Edge code is allowlisted and never a generic proxy.

See [`semantic-indexing.md`](semantic-indexing.md) for the executable DEN-3461 slice and its current production blockers.
