use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use super::SemanticError;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DiscoveryMode {
    Seed,
    RobotsSitemap,
    Sitemap,
    Rss,
    LinkCrawl,
    ExternalIndex,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SourceDomain {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub name: String,
    pub base_url: String,
    pub host: String,
    pub include_subdomains: bool,
    pub seed_urls: Vec<String>,
    pub discovery_modes: Vec<DiscoveryMode>,
    pub max_pages_per_scan: usize,
    pub source_priority: f32,
    pub respect_robots: bool,
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CreateSourceDomain {
    pub name: String,
    pub domain: String,
    #[serde(default)]
    pub include_subdomains: bool,
    #[serde(default)]
    pub seed_urls: Vec<String>,
    #[serde(default = "default_discovery_modes")]
    pub discovery_modes: Vec<DiscoveryMode>,
    #[serde(default = "default_max_pages_per_scan")]
    pub max_pages_per_scan: usize,
    #[serde(default = "default_source_priority")]
    pub source_priority: f32,
    #[serde(default = "default_true")]
    pub respect_robots: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_discovery_modes() -> Vec<DiscoveryMode> {
    vec![
        DiscoveryMode::Seed,
        DiscoveryMode::RobotsSitemap,
        DiscoveryMode::Sitemap,
        DiscoveryMode::LinkCrawl,
    ]
}

const fn default_max_pages_per_scan() -> usize {
    25
}

const fn default_source_priority() -> f32 {
    0.5
}

const fn default_true() -> bool {
    true
}

impl SourceDomain {
    pub(crate) fn create(
        tenant_id: Uuid,
        input: CreateSourceDomain,
    ) -> Result<Self, SemanticError> {
        let name = input.name.trim();
        if name.is_empty() || name.chars().count() > 120 {
            return Err(SemanticError::invalid(
                "source_name",
                "source name must contain 1 to 120 characters",
            ));
        }
        if !(1..=100).contains(&input.max_pages_per_scan) {
            return Err(SemanticError::invalid(
                "max_pages_per_scan",
                "max_pages_per_scan must be between 1 and 100",
            ));
        }
        if !input.source_priority.is_finite() || !(0.0..=1.0).contains(&input.source_priority) {
            return Err(SemanticError::invalid(
                "source_priority",
                "source_priority must be a finite value between 0 and 1",
            ));
        }
        if input.seed_urls.len() > 50 {
            return Err(SemanticError::invalid(
                "seed_urls",
                "a source may have at most 50 seed URLs",
            ));
        }

        let base_url = normalize_base_url(&input.domain)?;
        let host = base_url
            .host_str()
            .ok_or_else(|| SemanticError::invalid("domain", "domain must contain a host"))?
            .trim_end_matches('.')
            .to_ascii_lowercase();
        validate_public_domain_name(&host)?;

        let mut discovery_modes = input.discovery_modes;
        discovery_modes.sort_by_key(|mode| *mode as u8);
        discovery_modes.dedup();
        if discovery_modes.is_empty() {
            discovery_modes.push(DiscoveryMode::Seed);
        }

        let mut source = Self {
            id: Uuid::new_v4(),
            tenant_id,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            name: name.to_owned(),
            base_url: base_url.to_string(),
            host,
            include_subdomains: input.include_subdomains,
            seed_urls: Vec::new(),
            discovery_modes,
            max_pages_per_scan: input.max_pages_per_scan,
            source_priority: input.source_priority,
            respect_robots: input.respect_robots,
            enabled: input.enabled,
        };

        let supplied_seeds = if input.seed_urls.is_empty() {
            vec![source.base_url.clone()]
        } else {
            input.seed_urls
        };
        let mut seeds = BTreeSet::new();
        for seed in supplied_seeds {
            let parsed = Url::parse(seed.trim()).map_err(|error| {
                SemanticError::invalid(
                    "seed_urls",
                    format!("invalid seed URL {seed:?}: {error}"),
                )
            })?;
            let canonical = source.canonicalize_url(&parsed)?;
            seeds.insert(canonical.to_string());
        }
        source.seed_urls = seeds.into_iter().collect();
        Ok(source)
    }

    pub(crate) fn base(&self) -> Result<Url, SemanticError> {
        Url::parse(&self.base_url).map_err(|error| {
            SemanticError::internal(format!("stored source base URL is invalid: {error}"))
        })
    }

    pub(crate) fn has_mode(&self, mode: DiscoveryMode) -> bool {
        self.discovery_modes.contains(&mode)
    }

    pub(crate) fn allows_url(&self, url: &Url) -> bool {
        if !matches!(url.scheme(), "http" | "https") {
            return false;
        }
        if !url.username().is_empty() || url.password().is_some() {
            return false;
        }
        let Some(host) = url.host_str() else {
            return false;
        };
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        host == self.host
            || (self.include_subdomains
                && host.len() > self.host.len()
                && host.ends_with(&format!(".{}", self.host)))
    }

    pub(crate) fn canonicalize_url(&self, url: &Url) -> Result<Url, SemanticError> {
        if !self.allows_url(url) {
            return Err(SemanticError::forbidden(
                "source_domain",
                format!("URL is outside the configured source domain: {url}"),
            ));
        }
        canonicalize_url(url)
    }
}

pub(crate) fn canonicalize_url(url: &Url) -> Result<Url, SemanticError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(SemanticError::invalid(
            "url_scheme",
            "only http and https URLs can be indexed",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SemanticError::invalid(
            "url_credentials",
            "URLs containing credentials cannot be indexed",
        ));
    }

    let mut canonical = url.clone();
    canonical.set_fragment(None);
    if (canonical.scheme() == "https" && canonical.port() == Some(443))
        || (canonical.scheme() == "http" && canonical.port() == Some(80))
    {
        canonical
            .set_port(None)
            .map_err(|()| SemanticError::invalid("url_port", "invalid URL port"))?;
    }

    let mut retained: Vec<(String, String)> = canonical
        .query_pairs()
        .filter(|(key, _)| !is_tracking_parameter(key))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    retained.sort();
    canonical.set_query(None);
    if !retained.is_empty() {
        canonical.query_pairs_mut().extend_pairs(retained);
    }

    let path = canonical.path();
    if path.len() > 1 && path.ends_with('/') {
        let trimmed = path.trim_end_matches('/').to_owned();
        canonical.set_path(&trimmed);
    }
    Ok(canonical)
}

fn normalize_base_url(input: &str) -> Result<Url, SemanticError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(SemanticError::invalid(
            "domain",
            "domain must not be empty",
        ));
    }
    let candidate = if input.contains("://") {
        input.to_owned()
    } else {
        format!("https://{input}")
    };
    let mut url = Url::parse(&candidate).map_err(|error| {
        SemanticError::invalid("domain", format!("invalid source domain: {error}"))
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(SemanticError::invalid(
            "domain_scheme",
            "source domains must use http or https",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SemanticError::invalid(
            "domain_credentials",
            "source domains cannot contain credentials",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(SemanticError::invalid(
            "domain_url",
            "source domain must not contain a query string or fragment",
        ));
    }
    url.set_path("/");
    Ok(url)
}

fn validate_public_domain_name(host: &str) -> Result<(), SemanticError> {
    if host.parse::<std::net::IpAddr>().is_ok() {
        return Err(SemanticError::invalid(
            "domain_host",
            "configure a DNS domain name rather than an IP address",
        ));
    }
    if host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || !host.contains('.')
    {
        return Err(SemanticError::invalid(
            "domain_host",
            "source host must be a public DNS domain",
        ));
    }
    Ok(())
}

fn is_tracking_parameter(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.starts_with("utm_")
        || key.starts_with("mc_")
        || matches!(
            key.as_str(),
            "gclid" | "dclid" | "fbclid" | "msclkid" | "igshid" | "ref_src"
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(include_subdomains: bool) -> SourceDomain {
        SourceDomain::create(
            Uuid::nil(),
            CreateSourceDomain {
                name: "Example".into(),
                domain: "example.com".into(),
                include_subdomains,
                seed_urls: Vec::new(),
                discovery_modes: default_discovery_modes(),
                max_pages_per_scan: 25,
                source_priority: 0.5,
                respect_robots: true,
                enabled: true,
            },
        )
        .expect("source")
    }

    #[test]
    fn enforces_exact_domain_boundaries() {
        let exact = source(false);
        assert!(exact.allows_url(&Url::parse("https://example.com/a").unwrap()));
        assert!(!exact.allows_url(&Url::parse("https://news.example.com/a").unwrap()));
        assert!(!exact.allows_url(&Url::parse("https://example.com.evil.test/a").unwrap()));

        let subdomains = source(true);
        assert!(subdomains.allows_url(&Url::parse("https://news.example.com/a").unwrap()));
        assert!(!subdomains.allows_url(&Url::parse("https://example.com.evil.test/a").unwrap()));
    }

    #[test]
    fn canonicalization_removes_tracking_and_fragments() {
        let source = source(false);
        let canonical = source
            .canonicalize_url(
                &Url::parse(
                    "https://example.com/story/?utm_source=x&b=2&a=1&fbclid=secret#section",
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(canonical.as_str(), "https://example.com/story?a=1&b=2");
    }

    #[test]
    fn rejects_non_public_source_names() {
        for domain in ["localhost", "service.internal", "printer.local", "10.0.0.1"] {
            let result = SourceDomain::create(
                Uuid::nil(),
                CreateSourceDomain {
                    name: "bad".into(),
                    domain: domain.into(),
                    include_subdomains: false,
                    seed_urls: Vec::new(),
                    discovery_modes: default_discovery_modes(),
                    max_pages_per_scan: 10,
                    source_priority: 0.5,
                    respect_robots: true,
                    enabled: true,
                },
            );
            assert!(result.is_err(), "{domain} must be rejected");
        }
    }
}
