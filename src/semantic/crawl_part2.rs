impl RobotsPolicy {
    pub(crate) fn parse(text: &str, robots_url: &Url) -> Self {
        let mut exact_groups = Vec::<Vec<RobotsRule>>::new();
        let mut wildcard_groups = Vec::<Vec<RobotsRule>>::new();
        let mut current_agents = Vec::<String>::new();
        let mut current_rules = Vec::<RobotsRule>::new();
        let mut sitemaps = BTreeSet::new();

        let flush_group = |agents: &mut Vec<String>,
                           rules: &mut Vec<RobotsRule>,
                           exact: &mut Vec<Vec<RobotsRule>>,
                           wildcard: &mut Vec<Vec<RobotsRule>>| {
            if agents.is_empty() {
                rules.clear();
                return;
            }
            if agents.iter().any(|agent| agent == "embeddedalertsbot") {
                exact.push(std::mem::take(rules));
            } else if agents.iter().any(|agent| agent == "*") {
                wildcard.push(std::mem::take(rules));
            } else {
                rules.clear();
            }
            agents.clear();
        };

        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();
            match name.as_str() {
                "user-agent" => {
                    if !current_rules.is_empty() {
                        flush_group(
                            &mut current_agents,
                            &mut current_rules,
                            &mut exact_groups,
                            &mut wildcard_groups,
                        );
                    }
                    current_agents.push(value.to_ascii_lowercase());
                }
                "allow" | "disallow" if !current_agents.is_empty() => {
                    if !value.is_empty() {
                        current_rules.push(RobotsRule {
                            allow: name == "allow",
                            pattern: value.to_owned(),
                        });
                    }
                }
                "sitemap" => {
                    if let Ok(url) = robots_url.join(value)
                        && matches!(url.scheme(), "http" | "https")
                    {
                        sitemaps.insert(url.to_string());
                    }
                }
                _ => {}
            }
        }
        flush_group(
            &mut current_agents,
            &mut current_rules,
            &mut exact_groups,
            &mut wildcard_groups,
        );

        let groups = if exact_groups.is_empty() {
            wildcard_groups
        } else {
            exact_groups
        };
        Self {
            rules: groups.into_iter().flatten().collect(),
            sitemaps: sitemaps
                .into_iter()
                .filter_map(|url| Url::parse(&url).ok())
                .collect(),
        }
    }

    pub(crate) fn allows(&self, url: &Url) -> bool {
        let mut target = url.path().to_owned();
        if let Some(query) = url.query() {
            target.push('?');
            target.push_str(query);
        }
        let mut winner: Option<&RobotsRule> = None;
        for rule in &self.rules {
            if robots_pattern_matches(&rule.pattern, &target) {
                let replace = winner.is_none_or(|current| {
                    rule.pattern.len() > current.pattern.len()
                        || (rule.pattern.len() == current.pattern.len()
                            && rule.allow
                            && !current.allow)
                });
                if replace {
                    winner = Some(rule);
                }
            }
        }
        winner.is_none_or(|rule| rule.allow)
    }
}

fn robots_pattern_matches(pattern: &str, target: &str) -> bool {
    let anchored = pattern.ends_with('$');
    let pattern = pattern.trim_end_matches('$');
    if !pattern.contains('*') {
        return if anchored {
            target == pattern
        } else {
            target.starts_with(pattern)
        };
    }

    let mut cursor = 0;
    for (index, part) in pattern.split('*').enumerate() {
        if part.is_empty() {
            continue;
        }
        let Some(relative) = target[cursor..].find(part) else {
            return false;
        };
        if index == 0 && !pattern.starts_with('*') && relative != 0 {
            return false;
        }
        cursor += relative + part.len();
    }
    !anchored || cursor == target.len()
}

fn parse_xml_locations(xml: &str) -> Vec<String> {
    let lower = xml.to_ascii_lowercase();
    let mut cursor = 0;
    let mut locations = Vec::new();
    while let Some(open_relative) = lower[cursor..].find("<loc") {
        let open = cursor + open_relative;
        let Some(open_end_relative) = lower[open..].find('>') else {
            break;
        };
        let content_start = open + open_end_relative + 1;
        let Some(close_relative) = lower[content_start..].find("</loc>") else {
            break;
        };
        let content_end = content_start + close_relative;
        let value = decode_xml_entities(xml[content_start..content_end].trim());
        if !value.is_empty() {
            locations.push(value);
        }
        cursor = content_end + "</loc>".len();
        if locations.len() >= MAX_DISCOVERED_URLS {
            break;
        }
    }
    locations
}

fn decode_xml_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn looks_like_sitemap(url: &Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    path.contains("sitemap") || path.ends_with(".xml.gz")
}

fn header_string(headers: &reqwest::header::HeaderMap, name: reqwest::header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn ensure_fetchable_url(source: &SourceDomain, url: &Url) -> Result<(), SemanticError> {
    if !source.allows_url(url) {
        return Err(SemanticError::forbidden(
            "source_domain",
            format!("URL is outside the configured source domain: {url}"),
        ));
    }
    let canonical = canonicalize_url(url)?;
    if &canonical != url {
        return Err(SemanticError::invalid(
            "canonical_url",
            "crawler received a non-canonical URL",
        ));
    }
    Ok(())
}

async fn resolve_public_addresses(
    host: &str,
    port: u16,
) -> Result<Vec<SocketAddr>, SemanticError> {
    let mut addresses: Vec<SocketAddr> = lookup_host((host, port))
        .await
        .map_err(|error| SemanticError::fetch(format!("resolve {host}: {error}")))?
        .filter(|address| is_public_ip(address.ip()))
        .collect();
    addresses.sort();
    addresses.dedup();
    if addresses.is_empty() {
        return Err(SemanticError::forbidden(
            "private_network",
            "source domain did not resolve to a public IP address",
        ));
    }
    Ok(addresses)
}

pub(crate) fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => is_public_ipv6(ip),
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    if ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip == Ipv4Addr::BROADCAST
    {
        return false;
    }
    if octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
        || (octets[0] == 198 && octets[1] == 18)
        || (octets[0] == 198 && octets[1] == 19)
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
        || octets[0] >= 240
    {
        return false;
    }
    true
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
        return false;
    }
    let segments = ip.segments();
    if segments[0] & 0xfe00 == 0xfc00
        || segments[0] & 0xffc0 == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
    {
        return false;
    }
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_public_ipv4(mapped);
    }
    true
}

impl From<url::ParseError> for SemanticError {
    fn from(error: url::ParseError) -> Self {
        Self::invalid("url", format!("invalid URL: {error}"))
    }
}

