-- DEN-3461 durable contract for allowlisted public-page ingestion and model-versioned search.
-- The current Rust handlers remain process-local until this migration is wired through
-- the tenant repository in DEN-3459. Do not remove the production startup guard merely
-- because these tables exist.

BEGIN;

CREATE EXTENSION IF NOT EXISTS pgcrypto;
CREATE EXTENSION IF NOT EXISTS vector;

CREATE TABLE IF NOT EXISTS eal_source_domains (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 120),
    base_url text NOT NULL,
    host text NOT NULL,
    include_subdomains boolean NOT NULL DEFAULT false,
    discovery_modes text[] NOT NULL DEFAULT ARRAY['seed', 'robots_sitemap', 'sitemap', 'link_crawl']::text[],
    max_pages_per_scan integer NOT NULL DEFAULT 25 CHECK (max_pages_per_scan BETWEEN 1 AND 100),
    source_priority real NOT NULL DEFAULT 0.5 CHECK (source_priority BETWEEN 0 AND 1),
    respect_robots boolean NOT NULL DEFAULT true,
    enabled boolean NOT NULL DEFAULT true,
    UNIQUE (tenant_id, host, include_subdomains),
    CHECK (base_url ~ '^https?://'),
    CHECK (host = lower(host))
);

CREATE TABLE IF NOT EXISTS eal_source_seeds (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL,
    source_domain_id uuid NOT NULL REFERENCES eal_source_domains(id) ON DELETE CASCADE,
    canonical_url text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, source_domain_id, canonical_url)
);

CREATE TABLE IF NOT EXISTS eal_source_items (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL,
    source_domain_id uuid NOT NULL REFERENCES eal_source_domains(id) ON DELETE CASCADE,
    canonical_url text NOT NULL,
    first_seen_at timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    latest_revision_id uuid,
    UNIQUE (tenant_id, source_domain_id, canonical_url)
);

CREATE TABLE IF NOT EXISTS eal_source_item_revisions (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL,
    source_item_id uuid NOT NULL REFERENCES eal_source_items(id) ON DELETE CASCADE,
    previous_revision_id uuid REFERENCES eal_source_item_revisions(id) ON DELETE RESTRICT,
    requested_url text NOT NULL,
    fetched_url text NOT NULL,
    fetched_at timestamptz NOT NULL,
    content_type text NOT NULL,
    etag text,
    last_modified text,
    content_hash bytea NOT NULL CHECK (octet_length(content_hash) = 32),
    title text,
    summary text NOT NULL,
    keywords text[] NOT NULL DEFAULT '{}'::text[],
    entities text[] NOT NULL DEFAULT '{}'::text[],
    extractor_version text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, source_item_id, content_hash),
    UNIQUE (tenant_id, id, source_item_id)
);

ALTER TABLE eal_source_items
    DROP CONSTRAINT IF EXISTS eal_source_items_latest_revision_fk;
ALTER TABLE eal_source_items
    ADD CONSTRAINT eal_source_items_latest_revision_fk
    FOREIGN KEY (latest_revision_id)
    REFERENCES eal_source_item_revisions(id)
    ON DELETE RESTRICT
    DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE IF NOT EXISTS eal_embedding_sets (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL,
    source_item_revision_id uuid NOT NULL REFERENCES eal_source_item_revisions(id) ON DELETE CASCADE,
    provider text NOT NULL,
    model text NOT NULL,
    model_version text NOT NULL,
    dimensions integer NOT NULL CHECK (dimensions BETWEEN 8 AND 16384),
    normalization text NOT NULL CHECK (normalization IN ('l2')),
    extractor_version text NOT NULL,
    generated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (
        tenant_id,
        source_item_revision_id,
        provider,
        model,
        model_version,
        dimensions,
        normalization,
        extractor_version
    )
);

CREATE TABLE IF NOT EXISTS eal_embedding_segments (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL,
    embedding_set_id uuid NOT NULL REFERENCES eal_embedding_sets(id) ON DELETE CASCADE,
    ordinal integer NOT NULL CHECK (ordinal >= 0),
    segment_kind text NOT NULL CHECK (
        segment_kind IN ('title', 'heading', 'summary', 'sentence', 'entity', 'keyword', 'url_signal', 'query')
    ),
    segment_text text NOT NULL,
    segment_weight real NOT NULL CHECK (segment_weight > 0 AND segment_weight <= 2),
    embedding vector NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, embedding_set_id, ordinal),
    CHECK (vector_dims(embedding) BETWEEN 8 AND 16384)
);

CREATE TABLE IF NOT EXISTS eal_match_candidates (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL,
    match_key bytea NOT NULL CHECK (octet_length(match_key) = 32),
    alert_rule_id uuid NOT NULL,
    alert_rule_revision integer NOT NULL CHECK (alert_rule_revision > 0),
    source_item_revision_id uuid NOT NULL REFERENCES eal_source_item_revisions(id) ON DELETE RESTRICT,
    embedding_set_id uuid NOT NULL REFERENCES eal_embedding_sets(id) ON DELETE RESTRICT,
    query_hash bytea NOT NULL CHECK (octet_length(query_hash) = 32),
    score real NOT NULL CHECK (score BETWEEN 0 AND 1),
    score_components jsonb NOT NULL,
    evidence jsonb NOT NULL,
    state text NOT NULL DEFAULT 'candidate' CHECK (state IN ('candidate', 'suppressed', 'queued', 'dismissed')),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, match_key)
);

CREATE INDEX IF NOT EXISTS eal_source_items_latest_scan_idx
    ON eal_source_items (tenant_id, source_domain_id, last_seen_at DESC, id);
CREATE INDEX IF NOT EXISTS eal_source_item_revisions_history_idx
    ON eal_source_item_revisions (tenant_id, source_item_id, fetched_at DESC, id);
CREATE INDEX IF NOT EXISTS eal_embedding_sets_model_idx
    ON eal_embedding_sets (
        tenant_id,
        provider,
        model,
        model_version,
        dimensions,
        normalization,
        source_item_revision_id
    );
CREATE INDEX IF NOT EXISTS eal_embedding_segments_set_idx
    ON eal_embedding_segments (tenant_id, embedding_set_id, segment_kind, ordinal);
CREATE INDEX IF NOT EXISTS eal_match_candidates_rule_idx
    ON eal_match_candidates (tenant_id, alert_rule_id, alert_rule_revision, created_at DESC, id);
CREATE INDEX IF NOT EXISTS eal_match_candidates_state_idx
    ON eal_match_candidates (tenant_id, state, created_at, id);

-- Model-specific ANN indexes must be created as partial indexes once a production
-- model and dimensions are approved. A generic vector column intentionally prevents
-- silently comparing vectors from different model revisions.

ALTER TABLE eal_source_domains ENABLE ROW LEVEL SECURITY;
ALTER TABLE eal_source_seeds ENABLE ROW LEVEL SECURITY;
ALTER TABLE eal_source_items ENABLE ROW LEVEL SECURITY;
ALTER TABLE eal_source_item_revisions ENABLE ROW LEVEL SECURITY;
ALTER TABLE eal_embedding_sets ENABLE ROW LEVEL SECURITY;
ALTER TABLE eal_embedding_segments ENABLE ROW LEVEL SECURITY;
ALTER TABLE eal_match_candidates ENABLE ROW LEVEL SECURITY;

DO $policies$
DECLARE
    table_name text;
BEGIN
    FOREACH table_name IN ARRAY ARRAY[
        'eal_source_domains',
        'eal_source_seeds',
        'eal_source_items',
        'eal_source_item_revisions',
        'eal_embedding_sets',
        'eal_embedding_segments',
        'eal_match_candidates'
    ]
    LOOP
        EXECUTE format('DROP POLICY IF EXISTS tenant_isolation ON %I', table_name);
        EXECUTE format(
            'CREATE POLICY tenant_isolation ON %I USING (tenant_id = nullif(current_setting(''request.jwt.claim.tenant_id'', true), '''')::uuid) WITH CHECK (tenant_id = nullif(current_setting(''request.jwt.claim.tenant_id'', true), '''')::uuid)',
            table_name
        );
    END LOOP;
END
$policies$;

COMMENT ON TABLE eal_source_domains IS
    'Tenant-owned allowlist. The crawler must never fetch a URL outside these host boundaries.';
COMMENT ON TABLE eal_source_item_revisions IS
    'Immutable content revisions. Unchanged normalized content is a no-op.';
COMMENT ON TABLE eal_embedding_sets IS
    'Immutable embedding provenance. Cross-model comparison is forbidden unless an audited migration explicitly maps models.';
COMMENT ON TABLE eal_embedding_segments IS
    'Separate title, heading, summary, sentence, entity, keyword, and URL views; never one flattened page vector only.';
COMMENT ON TABLE eal_match_candidates IS
    'Search output only. Delivery, cooldown, retries, receipts, and dead-letter behavior belong to DEN-3460.';

COMMIT;
