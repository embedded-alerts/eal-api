#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn robots_uses_longest_rule_and_allow_wins_ties() {
        let robots = RobotsPolicy::parse(
            "User-agent: *\nDisallow: /private\nAllow: /private/public\n",
            &Url::parse("https://example.com/robots.txt").unwrap(),
        );
        assert!(!robots.allows(&Url::parse("https://example.com/private/a").unwrap()));
        assert!(robots.allows(
            &Url::parse("https://example.com/private/public/a").unwrap()
        ));
        assert!(robots.allows(&Url::parse("https://example.com/news").unwrap()));
    }

    #[test]
    fn exact_bot_group_takes_precedence_over_wildcard() {
        let robots = RobotsPolicy::parse(
            "User-agent: *\nDisallow: /\n\nUser-agent: EmbeddedAlertsBot\nAllow: /news\n",
            &Url::parse("https://example.com/robots.txt").unwrap(),
        );
        assert!(robots.allows(&Url::parse("https://example.com/news").unwrap()));
    }

    #[test]
    fn sitemap_parser_decodes_entities() {
        let locations = parse_xml_locations(
            "<urlset><url><loc>https://example.com/a?x=1&amp;y=2</loc></url></urlset>",
        );
        assert_eq!(locations, vec!["https://example.com/a?x=1&y=2"]);
    }

    #[test]
    fn private_and_documentation_addresses_are_rejected() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.1.1",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
        ] {
            assert!(!is_public_ip(ip.parse().unwrap()), "{ip} must be rejected");
        }
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }
}
