mod crawl;
mod domain;
mod embedding;
mod extract;
mod search;

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    sync::Arc,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use url::Url;
use uuid::Uuid;

use crawl::{Crawler, FetchedPage};
use embedding::{sha256_hex, Embedder};
use extract::{extract_page, query_segments, EXTRACTOR_VERSION};
use search::{score_page, validate_search_request};

pub(crate) use domain::{CreateSourceDomain, DiscoveryMode, SourceDomain};
pub(crate) use embedding::{EmbeddedSegment, EmbeddingModelRef};
pub(crate) use search::{
    AlertRuleRevisionRef, MatchEvidence, ScoreComponents, SearchResult, SemanticSearchRequest,
    SemanticSearchResponse,
};

#[derive(Debug)]
pub(crate) enum SemanticError {
    Invalid { code: &'static str, message: String },
    Forbidden { code: &'static str, message: String },
    NotFound { code: &'static str, message: String },
    Conflict { code: &'static str, message: String },
    Fetch(String),
    Provider(String),
    Configuration(String),
    Internal(String),
}

impl SemanticError {
    pub(crate) fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        Self::Invalid {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn forbidden(code: &'static str, message: impl Into<String>) -> Self {
        Self::Forbidden {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::NotFound {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::Conflict {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn fetch(message: impl Into<String>) -> Self {
        Self::Fetch(message.into())
    }

    pub(crate) fn provider(message: impl Into<String>) -> Self {
        Self::Provider(message.into())
    }

    pub(crate) fn configuration(message: impl Into<String>) -> Self {
        Self::Configuration(message.into())
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Invalid { code, .. }
            | Self::Forbidden { code, .. }
            | Self::NotFound { code, .. }
            | Self::Conflict { code, .. } => code,
            Self::Fetch(_) => "source_fetch",
            Self::Provider(_) => "embedding_provider",
            Self::Configuration(_) => "semantic_configuration",
            Self::Internal(_) => "semantic_internal",
        }
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Invalid { message, .. }
            | Self::Forbidden { message, .. }
            | Self::NotFound { message, .. }
            | Self::Conflict { message, .. }
            | Self::Fetch(message)
            | Self::Provider(message)
            | Self::Configuration(message)
            | Self::Internal(message) => message,
        }
    }

    pub(crate) fn status_code(&self) -> axum::http::StatusCode {
        use axum::http::StatusCode;
        match self {
            Self::Invalid { .. } => StatusCode::BAD_REQUEST,
            Self::Forbidden { .. } => StatusCode::FORBIDDEN,
            Self::NotFound { .. } => StatusCode::NOT_FOUND,
            Self::Conflict { .. } => StatusCode::CONFLICT,
            Self::Fetch(_) => StatusCode::BAD_GATEWAY,
            Self::Provider(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Configuration(_) | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl fmt::Display for SemanticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.message())
    }
}

impl std::error::Error for SemanticError {}

#[derive(Clone)]
pub(crate) struct SemanticService {
    store: Arc<RwLock<SemanticStore>>,
    crawler: Crawler,
    embedder: Embedder,
}

#[derive(Default)]
struct SemanticStore {
    sources: HashMap<Uuid, SourceDomain>,
    pages: HashMap<Uuid, PageRevision>,
    latest_pages: HashMap<(Uuid, Uuid, String), Uuid>,
    matches: HashMap<String, MatchCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PageRevision {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub source_id: Uuid,
    pub previous_revision_id: Option<Uuid>,
    pub canonical_url: String,
    pub requested_url: String,
    pub fetched_at: DateTime<Utc>,
    pub content_type: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_hash: String,
    pub title: Option<String>,
    pub summary: String,
    pub keywords: Vec<String>,
    pub entities: Vec<String>,
    pub model: EmbeddingModelRef,
    pub extractor_version: String,
    pub segments: Vec<EmbeddedSegment>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PageIndexRecord {
    pub id: Uuid,
    pub source_id: Uuid,
    pub previous_revision_id: Option<Uuid>,
    pub canonical_url: String,
    pub fetched_at: DateTime<Utc>,
    pub content_hash: String,
    pub title: Option<String>,
    pub summary: String,
    pub keywords: Vec<String>,
    pub entities: Vec<String>,
    pub model: EmbeddingModelRef,
    pub extractor_version: String,
    pub segment_count: usize,
}

impl From<&PageRevision> for PageIndexRecord {
    fn from(page: &PageRevision) -> Self {
        Self {
            id: page.id,
            source_id: page.source_id,
            previous_revision_id: page.previous_revision_id,
            canonical_url: page.canonical_url.clone(),
            fetched_at: page.fetched_at,
            content_hash: page.content_hash.clone(),
            title: page.title.clone(),
            summary: page.summary.clone(),
            keywords: page.keywords.clone(),
            entities: page.entities.clone(),
            model: page.model.clone(),
            extractor_version: page.extractor_version.clone(),
            segment_count: page.segments.len(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct IngestPageRequest {
    pub url: String,
    pub html: String,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IngestDisposition {
    Created,
    Updated,
    Unchanged,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct IngestOutcome {
    pub disposition: IngestDisposition,
    pub page_revision_id: Uuid,
    pub previous_revision_id: Option<Uuid>,
    pub canonical_url: String,
    pub content_hash: String,
    pub segment_count: usize,
    pub model: EmbeddingModelRef,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ScanReport {
    pub source_id: Uuid,
    pub discovered_urls: usize,
    pub sitemap_count: usize,
    pub attempted: usize,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub rejected_by_robots: usize,
    pub failed: usize,
    pub failures: Vec<ScanFailure>,
    pub embedding_model: EmbeddingModelRef,
    pub extractor_version: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ScanFailure {
    pub url: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MatchCandidate {
    pub id: Uuid,
    pub match_key: String,
    pub tenant_id: Uuid,
    pub alert_rule_id: Uuid,
    pub alert_rule_revision: u32,
    pub page_revision_id: Uuid,
    pub source_id: Uuid,
    pub canonical_url: String,
    pub content_hash: String,
    pub query_hash: String,
    pub model: EmbeddingModelRef,
    pub score: f32,
    pub components: ScoreComponents,
    pub evidence: Vec<MatchEvidence>,
    pub state: &'static str,
    pub created_at: DateTime<Utc>,
}

