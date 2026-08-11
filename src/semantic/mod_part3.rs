impl SemanticService {
    pub(crate) async fn scan_source(
        &self,
        tenant_id: Uuid,
        source_id: Uuid,
    ) -> Result<ScanReport, SemanticError> {
        let source = self.source_for_tenant(tenant_id, source_id).await?;
        if !source.enabled {
            return Err(SemanticError::conflict(
                "source_disabled",
                "source must be enabled before it can be scanned",
            ));
        }
        let discovery = self.crawler.discover(&source).await?;
        let mut queue: VecDeque<Url> = discovery.urls.into();
        let mut seen = HashSet::new();
        let mut report = ScanReport {
            source_id,
            discovered_urls: queue.len(),
            sitemap_count: discovery.sitemap_count,
            attempted: 0,
            created: 0,
            updated: 0,
            unchanged: 0,
            rejected_by_robots: 0,
            failed: 0,
            failures: Vec::new(),
            embedding_model: self.embedder.model().clone(),
            extractor_version: EXTRACTOR_VERSION,
        };

        while report.attempted < source.max_pages_per_scan {
            let Some(url) = queue.pop_front() else {
                break;
            };
            let canonical = match source.canonicalize_url(&url) {
                Ok(canonical) => canonical,
                Err(error) => {
                    record_failure(&mut report, url.to_string(), error);
                    continue;
                }
            };
            if !seen.insert(canonical.to_string()) {
                continue;
            }
            if source.respect_robots && !discovery.robots.allows(&canonical) {
                report.rejected_by_robots += 1;
                continue;
            }
            report.attempted += 1;

            match self.crawler.fetch_html(&source, canonical).await {
                Ok(fetched) => {
                    let extracted = match extract_page(&fetched.body, &fetched.final_url) {
                        Ok(extracted) => extracted,
                        Err(error) => {
                            record_failure(&mut report, fetched.final_url.to_string(), error);
                            continue;
                        }
                    };
                    if source.has_mode(DiscoveryMode::LinkCrawl) {
                        for link in &extracted.links {
                            if queue.len() + seen.len() >= source.max_pages_per_scan * 8 {
                                break;
                            }
                            if source.allows_url(link) {
                                queue.push_back(link.clone());
                            }
                        }
                        report.discovered_urls = report
                            .discovered_urls
                            .max(queue.len().saturating_add(seen.len()));
                    }
                    match self
                        .ingest_extracted_page(&source, fetched, extracted)
                        .await
                    {
                        Ok(outcome) => match outcome.disposition {
                            IngestDisposition::Created => report.created += 1,
                            IngestDisposition::Updated => report.updated += 1,
                            IngestDisposition::Unchanged => report.unchanged += 1,
                        },
                        Err(error) => {
                            record_failure(&mut report, url.to_string(), error);
                        }
                    }
                }
                Err(error) => record_failure(&mut report, url.to_string(), error),
            }
        }
        Ok(report)
    }

    pub(crate) async fn search(
        &self,
        tenant_id: Uuid,
        request: SemanticSearchRequest,
    ) -> Result<SemanticSearchResponse, SemanticError> {
        validate_search_request(&request)?;
        let query = query_segments(&request.query_text)?;
        if let Some(expected_model) = &request.expected_model {
            if expected_model != self.embedder.model() {
                return Err(SemanticError::conflict(
                    "embedding_model",
                    format!(
                        "requested model {} does not match active model {}",
                        expected_model.fingerprint(),
                        self.embedder.model().fingerprint()
                    ),
                ));
            }
        }
        let query_embeddings = self.embedder.embed_segments(&query.segments).await?;

        let (pages, sources) = {
            let store = self.store.read().await;
            let source_filter: HashSet<Uuid> = request.source_ids.iter().copied().collect();
            let mut pages = Vec::new();
            let mut sources = HashMap::new();
            for ((stored_tenant, source_id, _), page_id) in &store.latest_pages {
                if *stored_tenant != tenant_id
                    || (!source_filter.is_empty() && !source_filter.contains(source_id))
                {
                    continue;
                }
                let Some(page) = store.pages.get(page_id) else {
                    continue;
                };
                let Some(source) = store.sources.get(source_id) else {
                    continue;
                };
                pages.push(page.clone());
                sources.insert(*source_id, source.clone());
            }
            (pages, sources)
        };

        let mut results = Vec::new();
        let mut compared_pages = 0;
        let mut skipped_cross_model_pages = 0;
        for page in pages {
            if page.model != *self.embedder.model() {
                skipped_cross_model_pages += 1;
                continue;
            }
            let Some(source) = sources.get(&page.source_id) else {
                continue;
            };
            compared_pages += 1;
            let result = score_page(&query, &query_embeddings, &page, source, Utc::now())?;
            if result.score >= request.threshold {
                results.push(result);
            }
        }
        results.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.page_revision_id.cmp(&right.page_revision_id))
        });

        let start = match request.cursor.as_deref() {
            Some(cursor) => {
                let cursor = Uuid::parse_str(cursor).map_err(|error| {
                    SemanticError::invalid("cursor", format!("invalid search cursor: {error}"))
                })?;
                results
                    .iter()
                    .position(|result| result.page_revision_id == cursor)
                    .map(|position| position + 1)
                    .ok_or_else(|| {
                        SemanticError::conflict(
                            "cursor",
                            "search cursor is not present in the current stable result set",
                        )
                    })?
            }
            None => 0,
        };
        let candidate_matches_created = if let Some(rule) = &request.alert_rule {
            self.persist_match_candidates(tenant_id, rule, &query.text, &results)
                .await
        } else {
            0
        };

        let end = start.saturating_add(request.limit).min(results.len());
        let page = if start < results.len() {
            results[start..end].to_vec()
        } else {
            Vec::new()
        };
        let next_cursor = if end < results.len() {
            page.last()
                .map(|result| result.page_revision_id.to_string())
        } else {
            None
        };

        Ok(SemanticSearchResponse {
            query_text: query.text,
            model: self.embedder.model().clone(),
            results: page,
            next_cursor,
            compared_pages,
            skipped_cross_model_pages,
            candidate_matches_created,
        })
    }

}
