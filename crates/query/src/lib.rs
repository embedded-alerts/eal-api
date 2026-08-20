use eal_semantic_contracts::{
    EmbeddingInput, EmbeddingInputKind, MAX_EMBEDDING_INPUT_CHARS, MAX_EMBEDDING_INPUTS,
    QueryTextViews,
};
use std::{collections::BTreeSet, error::Error, fmt};

const MAX_KEYWORDS: usize = 24;
const MAX_ENTITIES: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryAnalysisError {
    pub code: &'static str,
    pub message: String,
}

impl QueryAnalysisError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for QueryAnalysisError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for QueryAnalysisError {}

pub fn analyze_query(value: &str) -> Result<QueryTextViews, QueryAnalysisError> {
    let query_text = collapse_whitespace(value);
    let character_count = query_text.chars().count();
    if !(3..=MAX_EMBEDDING_INPUT_CHARS).contains(&character_count) {
        return Err(QueryAnalysisError::new(
            "invalid_query_text",
            format!(
                "query text must contain 3 to {MAX_EMBEDDING_INPUT_CHARS} characters so the complete query remains one auditable embedding input"
            ),
        ));
    }

    let keywords = extract_keywords(&query_text);
    let entities = extract_entities(&query_text);
    let mut embedding_inputs = Vec::with_capacity(1 + keywords.len() + entities.len());
    push_input(
        &mut embedding_inputs,
        EmbeddingInputKind::Query,
        query_text.clone(),
        1.2,
    )?;
    for keyword in &keywords {
        push_input(
            &mut embedding_inputs,
            EmbeddingInputKind::Keyword,
            keyword.clone(),
            0.6,
        )?;
    }
    for entity in &entities {
        push_input(
            &mut embedding_inputs,
            EmbeddingInputKind::Entity,
            entity.clone(),
            0.9,
        )?;
    }

    let views = QueryTextViews {
        query_text,
        keywords,
        entities,
        embedding_inputs,
    };
    views
        .validate()
        .map_err(|error| QueryAnalysisError::new(error.code, error.message))?;
    Ok(views)
}

fn push_input(
    inputs: &mut Vec<EmbeddingInput>,
    kind: EmbeddingInputKind,
    text: String,
    weight: f32,
) -> Result<(), QueryAnalysisError> {
    if inputs.len() >= MAX_EMBEDDING_INPUTS {
        return Err(QueryAnalysisError::new(
            "too_many_embedding_inputs",
            format!("query analysis exceeded {MAX_EMBEDDING_INPUTS} embedding inputs"),
        ));
    }
    let ordinal = u16::try_from(inputs.len()).map_err(|_| {
        QueryAnalysisError::new(
            "too_many_embedding_inputs",
            "query analysis produced an unsupported number of inputs",
        )
    })?;
    inputs.push(EmbeddingInput {
        kind,
        ordinal,
        text,
        weight,
    });
    Ok(())
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn extract_keywords(query: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut output = Vec::new();
    for token in tokens(query) {
        let normalized = token.to_ascii_lowercase();
        if normalized.chars().count() < 3
            || is_stopword(&normalized)
            || !seen.insert(normalized.clone())
        {
            continue;
        }
        output.push(normalized);
        if output.len() >= MAX_KEYWORDS {
            break;
        }
    }
    output
}

fn extract_entities(query: &str) -> Vec<String> {
    let words = raw_tokens(query);
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    let mut current = Vec::new();

    for (index, word) in words.iter().enumerate() {
        let normalized = word.to_ascii_lowercase();
        let candidate =
            is_probable_entity_word(word) && !(index == 0 && is_instruction_word(&normalized));
        if candidate {
            current.push(word.clone());
        } else {
            flush_entity(&mut current, &mut seen, &mut output);
        }
        if output.len() >= MAX_ENTITIES {
            break;
        }
    }
    flush_entity(&mut current, &mut seen, &mut output);
    output.truncate(MAX_ENTITIES);
    output
}

fn flush_entity(current: &mut Vec<String>, seen: &mut BTreeSet<String>, output: &mut Vec<String>) {
    if current.is_empty() || output.len() >= MAX_ENTITIES {
        current.clear();
        return;
    }
    let entity = current.join(" ");
    let identity = entity.to_ascii_lowercase();
    if seen.insert(identity) {
        output.push(entity);
    }
    current.clear();
}

fn tokens(value: &str) -> impl Iterator<Item = String> + '_ {
    value
        .split_whitespace()
        .map(clean_token)
        .filter(|token| !token.is_empty())
}

fn raw_tokens(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .map(clean_token)
        .filter(|token| !token.is_empty())
        .collect()
}

fn clean_token(value: &str) -> String {
    value
        .trim_matches(|character: char| {
            !character.is_alphanumeric() && character != '-' && character != '_'
        })
        .to_owned()
}

fn is_probable_entity_word(value: &str) -> bool {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    first.is_uppercase()
        && characters.any(char::is_lowercase)
        && !value.chars().all(char::is_uppercase)
}

fn is_instruction_word(value: &str) -> bool {
    matches!(
        value,
        "alert" | "email" | "find" | "let" | "notify" | "show" | "tell" | "track" | "watch"
    )
}

fn is_stopword(value: &str) -> bool {
    matches!(
        value,
        "about"
            | "after"
            | "again"
            | "against"
            | "alert"
            | "also"
            | "among"
            | "and"
            | "are"
            | "because"
            | "been"
            | "before"
            | "being"
            | "between"
            | "could"
            | "email"
            | "find"
            | "for"
            | "from"
            | "have"
            | "into"
            | "just"
            | "more"
            | "notify"
            | "should"
            | "that"
            | "the"
            | "their"
            | "them"
            | "then"
            | "there"
            | "these"
            | "they"
            | "this"
            | "those"
            | "through"
            | "tools"
            | "when"
            | "where"
            | "which"
            | "while"
            | "with"
            | "would"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_complete_query_and_companion_views() {
        let query = "Notify me when Acme Corporation launches renewable energy tools in Colombia.";
        let views = analyze_query(query).unwrap();
        assert_eq!(views.embedding_inputs[0].kind, EmbeddingInputKind::Query);
        assert_eq!(views.embedding_inputs[0].text, query);
        assert!(views.keywords.contains(&"renewable".into()));
        assert!(views.keywords.contains(&"energy".into()));
        assert!(views.entities.contains(&"Acme Corporation".into()));
        assert!(views.entities.contains(&"Colombia".into()));
        for (ordinal, input) in views.embedding_inputs.iter().enumerate() {
            assert_eq!(usize::from(input.ordinal), ordinal);
        }
    }

    #[test]
    fn analysis_is_deterministic_and_deduplicated() {
        let first = analyze_query("Track Rust rust Rust releases from Acme Labs").unwrap();
        let second = analyze_query("Track Rust rust Rust releases from Acme Labs").unwrap();
        assert_eq!(first, second);
        assert_eq!(
            first
                .keywords
                .iter()
                .filter(|keyword| keyword.as_str() == "rust")
                .count(),
            1
        );
    }

    #[test]
    fn rejects_queries_that_cannot_remain_one_complete_input() {
        assert!(analyze_query("  ").is_err());
        assert!(analyze_query(&"x".repeat(MAX_EMBEDDING_INPUT_CHARS + 1)).is_err());
    }
}
