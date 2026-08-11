fn extract_tag_texts(html: &str, tag: &str, limit: usize) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let open_prefix = format!("<{tag}");
    let close_tag = format!("</{tag}>");
    let mut cursor = 0;
    let mut result = Vec::new();

    while result.len() < limit {
        let Some(open_relative) = lower[cursor..].find(&open_prefix) else {
            break;
        };
        let open = cursor + open_relative;
        let boundary = lower.as_bytes().get(open + open_prefix.len()).copied();
        if !matches!(boundary, Some(b'>') | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n'))
        {
            cursor = open + open_prefix.len();
            continue;
        }
        let Some(open_end_relative) = lower[open..].find('>') else {
            break;
        };
        let content_start = open + open_end_relative + 1;
        let Some(close_relative) = lower[content_start..].find(&close_tag) else {
            break;
        };
        let content_end = content_start + close_relative;
        let text = normalize_whitespace(&decode_html_entities(&strip_tags(
            &html[content_start..content_end],
        )));
        if !text.is_empty() {
            result.push(truncate_chars(&text, 700));
        }
        cursor = content_end + close_tag.len();
    }
    result
}

fn remove_ignored_elements(html: &str) -> String {
    let mut output = html.to_owned();
    for tag in [
        "script", "style", "noscript", "svg", "canvas", "template", "iframe", "form",
    ] {
        output = remove_element_blocks(&output, tag);
    }
    output
}

fn remove_element_blocks(input: &str, tag: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let open_prefix = format!("<{tag}");
    let close_tag = format!("</{tag}>");
    let mut cursor = 0;
    let mut output = String::with_capacity(input.len());

    while let Some(open_relative) = lower[cursor..].find(&open_prefix) {
        let open = cursor + open_relative;
        output.push_str(&input[cursor..open]);
        let Some(open_end_relative) = lower[open..].find('>') else {
            cursor = open;
            break;
        };
        let content_start = open + open_end_relative + 1;
        let Some(close_relative) = lower[content_start..].find(&close_tag) else {
            cursor = input.len();
            break;
        };
        cursor = content_start + close_relative + close_tag.len();
        output.push(' ');
    }
    output.push_str(&input[cursor..]);
    output
}

fn strip_tags(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut in_tag = false;
    let mut quote: Option<char> = None;
    let mut tag_buffer = String::new();

    for character in input.chars() {
        if in_tag {
            if let Some(active_quote) = quote {
                if character == active_quote {
                    quote = None;
                }
                continue;
            }
            match character {
                '\'' | '"' => quote = Some(character),
                '>' => {
                    in_tag = false;
                    if is_block_tag(&tag_buffer) {
                        output.push_str(". ");
                    } else {
                        output.push(' ');
                    }
                    tag_buffer.clear();
                }
                _ => {
                    if tag_buffer.len() < 24 {
                        tag_buffer.push(character);
                    }
                }
            }
        } else if character == '<' {
            in_tag = true;
            tag_buffer.clear();
        } else {
            output.push(character);
        }
    }
    output
}

fn is_block_tag(tag_buffer: &str) -> bool {
    let tag = tag_buffer
        .trim_start_matches('/')
        .split_ascii_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        tag.as_str(),
        "article"
            | "aside"
            | "blockquote"
            | "br"
            | "div"
            | "footer"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "li"
            | "main"
            | "nav"
            | "p"
            | "section"
            | "td"
            | "th"
            | "tr"
    )
}

fn decode_html_entities(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative) = input[cursor..].find('&') {
        let start = cursor + relative;
        output.push_str(&input[cursor..start]);
        let Some(end_relative) = input[start..].find(';').filter(|offset| *offset <= 15) else {
            output.push('&');
            cursor = start + 1;
            continue;
        };
        let end = start + end_relative;
        let entity = &input[start + 1..end];
        if let Some(decoded) = decode_entity(entity) {
            output.push(decoded);
            cursor = end + 1;
        } else {
            output.push('&');
            cursor = start + 1;
        }
    }
    output.push_str(&input[cursor..]);
    output
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" | "#39" => Some('\''),
        "nbsp" => Some(' '),
        _ if entity.starts_with("#x") || entity.starts_with("#X") => {
            u32::from_str_radix(&entity[2..], 16).ok().and_then(char::from_u32)
        }
        _ if entity.starts_with('#') => entity[1..].parse().ok().and_then(char::from_u32),
        _ => None,
    }
}

