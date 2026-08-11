use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};
use url::Url;

use super::SemanticError;

pub(crate) const EXTRACTOR_VERSION: &str = "html-multiview-v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SegmentKind {
    Title,
    Heading,
    Summary,
    Sentence,
    Entity,
    Keyword,
    UrlSignal,
    Query,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TextSegment {
    pub kind: SegmentKind,
    pub text: String,
    pub weight: f32,
    pub ordinal: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct ExtractedPage {
    pub title: Option<String>,
    pub summary: String,
    pub visible_text: String,
    pub keywords: Vec<String>,
    pub entities: Vec<String>,
    pub segments: Vec<TextSegment>,
    pub links: Vec<Url>,
}

pub(crate) fn extract_page(html: &str, canonical_url: &Url) -> Result<ExtractedPage, SemanticError> {
    if html.trim().is_empty() {
        return Err(SemanticError::invalid(
            "html",
            "fetched HTML document is empty",
        ));
    }

    let title = extract_tag_texts(html, "title", 1).into_iter().next();
    let mut headings = Vec::new();
    for tag in ["h1", "h2", "h3"] {
        headings.extend(extract_tag_texts(html, tag, 12));
    }
    headings = dedupe_text(headings, 20);

    let without_noise = remove_ignored_elements(html);
    let visible_text = normalize_whitespace(&decode_html_entities(&strip_tags(&without_noise)));
    if visible_text.chars().count() < 20 {
        return Err(SemanticError::invalid(
            "html_content",
            "HTML document has too little visible text to index",
        ));
    }

    let sentences = complete_sentences(&visible_text, 48);
    let summary = build_summary(&sentences, &visible_text);
    let keywords = extract_keywords(&visible_text, 32);
    let entities = extract_entities(&visible_text, 32);
    let links = extract_links(html, canonical_url);

    let mut segments = Vec::new();
    if let Some(title) = title.as_ref() {
        push_segment(&mut segments, SegmentKind::Title, title, 1.20);
    }
    for heading in &headings {
        push_segment(&mut segments, SegmentKind::Heading, heading, 1.10);
    }
    push_segment(&mut segments, SegmentKind::Summary, &summary, 1.08);
    for sentence in &sentences {
        push_segment(&mut segments, SegmentKind::Sentence, sentence, 1.00);
    }
    for group in keywords.chunks(8) {
        push_segment(
            &mut segments,
            SegmentKind::Keyword,
            &group.join(", "),
            0.82,
        );
    }
    for group in entities.chunks(6) {
        push_segment(
            &mut segments,
            SegmentKind::Entity,
            &group.join("; "),
            0.94,
        );
    }
    if let Some(url_signal) = url_signal(canonical_url) {
        push_segment(
            &mut segments,
            SegmentKind::UrlSignal,
            &url_signal,
            0.62,
        );
    }

    dedupe_segments(&mut segments);
    segments.truncate(96);
    for (ordinal, segment) in segments.iter_mut().enumerate() {
        segment.ordinal = ordinal;
    }

    Ok(ExtractedPage {
        title,
        summary,
        visible_text,
        keywords,
        entities,
        segments,
        links,
    })
}

pub(crate) fn query_segments(query: &str) -> Result<QueryAnalysis, SemanticError> {
    let query = normalize_whitespace(query);
    if query.chars().count() < 3 || query.chars().count() > 2_000 {
        return Err(SemanticError::invalid(
            "query_text",
            "query_text must contain 3 to 2,000 characters",
        ));
    }
    let tokens = normalized_token_set(&query);
    let keywords = extract_keywords(&query, 20);
    let entities = extract_entities(&query, 20);
    let mut segments = vec![TextSegment {
        kind: SegmentKind::Query,
        text: query.clone(),
        weight: 1.20,
        ordinal: 0,
    }];
    if !keywords.is_empty() {
        segments.push(TextSegment {
            kind: SegmentKind::Keyword,
            text: keywords.join(", "),
            weight: 0.82,
            ordinal: segments.len(),
        });
    }
    if !entities.is_empty() {
        segments.push(TextSegment {
            kind: SegmentKind::Entity,
            text: entities.join("; "),
            weight: 0.96,
            ordinal: segments.len(),
        });
    }
    Ok(QueryAnalysis {
        text: query,
        tokens,
        keywords,
        entities,
        segments,
    })
}

#[derive(Debug, Clone)]
pub(crate) struct QueryAnalysis {
    pub text: String,
    pub tokens: BTreeSet<String>,
    pub keywords: Vec<String>,
    pub entities: Vec<String>,
    pub segments: Vec<TextSegment>,
}

pub(crate) fn normalized_token_set(text: &str) -> BTreeSet<String> {
    normalized_tokens(text)
        .into_iter()
        .filter(|token| !is_stopword(token))
        .collect()
}

fn push_segment(segments: &mut Vec<TextSegment>, kind: SegmentKind, text: &str, weight: f32) {
    let text = truncate_chars(&normalize_whitespace(text), 700);
    if text.chars().count() < 3 {
        return;
    }
    segments.push(TextSegment {
        kind,
        text,
        weight,
        ordinal: segments.len(),
    });
}

fn dedupe_segments(segments: &mut Vec<TextSegment>) {
    let mut seen = HashSet::new();
    segments.retain(|segment| seen.insert(segment.text.to_ascii_lowercase()));
}

