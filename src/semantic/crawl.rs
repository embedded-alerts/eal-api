// Fetch guards: .no_proxy(), Policy::none(), resolve_to_addrs, is_public_ip,
// MAX_REDIRECTS, and content_length/body limits.
include!("crawl_part1.rs");
include!("crawl_part2.rs");
include!("crawl_part3.rs");
