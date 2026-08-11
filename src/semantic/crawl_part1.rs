use std::{
    collections::{BTreeSet, HashSet},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use futures_util::StreamExt;
use reqwest::{
    header::{ACCEPT, CONTENT_TYPE, ETAG, LAST_MODIFIED, LOCATION},
    redirect::Policy,
    StatusCode,
};
use tokio::net::lookup_host;
use url::Url;

use super::{
    domain::{canonicalize_url, DiscoveryMode, SourceDomain},
    SemanticError,
};

const DEFAULT_HTML_LIMIT: usize = 2 * 1024 * 1024;
const DEFAULT_DISCOVERY_LIMIT: usize = 4 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const MAX_SITEMAPS: usize = 12;
const MAX_DISCOVERED_URLS: usize = 2_000;

#[derive(Debug, Clone)]
pub(crate) struct Crawler {
    user_agent: String,
    html_limit: usize,
    discovery_limit: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct FetchedPage {
    pub requested_url: Url,
    pub final_url: Url,
    pub content_type: String,
    pub body: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct DiscoveryResult {
    pub urls: Vec<Url>,
    pub robots: RobotsPolicy,
    pub sitemap_count: usize,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RobotsPolicy {
    rules: Vec<RobotsRule>,
    pub sitemaps: Vec<Url>,
}

#[derive(Debug, Clone)]
struct RobotsRule {
    allow: bool,
    pattern: String,
}

#[derive(Debug, Clone)]
struct FetchedResource {
    requested_url: Url,
    final_url: Url,
    status: StatusCode,
    content_type: String,
    body: Vec<u8>,
    etag: Option<String>,
    last_modified: Option<String>,
}

impl Crawler {
    pub(crate) fn new() -> Self {
        Self {
            user_agent: "EmbeddedAlertsBot/0.1 (+https://embedded-alerts.github.io/)".into(),
            html_limit: DEFAULT_HTML_LIMIT,
            discovery_limit: DEFAULT_DISCOVERY_LIMIT,
        }
    }

    pub(crate) async fn discover(
        &self,
        source: &SourceDomain,
    ) -> Result<DiscoveryResult, SemanticError> {
        let base = source.base()?;
        let mut urls = BTreeSet::new();
        if source.has_mode(DiscoveryMode::Seed) {
            for seed in &source.seed_urls {
                if let Ok(seed) = Url::parse(seed)
                    && let Ok(seed) = source.canonicalize_url(&seed)
                {
                    urls.insert(seed.to_string());
                }
            }
        }

        let robots = if source.respect_robots
            || source.has_mode(DiscoveryMode::RobotsSitemap)
            || source.has_mode(DiscoveryMode::Sitemap)
        {
            self.fetch_robots(source, &base).await?
        } else {
            RobotsPolicy::default()
        };

        let mut sitemap_urls = BTreeSet::new();
        if source.has_mode(DiscoveryMode::RobotsSitemap) {
            sitemap_urls.extend(robots.sitemaps.iter().map(ToString::to_string));
        }
        if source.has_mode(DiscoveryMode::Sitemap) {
            sitemap_urls.insert(base.join("/sitemap.xml")?.to_string());
        }

        let mut sitemap_count = 0;
        let mut pending: Vec<String> = sitemap_urls.into_iter().collect();
        let mut seen_sitemaps = HashSet::new();
        while let Some(sitemap_url) = pending.pop() {
            if sitemap_count >= MAX_SITEMAPS || urls.len() >= MAX_DISCOVERED_URLS {
                break;
            }
            if !seen_sitemaps.insert(sitemap_url.clone()) {
                continue;
            }
            let sitemap_url = Url::parse(&sitemap_url).map_err(|error| {
                SemanticError::invalid("sitemap_url", format!("invalid sitemap URL: {error}"))
            })?;
            if !source.allows_url(&sitemap_url) {
                continue;
            }
            let resource = match self
                .fetch_resource(
                    source,
                    sitemap_url,
                    &[
                        "application/xml",
                        "text/xml",
                        "application/rss+xml",
                        "application/atom+xml",
                        "text/plain",
                    ],
                    self.discovery_limit,
                )
                .await
            {
                Ok(resource) if resource.status.is_success() => resource,
                Ok(_) => continue,
                Err(_) => continue,
            };
            sitemap_count += 1;
            let text = String::from_utf8_lossy(&resource.body);
            for location in parse_xml_locations(&text) {
                let Ok(location) = resource.final_url.join(location.trim()) else {
                    continue;
                };
                if !source.allows_url(&location) {
                    continue;
                }
                if looks_like_sitemap(&location) {
                    if seen_sitemaps.len() + pending.len() < MAX_SITEMAPS * 2 {
                        pending.push(location.to_string());
                    }
                } else if let Ok(canonical) = source.canonicalize_url(&location) {
                    urls.insert(canonical.to_string());
                    if urls.len() >= MAX_DISCOVERED_URLS {
                        break;
                    }
                }
            }
        }

        Ok(DiscoveryResult {
            urls: urls
                .into_iter()
                .filter_map(|url| Url::parse(&url).ok())
                .collect(),
            robots,
            sitemap_count,
        })
    }

    pub(crate) async fn fetch_html(
        &self,
        source: &SourceDomain,
        url: Url,
    ) -> Result<FetchedPage, SemanticError> {
        let resource = self
            .fetch_resource(
                source,
                url,
                &["text/html", "application/xhtml+xml"],
                self.html_limit,
            )
            .await?;
        if !resource.status.is_success() {
            return Err(SemanticError::fetch(format!(
                "page returned HTTP {}",
                resource.status.as_u16()
            )));
        }
        let body = String::from_utf8_lossy(&resource.body).into_owned();
        Ok(FetchedPage {
            requested_url: resource.requested_url,
            final_url: resource.final_url,
            content_type: resource.content_type,
            body,
            etag: resource.etag,
            last_modified: resource.last_modified,
        })
    }

    async fn fetch_robots(
        &self,
        source: &SourceDomain,
        base: &Url,
    ) -> Result<RobotsPolicy, SemanticError> {
        let robots_url = base.join("/robots.txt")?;
        let resource = self
            .fetch_resource(
                source,
                robots_url,
                &["text/plain", "text/html", "application/octet-stream"],
                512 * 1024,
            )
            .await?;
        if resource.status == StatusCode::NOT_FOUND {
            return Ok(RobotsPolicy::default());
        }
        if !resource.status.is_success() {
            return Err(SemanticError::fetch(format!(
                "robots.txt returned HTTP {}",
                resource.status.as_u16()
            )));
        }
        let text = String::from_utf8_lossy(&resource.body);
        Ok(RobotsPolicy::parse(&text, &resource.final_url))
    }

    async fn fetch_resource(
        &self,
        source: &SourceDomain,
        requested_url: Url,
        accepted_types: &[&str],
        max_bytes: usize,
    ) -> Result<FetchedResource, SemanticError> {
        let requested_url = source.canonicalize_url(&requested_url)?;
        let mut current = requested_url.clone();

        for redirect_count in 0..=MAX_REDIRECTS {
            ensure_fetchable_url(source, &current)?;
            let host = current
                .host_str()
                .ok_or_else(|| SemanticError::invalid("url_host", "URL has no host"))?;
            let port = current.port_or_known_default().ok_or_else(|| {
                SemanticError::invalid("url_port", "URL has no known destination port")
            })?;
            let addresses = resolve_public_addresses(host, port).await?;
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(Policy::none())
                .resolve_to_addrs(host, &addresses)
                .connect_timeout(Duration::from_secs(5))
                .read_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .user_agent(self.user_agent.clone())
                .build()
                .map_err(|error| SemanticError::fetch(format!("build HTTP client: {error}")))?;

            let response = client
                .get(current.clone())
                .header(ACCEPT, accepted_types.join(", "))
                .send()
                .await
                .map_err(|error| SemanticError::fetch(format!("GET {current}: {error}")))?;

            if let Some(remote_addr) = response.remote_addr()
                && !is_public_ip(remote_addr.ip())
            {
                return Err(SemanticError::forbidden(
                    "private_network",
                    "remote server resolved to a non-public address",
                ));
            }

            if response.status().is_redirection() {
                if redirect_count == MAX_REDIRECTS {
                    return Err(SemanticError::fetch(
                        "redirect limit exceeded while fetching source",
                    ));
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .ok_or_else(|| SemanticError::fetch("redirect response omitted Location"))?
                    .to_str()
                    .map_err(|_| SemanticError::fetch("redirect Location was not valid UTF-8"))?;
                let next = current.join(location).map_err(|error| {
                    SemanticError::fetch(format!("invalid redirect Location: {error}"))
                })?;
                current = source.canonicalize_url(&next)?;
                continue;
            }

            let status = response.status();
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();
            if status.is_success()
                && !accepted_types
                    .iter()
                    .any(|accepted| content_type.starts_with(accepted))
            {
                return Err(SemanticError::invalid(
                    "content_type",
                    format!("unsupported content type {content_type:?}"),
                ));
            }
            if response
                .content_length()
                .is_some_and(|length| length > max_bytes as u64)
            {
                return Err(SemanticError::invalid(
                    "content_length",
                    format!("response exceeds the {max_bytes}-byte limit"),
                ));
            }

            let etag = header_string(response.headers(), ETAG);
            let last_modified = header_string(response.headers(), LAST_MODIFIED);
            let mut stream = response.bytes_stream();
            let mut body = Vec::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| {
                    SemanticError::fetch(format!("read response body: {error}"))
                })?;
                if body.len().saturating_add(chunk.len()) > max_bytes {
                    return Err(SemanticError::invalid(
                        "content_length",
                        format!("response exceeds the {max_bytes}-byte limit"),
                    ));
                }
                body.extend_from_slice(&chunk);
            }

            return Ok(FetchedResource {
                requested_url,
                final_url: current,
                status,
                content_type,
                body,
                etag,
                last_modified,
            });
        }
        Err(SemanticError::fetch(
            "redirect loop ended without a response",
        ))
    }
}

