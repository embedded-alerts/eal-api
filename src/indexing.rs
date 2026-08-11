use eal_interfaces::{CreateSourcePolicy, SourcePolicy, ValidationError};
use url::{Host, Url};

pub fn canonicalize_source_root(input: &CreateSourcePolicy) -> Result<String, ValidationError> {
    canonicalize(
        &input.root_url,
        &input.allowed_hosts,
        input.include_subdomains,
        &input.allowed_path_prefixes,
    )
}

pub fn canonicalize_for_source(
    source: &SourcePolicy,
    raw_url: &str,
) -> Result<String, ValidationError> {
    canonicalize(
        raw_url,
        &source.allowed_hosts,
        source.include_subdomains,
        &source.allowed_path_prefixes,
    )
}

fn canonicalize(
    raw_url: &str,
    allowed_hosts: &[String],
    include_subdomains: bool,
    allowed_path_prefixes: &[String],
) -> Result<String, ValidationError> {
    let mut url = Url::parse(raw_url)
        .map_err(|error| ValidationError(format!("invalid absolute URL: {error}")))?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(ValidationError(
            "only http and https URLs may be indexed".into(),
        ));
    }
    if url.cannot_be_a_base() {
        return Err(ValidationError("URL cannot be used as a crawl base".into()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ValidationError(
            "URL credentials are forbidden for indexed sources".into(),
        ));
    }

    let host = match url.host() {
        Some(Host::Domain(host)) => normalize_host(host),
        Some(Host::Ipv4(_)) | Some(Host::Ipv6(_)) => {
            return Err(ValidationError(
                "literal IP addresses are forbidden; configure an exact public domain".into(),
            ));
        }
        None => return Err(ValidationError("URL must include a public host".into())),
    };

    if host == "localhost" || !host.contains('.') {
        return Err(ValidationError(
            "URL must use an exact public DNS host".into(),
        ));
    }
    if !host_is_allowed(&host, allowed_hosts, include_subdomains) {
        return Err(ValidationError(format!(
            "host {host} is outside the source allowlist"
        )));
    }
    if !allowed_path_prefixes
        .iter()
        .any(|prefix| path_is_allowed(url.path(), prefix))
    {
        return Err(ValidationError(format!(
            "path {} is outside the source allowlist",
            url.path()
        )));
    }

    let is_default_port = matches!(
        (url.scheme(), url.port()),
        ("http", Some(80)) | ("https", Some(443))
    );
    if is_default_port {
        url.set_port(None)
            .map_err(|()| ValidationError("could not normalize URL port".into()))?;
    }
    url.set_fragment(None);

    Ok(url.to_string())
}

fn host_is_allowed(host: &str, allowed_hosts: &[String], include_subdomains: bool) -> bool {
    allowed_hosts.iter().any(|allowed| {
        let allowed = normalize_host(allowed);
        host == allowed
            || (include_subdomains
                && host
                    .strip_suffix(&allowed)
                    .is_some_and(|prefix| prefix.ends_with('.') && prefix.len() > 1))
    })
}

fn path_is_allowed(path: &str, prefix: &str) -> bool {
    if prefix == "/" {
        return true;
    }
    let prefix = prefix.trim_end_matches('/');
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|remaining| remaining.starts_with('/'))
}

fn normalize_host(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use eal_interfaces::DiscoveryMode;

    use super::*;

    fn source(include_subdomains: bool) -> CreateSourcePolicy {
        CreateSourcePolicy {
            name: "Rust blog".into(),
            root_url: "https://blog.rust-lang.org/".into(),
            allowed_hosts: vec!["blog.rust-lang.org".into()],
            allowed_path_prefixes: vec!["/inside".into()],
            include_subdomains,
            discovery_modes: vec![DiscoveryMode::Sitemap],
            crawl_interval_seconds: 900,
            max_depth: 3,
            max_pages_per_run: 100,
            obey_robots: true,
            enabled: true,
        }
    }

    #[test]
    fn canonicalization_removes_fragments_and_default_ports() {
        let mut policy = source(false);
        policy.root_url = "https://blog.rust-lang.org:443/inside/item#fragment".into();
        assert_eq!(
            canonicalize_source_root(&policy).unwrap(),
            "https://blog.rust-lang.org/inside/item"
        );
    }

    #[test]
    fn path_prefixes_are_segment_aware() {
        let mut policy = source(false);
        policy.root_url = "https://blog.rust-lang.org/inside-item".into();
        assert!(canonicalize_source_root(&policy).is_err());

        policy.root_url = "https://blog.rust-lang.org/inside/item".into();
        assert!(canonicalize_source_root(&policy).is_ok());
    }

    #[test]
    fn rejects_credentials_ips_and_unlisted_hosts() {
        let mut policy = source(false);
        policy.root_url = "https://user:secret@blog.rust-lang.org/inside".into();
        assert!(canonicalize_source_root(&policy).is_err());

        policy.root_url = "https://127.0.0.1/inside".into();
        policy.allowed_hosts = vec!["127.0.0.1".into()];
        assert!(canonicalize_source_root(&policy).is_err());

        policy.root_url = "https://example.com/inside".into();
        policy.allowed_hosts = vec!["blog.rust-lang.org".into()];
        assert!(canonicalize_source_root(&policy).is_err());
    }

    #[test]
    fn subdomains_are_opt_in_and_boundary_checked() {
        let mut policy = source(false);
        policy.root_url = "https://updates.blog.rust-lang.org/inside".into();
        assert!(canonicalize_source_root(&policy).is_err());

        policy.include_subdomains = true;
        assert!(canonicalize_source_root(&policy).is_ok());

        policy.root_url = "https://notblog.rust-lang.org/inside".into();
        assert!(canonicalize_source_root(&policy).is_err());
    }
}
