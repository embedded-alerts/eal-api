# Natural-language semantic evaluation

Embedded Alerts accepts user-authored interest statements such as:

> Notify me when a Colombian renewable-energy company launches tooling for engineering teams.

Clients do not generate, transmit, or compare raw vectors. The API owns query analysis, provider calls, model compatibility, ranking, candidate persistence, and evidence retention.

## Query representation

The complete normalized query remains the first and strongest embedding input. Companion views add:

- stopword-filtered keywords;
- probable proper nouns and entity phrases.

This prevents the system from reducing a nuanced alert to literal keywords while retaining useful lexical and entity evidence. All inputs carry a kind, deterministic ordinal, bounded text, and explicit weight through `eal-semantic-contracts`.

## Page representation

The bounded ingestion worker emits normalized document text plus independent views for:

- title;
- `h1`–`h3` headings;
- lead summary;
- complete sentence passages;
- keywords;
- proper-noun/entity phrases;
- URL/domain/path signals;
- a bounded document fallback.

Current aggregate-vector search may consume a labeled weighted fallback. The semantic-input tables retain every view and its provenance so native multi-vector ranking can be introduced without recrawling pages.

## Intended API flow

```text
immutable alert-rule revision
          │
          ├── analyze complete query + companion views
          ├── embed with the rule's pinned embedding space
          ├── search only model-compatible page revisions
          ├── persist deterministic match candidates + evidence
          └── hand candidates to DEN-3460 delivery state machine
```

The public route will be an authenticated operation on an immutable alert-rule revision. It must resolve the query text server-side and reject caller-supplied vectors. A provider outage, missing production endpoint, model mismatch, dimensional mismatch, non-finite value, zero vector, or tenant mismatch fails closed and creates no candidate.

## Provider configuration

`EAL_EMBEDDING_ENDPOINT` identifies an OpenAI-compatible embeddings endpoint. Remote endpoints must use HTTPS. Development may use HTTP only on loopback. `EAL_EMBEDDING_API_KEY` is optional for providers that do not require bearer authentication.

The operator-only `semantic_query_probe` binary certifies a configured embedding space and query decomposition without printing raw vector values:

```bash
EAL_QUERY_TEXT='Notify me when Acme launches renewable energy tools' \
EAL_EMBEDDING_SPACE_JSON='{"provider":"...","model":"...","model_version":"...","dimensions":1536,"normalization":"l2"}' \
EAL_EMBEDDING_ENDPOINT='https://embedding.example/v1/embeddings' \
cargo run --bin semantic_query_probe
```

Use the exact JSON shape defined by the active `EmbeddingSpaceConfig` contract. The probe reports query views, input sizes and weights, vector dimensions, and L2 norm; it never returns vector values.

## Delivery boundary

Semantic evaluation produces candidate records only. It never sends email, webhooks, Slack, Discord, browser push, or mobile push directly. DEN-3460 owns suppression, cooldown, grouping, retries, receipts, dead letters, and logical exactly-once delivery.

## Production gates

Production remains blocked until:

1. Shared Auth claims replace development tenant selectors and all repositories use tenant-scoped transactions.
2. Alert-rule revisions and query views are durable rather than process-local.
3. The authenticated natural-language evaluation route is wired to `QueryEmbeddingService` and the durable match store.
4. PostgreSQL migrations are applied and restart/isolation tests pass.
5. Explicit CORS origins, rate limits, provider quotas, and WebSocket tenant isolation are certified.
6. DEN-3460 notification canaries pass before any delivery destination is enabled.
