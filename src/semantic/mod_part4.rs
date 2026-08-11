impl SemanticService {
    pub(crate) async fn list_matches(&self, tenant_id: Uuid) -> Vec<MatchCandidate> {
        let store = self.store.read().await;
        let mut matches: Vec<MatchCandidate> = store
            .matches
            .values()
            .filter(|candidate| candidate.tenant_id == tenant_id)
            .cloned()
            .collect();
        matches.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        matches
    }

    async fn source_for_tenant(
        &self,
        tenant_id: Uuid,
        source_id: Uuid,
    ) -> Result<SourceDomain, SemanticError> {
        let store = self.store.read().await;
        let source = store.sources.get(&source_id).ok_or_else(|| {
            SemanticError::not_found("source", "source domain was not found")
        })?;
        if source.tenant_id != tenant_id {
            return Err(SemanticError::not_found(
                "source",
                "source domain was not found",
            ));
        }
        Ok(source.clone())
    }

    async fn ingest_fetched_page(
        &self,
        source: &SourceDomain,
        fetched: FetchedPage,
    ) -> Result<IngestOutcome, SemanticError> {
        let extracted = extract_page(&fetched.body, &fetched.final_url)?;
        self.ingest_extracted_page(source, fetched, extracted).await
    }

    async fn ingest_extracted_page(
        &self,
        source: &SourceDomain,
        fetched: FetchedPage,
        extracted: extract::ExtractedPage,
    ) -> Result<IngestOutcome, SemanticError> {
        let canonical_url = source.canonicalize_url(&fetched.final_url)?.to_string();
        let content_hash = sha256_hex(extracted.visible_text.as_bytes());
        let key = (source.tenant_id, source.id, canonical_url.clone());

        if let Some(existing) = self.latest_page(&key).await {
            if existing.content_hash == content_hash {
                if existing.model != *self.embedder.model() {
                    return Err(SemanticError::conflict(
                        "model_migration_required",
                        "content is unchanged but the active embedding model changed; use the audited model migration workflow from DEN-3462",
                    ));
                }
                return Ok(IngestOutcome {
                    disposition: IngestDisposition::Unchanged,
                    page_revision_id: existing.id,
                    previous_revision_id: existing.previous_revision_id,
                    canonical_url,
                    content_hash,
                    segment_count: existing.segments.len(),
                    model: existing.model,
                });
            }
        }

        let segments = self.embedder.embed_segments(&extracted.segments).await?;
        let mut store = self.store.write().await;
        let previous_revision_id = store.latest_pages.get(&key).copied();
        if let Some(previous_id) = previous_revision_id {
            if let Some(previous) = store.pages.get(&previous_id) {
                if previous.content_hash == content_hash {
                    if previous.model != *self.embedder.model() {
                        return Err(SemanticError::conflict(
                            "model_migration_required",
                            "content is unchanged but the active embedding model changed; use the audited model migration workflow from DEN-3462",
                        ));
                    }
                    return Ok(IngestOutcome {
                        disposition: IngestDisposition::Unchanged,
                        page_revision_id: previous.id,
                        previous_revision_id: previous.previous_revision_id,
                        canonical_url,
                        content_hash,
                        segment_count: previous.segments.len(),
                        model: previous.model.clone(),
                    });
                }
            }
        }

        let revision = PageRevision {
            id: Uuid::new_v4(),
            tenant_id: source.tenant_id,
            source_id: source.id,
            previous_revision_id,
            canonical_url: canonical_url.clone(),
            requested_url: fetched.requested_url.to_string(),
            fetched_at: Utc::now(),
            content_type: fetched.content_type,
            etag: fetched.etag,
            last_modified: fetched.last_modified,
            content_hash: content_hash.clone(),
            title: extracted.title,
            summary: extracted.summary,
            keywords: extracted.keywords,
            entities: extracted.entities,
            model: self.embedder.model().clone(),
            extractor_version: EXTRACTOR_VERSION.into(),
            segments,
        };
        let disposition = if previous_revision_id.is_some() {
            IngestDisposition::Updated
        } else {
            IngestDisposition::Created
        };
        let outcome = IngestOutcome {
            disposition,
            page_revision_id: revision.id,
            previous_revision_id,
            canonical_url,
            content_hash,
            segment_count: revision.segments.len(),
            model: revision.model.clone(),
        };
        store.latest_pages.insert(key, revision.id);
        store.pages.insert(revision.id, revision);
        Ok(outcome)
    }

    async fn latest_page(&self, key: &(Uuid, Uuid, String)) -> Option<PageRevision> {
        let store = self.store.read().await;
        store
            .latest_pages
            .get(key)
            .and_then(|page_id| store.pages.get(page_id))
            .cloned()
    }

    async fn persist_match_candidates(
        &self,
        tenant_id: Uuid,
        rule: &AlertRuleRevisionRef,
        query_text: &str,
        results: &[SearchResult],
    ) -> usize {
        let query_hash = sha256_hex(query_text.as_bytes());
        let mut store = self.store.write().await;
        let mut created = 0;
        for result in results {
            let match_key = sha256_hex(
                format!(
                    "{tenant_id}|{}|{}|{}|{}|{}",
                    rule.id,
                    rule.revision,
                    result.page_revision_id,
                    result.model.fingerprint(),
                    result.content_hash
                )
                .as_bytes(),
            );
            if store.matches.contains_key(&match_key) {
                continue;
            }
            store.matches.insert(
                match_key.clone(),
                MatchCandidate {
                    id: Uuid::new_v4(),
                    match_key,
                    tenant_id,
                    alert_rule_id: rule.id,
                    alert_rule_revision: rule.revision,
                    page_revision_id: result.page_revision_id,
                    source_id: result.source_id,
                    canonical_url: result.canonical_url.clone(),
                    content_hash: result.content_hash.clone(),
                    query_hash: query_hash.clone(),
                    model: result.model.clone(),
                    score: result.score,
                    components: result.components.clone(),
                    evidence: result.evidence.clone(),
                    state: "candidate",
                    created_at: Utc::now(),
                },
            );
            created += 1;
        }
        created
    }
}

