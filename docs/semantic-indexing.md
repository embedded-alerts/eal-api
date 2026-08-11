# Semantic page indexing

This slice implements the first executable DEN-3461 contract. It intentionally does **not** turn Embedded Alerts into an unrestricted crawler or a notification sender.

## Product boundary

A tenant registers an explicit public DNS domain. Each source controls:

- exact-host versus subdomain inclusion;
- seed URLs;
- discovery modes;
- per-scan page budget;
- source-priority contribution;
- robots.txt enforcement;
- enabled/disabled state.

Only `http` and `https` are accepted. Credentials in URLs, IP-literal source registrations, localhost/internal names, off-domain redirects, non-public resolved addresses, unsupported content types, and oversized bodies are rejected. DNS results are checked and pinned into the HTTP client for the request; system proxies and automatic redirects are disabled.

## Discovery strategy

The first scanner combines bounded, source-controlled discovery:

1. configured seed URLs;
2. sitemap URLs declared by `robots.txt`;
3. the conventional `/sitemap.xml` location;
4. same-domain links found while fetching accepted pages.

RSS and external-index modes are present in the wire contract but are not yet discovery adapters. They are intended for feed ingestion and opt-in Common Crawl/search-provider candidate discovery. Even when an external index supplies candidate URLs, Embedded Alerts must still enforce the source allowlist and fetch/canonicalize content itself before matching.

## Page identity and revisions

A page is identified by tenant, source, and canonical URL. Canonicalization removes fragments, default ports, common tracking parameters, and a trailing non-root slash; remaining query parameters are sorted.

Visible normalized content is SHA-256 hashed. Re-ingesting the same hash under the same model is a no-op. Changed content creates an immutable revision linked to the previous revision. Re-ingesting unchanged content under a different model fails with `model_migration_required`; DEN-3462 owns audited re-embedding and replay.

## Multi-view embeddings

A single flattened page vector loses useful signals. The extractor creates bounded embedding inputs for:

- document title;
- `h1`–`h3` headings;
- a lead summary;
- complete sentence-sized passages;
- frequent non-stopword keywords;
- probable proper-noun/entity phrases;
- URL path/domain terms.

Queries receive the same treatment: the complete user sentence remains the strongest representation, with companion keyword and proper-noun views. Every stored vector carries provider, model, model version, dimensions, normalization, generated time, and extractor version.

When `EMBEDDING_ENDPOINT` is configured, the API calls an HTTPS, OpenAI-compatible embeddings endpoint. Without it, development and tests use a deterministic feature-hash vector identified as `embedded-alerts:feature-hash:development-v1`. That fallback is useful for contracts and dedupe tests; it is not represented as a deep semantic model, and production startup remains blocked.

## Explainable matching

`POST /v1/embeddings/search` embeds the query with the active model and compares only pages with identical model provenance. The returned score combines:

- semantic segment similarity: 74%;
- lexical token overlap: 12%;
- proper-noun/entity overlap: 9%;
- recency: 3%;
- source priority: 2%.

Results include the component values and top page/query segment evidence. A score is ranking evidence, not a statement of truth.

When the request carries an immutable alert-rule ID and revision, qualifying results become deterministic `candidate` matches. The search path never sends a webhook, email, Slack message, or push notification. DEN-3460 owns suppression, cooldown, grouping, retries, provider receipts, dead letters, and logical exactly-once delivery.

## API flow

```text
POST /v1/sources
        │
        ├── POST /v1/sources/{id}/scan
        │       ├── robots + sitemap + bounded link discovery
        │       ├── public-network and domain policy
        │       ├── HTML extraction
        │       ├── content revision dedupe
        │       └── model-versioned vectors
        │
        ├── POST /v1/sources/{id}/ingest
        │       └── trusted connector supplies already-fetched HTML
        │
        └── POST /v1/embeddings/search
                ├── complete query + keyword + entity views
                ├── hybrid ranking and evidence
                └── optional candidate match, never direct delivery
```

`GET /v1/pages` returns latest revision summaries without raw vectors. `GET /v1/matches` returns candidate matches waiting for the delivery state machine. The bundled OpenAPI 3.1 document is served at `/openapi.json`.

## Current safety boundary

The current branch deliberately remains development/test-only:

- source, page, vector, and match state is process-local;
- `x-eal-tenant-id` is a development tenant selector, not authentication;
- alert-rule storage and WebSocket events remain process-local from the parent DEN-3459 branch;
- CORS is still permissive;
- the SQL migration is a durable contract but is not yet wired into SeaORM repositories.

Production startup continues to fail closed. DEN-3459 must supply Shared Auth claims, tenant authorization, explicit origins, durable repositories, restart tests, and WebSocket isolation. The migration in `migrations/20260810223000_semantic_page_index.sql` then becomes the persistence target for this semantic service.
