-- Domain-scoped indexing and model-versioned semantic matching.
-- The original alert_documents table remains untouched for migration compatibility;
-- new code must use the tenant-owned eal_* tables below.

create extension if not exists pgcrypto;
create extension if not exists vector;

create table if not exists eal_sources (
    id uuid primary key default gen_random_uuid(),
    tenant_id uuid not null,
    name text not null check (length(name) between 1 and 256),
    root_url text not null,
    allowed_hosts jsonb not null check (
        jsonb_typeof(allowed_hosts) = 'array'
        and jsonb_array_length(allowed_hosts) between 1 and 64
    ),
    allowed_path_prefixes jsonb not null default '["/"]'::jsonb check (
        jsonb_typeof(allowed_path_prefixes) = 'array'
        and jsonb_array_length(allowed_path_prefixes) between 1 and 128
    ),
    include_subdomains boolean not null default false,
    discovery_modes jsonb not null default '["sitemap", "rss"]'::jsonb check (
        jsonb_typeof(discovery_modes) = 'array'
        and jsonb_array_length(discovery_modes) >= 1
    ),
    crawl_interval_seconds integer not null default 900 check (
        crawl_interval_seconds between 60 and 604800
    ),
    max_depth smallint not null default 3 check (max_depth between 0 and 16),
    max_pages_per_run integer not null default 1000 check (
        max_pages_per_run between 1 and 10000
    ),
    obey_robots boolean not null default true check (obey_robots),
    enabled boolean not null default true,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (tenant_id, root_url)
);

create index if not exists eal_sources_tenant_enabled_idx
    on eal_sources (tenant_id, enabled, updated_at desc, id);

create table if not exists eal_crawl_queue (
    id uuid primary key default gen_random_uuid(),
    tenant_id uuid not null,
    source_id uuid not null references eal_sources(id) on delete cascade,
    url text not null,
    canonical_url text not null,
    discovered_by text not null check (
        discovered_by in ('manual', 'sitemap', 'rss', 'atom', 'external_index_candidates', 'link')
    ),
    depth smallint not null default 0 check (depth between 0 and 16),
    status text not null default 'pending' check (
        status in ('pending', 'leased', 'fetched', 'unchanged', 'blocked', 'failed', 'dead_letter')
    ),
    priority integer not null default 100,
    next_attempt_at timestamptz not null default now(),
    lease_owner text,
    lease_expires_at timestamptz,
    attempt_count integer not null default 0 check (attempt_count >= 0),
    last_error text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (tenant_id, source_id, canonical_url)
);

create index if not exists eal_crawl_queue_claim_idx
    on eal_crawl_queue (status, next_attempt_at, priority, created_at)
    where status in ('pending', 'failed');

create table if not exists eal_pages (
    id uuid primary key default gen_random_uuid(),
    tenant_id uuid not null,
    source_id uuid not null references eal_sources(id) on delete cascade,
    canonical_url text not null,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    latest_revision_id uuid,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (tenant_id, source_id, canonical_url)
);

create index if not exists eal_pages_tenant_source_seen_idx
    on eal_pages (tenant_id, source_id, last_seen_at desc, id);

create table if not exists eal_page_revisions (
    id uuid primary key default gen_random_uuid(),
    tenant_id uuid not null,
    page_id uuid not null references eal_pages(id) on delete cascade,
    predecessor_revision_id uuid references eal_page_revisions(id) on delete set null,
    original_url text not null,
    final_url text not null,
    title text,
    content_text text not null,
    content_sha256 text not null check (length(content_sha256) = 64),
    content_type text not null,
    http_status smallint not null check (http_status between 200 and 399),
    published_at timestamptz,
    fetched_at timestamptz not null,
    created_at timestamptz not null default now(),
    unique (tenant_id, page_id, content_sha256)
);

alter table eal_pages
    drop constraint if exists eal_pages_latest_revision_fk;
alter table eal_pages
    add constraint eal_pages_latest_revision_fk
    foreign key (latest_revision_id) references eal_page_revisions(id) on delete set null;

create index if not exists eal_page_revisions_tenant_fetched_idx
    on eal_page_revisions (tenant_id, fetched_at desc, id);

create table if not exists eal_embeddings (
    id uuid primary key default gen_random_uuid(),
    tenant_id uuid not null,
    revision_id uuid not null references eal_page_revisions(id) on delete cascade,
    model text not null,
    model_version text not null,
    dimensions integer not null check (dimensions between 1 and 65535),
    normalization text not null check (normalization in ('none', 'l2', 'unit_length')),
    embedding vector not null,
    generated_at timestamptz not null default now(),
    created_at timestamptz not null default now(),
    unique (tenant_id, revision_id, model, model_version, dimensions, normalization)
);

-- A vector column without a fixed dimension allows multiple explicitly-versioned models.
-- Add partial HNSW indexes per production model/dimension after traffic establishes the
-- supported set; search queries always filter model, version, dimensions, and normalization.
create index if not exists eal_embeddings_provenance_idx
    on eal_embeddings (tenant_id, model, model_version, dimensions, normalization, created_at desc, id);

create table if not exists eal_match_candidates (
    id uuid primary key default gen_random_uuid(),
    tenant_id uuid not null,
    alert_rule_id uuid not null,
    revision_id uuid not null references eal_page_revisions(id) on delete cascade,
    embedding_id uuid not null references eal_embeddings(id) on delete cascade,
    canonical_match_key text not null,
    similarity double precision not null check (similarity between -1 and 1),
    threshold double precision not null check (threshold between 0 and 1),
    status text not null default 'pending' check (
        status in ('pending', 'suppressed', 'approved', 'rejected', 'delivered', 'failed')
    ),
    score_explanation jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (tenant_id, canonical_match_key)
);

create index if not exists eal_match_candidates_rule_status_idx
    on eal_match_candidates (tenant_id, alert_rule_id, status, created_at desc, id);

-- Deny-by-default RLS. The service role may bypass RLS, but every application query must
-- still include tenant_id. Shared Auth/Supabase claim policies are added only after the
-- registered-client and tenant-claim contract is certified.
alter table eal_sources enable row level security;
alter table eal_crawl_queue enable row level security;
alter table eal_pages enable row level security;
alter table eal_page_revisions enable row level security;
alter table eal_embeddings enable row level security;
alter table eal_match_candidates enable row level security;
