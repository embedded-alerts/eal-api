# Alert-rule source scoping

Alert rules may restrict semantic matching to durable Embedded Alerts source
policies. The restriction is part of the immutable alert-rule revision and is
applied before pgvector ranking, not after a broad tenant search.

## Wire form

`source_filters` remains a string array for compatibility, but each value has a
single typed meaning:

```json
{
  "source_filters": [
    "source:11111111-1111-1111-1111-111111111111"
  ]
}
```

A bare UUID is accepted on create and normalized to `source:<uuid>`. Values are
trimmed, canonicalized, sorted, and deduplicated. At most 100 source IDs are
accepted, matching the bounded `EmbeddingSearchRequest.source_ids` contract.
Arbitrary labels, URLs, host names, malformed UUIDs, and additional selector
kinds fail closed.

An empty rule scope means the rule does not add a source restriction. A nonempty
rule scope is an upper bound:

- omitted request `source_ids` inherit the complete rule scope;
- a request may select a subset of the rule scope;
- a request containing any source outside the rule scope is rejected;
- source IDs are still evaluated only inside the authenticated tenant by the
  existing search SQL.

This prevents a client from broadening an alert rule during match evaluation.
The filtered IDs enter `search_embeddings` before ranking and result limiting,
so out-of-scope pages cannot consume candidate slots or create match records.

## Executable assurance

`formal/source-scope/model.py` independently exhausts all 64 combinations of
rule and request scopes over a three-source universe. It proves the abstract
upper-bound policy. `tests/source_filter_scope_contract.rs` exhausts the same
state space through the production Rust implementation, providing a bounded
refinement check in normal CI.

The model covers finite set scoping only. PostgreSQL row-level security,
authentication, source ownership, pgvector correctness, transaction isolation,
and deployment configuration remain separate tested obligations.
