impl SemanticService {
    pub(crate) fn from_env() -> Result<Self, SemanticError> {
        Ok(Self {
            store: Arc::new(RwLock::new(SemanticStore::default())),
            crawler: Crawler::new(),
            embedder: Embedder::from_env()?,
        })
    }

    #[cfg(test)]
    fn for_test() -> Self {
        Self {
            store: Arc::new(RwLock::new(SemanticStore::default())),
            crawler: Crawler::new(),
            embedder: Embedder::development_hash(),
        }
    }

    pub(crate) fn embedding_model(&self) -> &EmbeddingModelRef {
        self.embedder.model()
    }

    pub(crate) fn embedding_mode(&self) -> &'static str {
        self.embedder.mode()
    }

    pub(crate) async fn register_source(
        &self,
        tenant_id: Uuid,
        input: CreateSourceDomain,
    ) -> Result<SourceDomain, SemanticError> {
        let source = SourceDomain::create(tenant_id, input)?;
        let mut store = self.store.write().await;
        if store.sources.values().any(|existing| {
            existing.tenant_id == tenant_id
                && existing.host == source.host
                && existing.include_subdomains == source.include_subdomains
        }) {
            return Err(SemanticError::conflict(
                "source_domain",
                "an equivalent source domain is already registered for this tenant",
            ));
        }
        store.sources.insert(source.id, source.clone());
        Ok(source)
    }

    pub(crate) async fn list_sources(&self, tenant_id: Uuid) -> Vec<SourceDomain> {
        let store = self.store.read().await;
        let mut sources: Vec<SourceDomain> = store
            .sources
            .values()
            .filter(|source| source.tenant_id == tenant_id)
            .cloned()
            .collect();
        sources.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
        sources
    }

    pub(crate) async fn list_pages(&self, tenant_id: Uuid) -> Vec<PageIndexRecord> {
        let store = self.store.read().await;
        let mut pages: Vec<PageIndexRecord> = store
            .latest_pages
            .iter()
            .filter(|((stored_tenant, _, _), _)| *stored_tenant == tenant_id)
            .filter_map(|(_, page_id)| store.pages.get(page_id))
            .map(PageIndexRecord::from)
            .collect();
        pages.sort_by(|left, right| {
            right
                .fetched_at
                .cmp(&left.fetched_at)
                .then_with(|| left.canonical_url.cmp(&right.canonical_url))
        });
        pages
    }

    pub(crate) async fn ingest_supplied_page(
        &self,
        tenant_id: Uuid,
        source_id: Uuid,
        request: IngestPageRequest,
    ) -> Result<IngestOutcome, SemanticError> {
        let source = self.source_for_tenant(tenant_id, source_id).await?;
        let requested_url = Url::parse(request.url.trim()).map_err(|error| {
            SemanticError::invalid("url", format!("invalid page URL: {error}"))
        })?;
        let final_url = source.canonicalize_url(&requested_url)?;
        let fetched = FetchedPage {
            requested_url,
            final_url,
            content_type: request
                .content_type
                .unwrap_or_else(|| "text/html; charset=utf-8".into()),
            body: request.html,
            etag: request.etag,
            last_modified: request.last_modified,
        };
        self.ingest_fetched_page(&source, fetched).await
    }

}
