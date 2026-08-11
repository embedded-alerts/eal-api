fn url_signal(url: &Url) -> Option<String> {
    let mut terms = Vec::new();
    if let Some(host) = url.host_str() {
        terms.extend(
            host.split('.')
                .filter(|part| part.len() > 2 && !matches!(*part, "www" | "com" | "org" | "net")),
        );
    }
    terms.extend(
        url.path_segments()
            .into_iter()
            .flatten()
            .flat_map(|segment| segment.split(['-', '_']))
            .filter(|part| part.len() > 2),
    );
    let signal = terms.join(" ");
    (!signal.is_empty()).then_some(signal)
}

fn extract_links(html: &str, base: &Url) -> Vec<Url> {
    let lower = html.to_ascii_lowercase();
    let mut cursor = 0;
    let mut links = BTreeSet::new();
    while let Some(relative) = lower[cursor..].find("href") {
        let start = cursor + relative + 4;
        let rest = &html[start..];
        let Some(equal_relative) = rest.find('=') else {
            cursor = start;
            continue;
        };
        if equal_relative > 8 {
            cursor = start;
            continue;
        }
        let value_start = start + equal_relative + 1;
        let value_rest = html[value_start..].trim_start();
        let whitespace = html[value_start..].len() - value_rest.len();
        let value_start = value_start + whitespace;
        let Some(first) = html[value_start..].chars().next() else {
            break;
        };
        let (raw, consumed) = if matches!(first, '\'' | '"') {
            let content_start = value_start + first.len_utf8();
            let Some(end_relative) = html[content_start..].find(first) else {
                break;
            };
            (&html[content_start..content_start + end_relative], end_relative + 2)
        } else {
            let end = html[value_start..]
                .find(|character: char| character.is_whitespace() || character == '>')
                .unwrap_or(html.len() - value_start);
            (&html[value_start..value_start + end], end)
        };
        cursor = value_start + consumed;
        let raw = decode_html_entities(raw);
        if raw.starts_with('#')
            || raw.starts_with("mailto:")
            || raw.starts_with("tel:")
            || raw.starts_with("javascript:")
            || raw.starts_with("data:")
        {
            continue;
        }
        if let Ok(mut url) = base.join(raw.trim()) {
            url.set_fragment(None);
            if matches!(url.scheme(), "http" | "https") {
                links.insert(url.to_string());
            }
        }
    }
    links
        .into_iter()
        .filter_map(|link| Url::parse(&link).ok())
        .take(500)
        .collect()
}

fn dedupe_text(items: Vec<String>, limit: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.to_ascii_lowercase()))
        .take(limit)
        .collect()
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let mut output: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    output.push('…');
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_keyword_entity_and_sentence_views_without_script_noise() {
        let html = r#"
            <html><head><title>Acme Launches Atlas</title><script>SecretNoise SecretNoise</script></head>
            <body>
              <h1>Acme Corporation launches Atlas in Bogotá</h1>
              <p>Acme Corporation announced that Atlas will monitor renewable energy projects across Colombia.</p>
              <p>The platform helps engineering teams detect project risks before schedules begin to slip.</p>
              <a href="/atlas?utm_source=test">Details</a>
            </body></html>
        "#;
        let page = extract_page(html, &Url::parse("https://example.com/news/atlas").unwrap())
            .expect("page");
        assert_eq!(page.title.as_deref(), Some("Acme Launches Atlas"));
        assert!(page.entities.iter().any(|entity| entity.contains("Acme Corporation")));
        assert!(page.keywords.iter().any(|keyword| keyword == "atlas"));
        assert!(page
            .segments
            .iter()
            .any(|segment| segment.kind == SegmentKind::Sentence));
        assert!(!page.visible_text.contains("SecretNoise"));
        assert!(page.links.iter().any(|url| url.path() == "/atlas"));
    }

    #[test]
    fn query_keeps_full_text_and_companion_views() {
        let query = query_segments(
            "Notify me when Acme Corporation launches renewable energy tools in Colombia.",
        )
        .expect("query");
        assert_eq!(query.segments[0].kind, SegmentKind::Query);
        assert!(query.keywords.iter().any(|keyword| keyword == "renewable"));
        assert!(query.entities.iter().any(|entity| entity.contains("Acme Corporation")));
    }

    #[test]
    fn html_entities_are_decoded() {
        assert_eq!(decode_html_entities("A &amp; B &#x2014; C"), "A & B — C");
    }
}
