fn normalize_whitespace(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut previous_space = true;
    for character in input.chars() {
        if character.is_whitespace() {
            if !previous_space {
                output.push(' ');
                previous_space = true;
            }
        } else {
            output.push(character);
            previous_space = false;
        }
    }
    output.trim().to_owned()
}

fn complete_sentences(text: &str, limit: usize) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut buffer = String::new();
    for character in text.chars() {
        buffer.push(character);
        if matches!(character, '.' | '!' | '?') {
            let candidate = normalize_whitespace(&buffer);
            let words = candidate.split_whitespace().count();
            if (6..=110).contains(&words) && (36..=900).contains(&candidate.chars().count()) {
                sentences.push(candidate);
                if sentences.len() == limit {
                    break;
                }
            }
            buffer.clear();
        } else if buffer.chars().count() > 1_000 {
            buffer.clear();
        }
    }
    dedupe_text(sentences, limit)
}

fn build_summary(sentences: &[String], visible_text: &str) -> String {
    let summary = sentences.iter().take(3).cloned().collect::<Vec<_>>().join(" ");
    if summary.chars().count() >= 80 {
        truncate_chars(&summary, 900)
    } else {
        truncate_chars(visible_text, 900)
    }
}

fn normalized_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() || (character == '\'' && !current.is_empty()) {
            for lower in character.to_lowercase() {
                current.push(lower);
            }
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn extract_keywords(text: &str, limit: usize) -> Vec<String> {
    let mut counts = BTreeMap::<String, usize>::new();
    for token in normalized_tokens(text) {
        if token.chars().count() < 3 || is_stopword(&token) || token.chars().all(char::is_numeric)
        {
            continue;
        }
        *counts.entry(token).or_default() += 1;
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|(left_word, left_count), (right_word, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_word.cmp(right_word))
    });
    ranked
        .into_iter()
        .take(limit)
        .map(|(word, _)| word)
        .collect()
}

fn extract_entities(text: &str, limit: usize) -> Vec<String> {
    let mut counts = BTreeMap::<String, usize>::new();
    let mut phrase = Vec::new();

    let flush = |phrase: &mut Vec<String>, counts: &mut BTreeMap<String, usize>| {
        if phrase.is_empty() {
            return;
        }
        let candidate = phrase.join(" ");
        if candidate.chars().count() >= 3 && !is_common_sentence_lead(&candidate) {
            *counts.entry(candidate).or_default() += 1;
        }
        phrase.clear();
    };

    for raw in text.split_whitespace() {
        let token = raw.trim_matches(|character: char| {
            !character.is_alphanumeric() && character != '-' && character != '\''
        });
        if is_entity_token(token) {
            phrase.push(token.to_owned());
            if phrase.len() == 5 {
                flush(&mut phrase, &mut counts);
            }
        } else {
            flush(&mut phrase, &mut counts);
        }
    }
    flush(&mut phrase, &mut counts);

    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|(left_name, left_count), (right_name, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_name.cmp(right_name))
    });
    ranked
        .into_iter()
        .take(limit)
        .map(|(name, _)| name)
        .collect()
}

fn is_entity_token(token: &str) -> bool {
    if token.chars().count() < 2 || token.chars().all(char::is_numeric) {
        return false;
    }
    let mut characters = token.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !first.is_uppercase() {
        return false;
    }
    let letters: Vec<char> = token.chars().filter(|character| character.is_alphabetic()).collect();
    let uppercase = letters.iter().filter(|character| character.is_uppercase()).count();
    first.is_uppercase() && (uppercase == 1 || (uppercase == letters.len() && letters.len() <= 8))
}

fn is_common_sentence_lead(candidate: &str) -> bool {
    matches!(
        candidate,
        "A" | "An" | "And" | "But" | "For" | "How" | "In" | "It" | "On" | "Or"
            | "The" | "This" | "To" | "We" | "What" | "When" | "Where" | "Why" | "You"
    )
}

fn is_stopword(token: &str) -> bool {
    matches!(
        token,
        "a" | "about" | "after" | "again" | "all" | "also" | "am" | "an" | "and"
            | "any" | "are" | "as" | "at" | "be" | "because" | "been" | "before"
            | "being" | "between" | "both" | "but" | "by" | "can" | "could" | "did"
            | "do" | "does" | "doing" | "down" | "during" | "each" | "few" | "for"
            | "from" | "further" | "had" | "has" | "have" | "having" | "he" | "her"
            | "here" | "hers" | "herself" | "him" | "himself" | "his" | "how" | "i"
            | "if" | "in" | "into" | "is" | "it" | "its" | "itself" | "just" | "me"
            | "more" | "most" | "my" | "myself" | "no" | "nor" | "not" | "now" | "of"
            | "off" | "on" | "once" | "only" | "or" | "other" | "our" | "ours"
            | "ourselves" | "out" | "over" | "own" | "same" | "she" | "should" | "so"
            | "some" | "such" | "than" | "that" | "the" | "their" | "theirs" | "them"
            | "themselves" | "then" | "there" | "these" | "they" | "this" | "those"
            | "through" | "to" | "too" | "under" | "until" | "up" | "very" | "was"
            | "we" | "were" | "what" | "when" | "where" | "which" | "while" | "who"
            | "whom" | "why" | "will" | "with" | "would" | "you" | "your" | "yours"
            | "yourself" | "yourselves"
    )
}

