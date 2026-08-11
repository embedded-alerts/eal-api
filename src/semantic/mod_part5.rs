fn record_failure(report: &mut ScanReport, url: String, error: SemanticError) {
    report.failed += 1;
    if report.failures.len() < 50 {
        report.failures.push(ScanFailure {
            url,
            code: error.code().to_owned(),
            message: error.message().to_owned(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_input() -> CreateSourceDomain {
        CreateSourceDomain {
            name: "Example News".into(),
            domain: "example.com".into(),
            include_subdomains: false,
            seed_urls: Vec::new(),
            discovery_modes: vec![DiscoveryMode::Seed],
            max_pages_per_scan: 10,
            source_priority: 0.8,
            respect_robots: true,
            enabled: true,
        }
    }

    fn page_html(extra: &str) -> String {
        format!(
            "<html><head><title>Acme Atlas</title></head><body><h1>Acme Corporation Atlas</h1><p>Acme Corporation launched Atlas to monitor renewable energy projects across Colombia.</p><p>{extra} Engineering teams can detect risks before project schedules begin to slip.</p></body></html>"
        )
    }

    #[tokio::test]
    async fn unchanged_ingestion_is_a_noop_and_changed_content_links_revision() {
        let service = SemanticService::for_test();
        let tenant = Uuid::new_v4();
        let source = service
            .register_source(tenant, source_input())
            .await
            .unwrap();
        let request = |html| IngestPageRequest {
            url: "https://example.com/atlas".into(),
            html,
            content_type: None,
            etag: None,
            last_modified: None,
        };

        let first = service
            .ingest_supplied_page(tenant, source.id, request(page_html("New.")))
            .await
            .unwrap();
        assert_eq!(first.disposition, IngestDisposition::Created);
        let second = service
            .ingest_supplied_page(tenant, source.id, request(page_html("New.")))
            .await
            .unwrap();
        assert_eq!(second.disposition, IngestDisposition::Unchanged);
        assert_eq!(first.page_revision_id, second.page_revision_id);

        let third = service
            .ingest_supplied_page(tenant, source.id, request(page_html("Updated.")))
            .await
            .unwrap();
        assert_eq!(third.disposition, IngestDisposition::Updated);
        assert_eq!(third.previous_revision_id, Some(first.page_revision_id));
    }

    #[tokio::test]
    async fn search_creates_deduplicated_candidates_without_delivery() {
        let service = SemanticService::for_test();
        let tenant = Uuid::new_v4();
        let source = service
            .register_source(tenant, source_input())
            .await
            .unwrap();
        service
            .ingest_supplied_page(
                tenant,
                source.id,
                IngestPageRequest {
                    url: "https://example.com/atlas".into(),
                    html: page_html("New."),
                    content_type: None,
                    etag: None,
                    last_modified: None,
                },
            )
            .await
            .unwrap();

        let rule = AlertRuleRevisionRef {
            id: Uuid::new_v4(),
            revision: 1,
        };
        let request = || SemanticSearchRequest {
            query_text: "renewable energy monitoring projects in Colombia".into(),
            source_ids: vec![source.id],
            threshold: 0.0,
            limit: 20,
            cursor: None,
            expected_model: Some(service.embedding_model().clone()),
            alert_rule: Some(rule.clone()),
        };
        let first = service.search(tenant, request()).await.unwrap();
        assert_eq!(first.results.len(), 1);
        assert_eq!(first.candidate_matches_created, 1);
        let second = service.search(tenant, request()).await.unwrap();
        assert_eq!(second.candidate_matches_created, 0);
        let matches = service.list_matches(tenant).await;
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].state, "candidate");
    }

    #[tokio::test]
    async fn tenants_cannot_observe_each_others_sources() {
        let service = SemanticService::for_test();
        let first_tenant = Uuid::new_v4();
        let second_tenant = Uuid::new_v4();
        let source = service
            .register_source(first_tenant, source_input())
            .await
            .unwrap();
        assert!(service
            .ingest_supplied_page(
                second_tenant,
                source.id,
                IngestPageRequest {
                    url: "https://example.com/atlas".into(),
                    html: page_html("New."),
                    content_type: None,
                    etag: None,
                    last_modified: None,
                },
            )
            .await
            .is_err());
    }
}
