use std::{collections::BTreeSet, fmt};

use uuid::Uuid;

pub const MAX_SOURCE_FILTERS: usize = 100;
const SOURCE_PREFIX: &str = "source:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceScopeError {
    TooManyRuleFilters,
    InvalidRuleFilter,
    RequestedSourceOutsideRuleScope,
}

impl fmt::Display for SourceScopeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooManyRuleFilters => "source_filters must contain at most 100 source IDs",
            Self::InvalidRuleFilter => {
                "each source_filters value must be a UUID or source:<UUID>"
            }
            Self::RequestedSourceOutsideRuleScope => {
                "requested source_ids must be a subset of the alert rule source_filters"
            }
        })
    }
}

impl std::error::Error for SourceScopeError {}

pub fn normalize_source_filters(values: &mut Vec<String>) -> Result<(), SourceScopeError> {
    let ids = parse_source_filter_ids(values)?;
    *values = ids
        .into_iter()
        .map(|source_id| format!("{SOURCE_PREFIX}{source_id}"))
        .collect();
    Ok(())
}

pub fn validate_source_filters(values: &[String]) -> Result<(), SourceScopeError> {
    parse_source_filter_ids(values).map(|_| ())
}

pub fn parse_source_filter_ids(values: &[String]) -> Result<Vec<Uuid>, SourceScopeError> {
    if values.len() > MAX_SOURCE_FILTERS {
        return Err(SourceScopeError::TooManyRuleFilters);
    }

    values
        .iter()
        .map(|value| parse_source_filter(value))
        .collect::<Result<BTreeSet<_>, _>>()
        .map(BTreeSet::into_iter)
        .map(Iterator::collect)
}

pub fn constrain_search_sources(
    rule_filters: &[String],
    requested_source_ids: &[Uuid],
) -> Result<Vec<Uuid>, SourceScopeError> {
    let rule_scope = parse_source_filter_ids(rule_filters)?;
    let requested = requested_source_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();

    if rule_scope.is_empty() {
        return Ok(requested.into_iter().collect());
    }

    let rule_scope = rule_scope.into_iter().collect::<BTreeSet<_>>();
    if requested.is_empty() {
        return Ok(rule_scope.into_iter().collect());
    }

    if requested.iter().any(|source_id| !rule_scope.contains(source_id)) {
        return Err(SourceScopeError::RequestedSourceOutsideRuleScope);
    }

    Ok(requested.into_iter().collect())
}

fn parse_source_filter(value: &str) -> Result<Uuid, SourceScopeError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(SourceScopeError::InvalidRuleFilter);
    }
    let uuid = value.strip_prefix(SOURCE_PREFIX).unwrap_or(value);
    Uuid::parse_str(uuid).map_err(|_| SourceScopeError::InvalidRuleFilter)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }

    #[test]
    fn normalizes_bare_and_prefixed_ids_to_one_canonical_form() {
        let source = id(1);
        let mut filters = vec![
            format!(" {source} "),
            format!("source:{source}"),
            format!("source:{}", id(2)),
        ];

        normalize_source_filters(&mut filters).expect("valid source scope");

        assert_eq!(
            filters,
            [format!("source:{source}"), format!("source:{}", id(2))]
        );
    }

    #[test]
    fn rule_scope_is_an_upper_bound_on_request_scope() {
        let first = id(1);
        let second = id(2);
        let filters = vec![format!("source:{first}"), format!("source:{second}")];

        assert_eq!(
            constrain_search_sources(&filters, &[]).expect("rule default scope"),
            [first, second]
        );
        assert_eq!(
            constrain_search_sources(&filters, &[second]).expect("subset scope"),
            [second]
        );
        assert_eq!(
            constrain_search_sources(&filters, &[id(3)]),
            Err(SourceScopeError::RequestedSourceOutsideRuleScope)
        );
    }

    #[test]
    fn empty_rule_scope_preserves_a_bounded_request_scope() {
        assert_eq!(
            constrain_search_sources(&[], &[id(2), id(1), id(2)])
                .expect("unrestricted rule"),
            [id(1), id(2)]
        );
    }

    #[test]
    fn malformed_filters_fail_closed_without_echoing_input() {
        let error = parse_source_filter_ids(&["not-a-source-secret".into()])
            .expect_err("malformed filter must fail");
        assert_eq!(error, SourceScopeError::InvalidRuleFilter);
        assert!(!error.to_string().contains("not-a-source-secret"));
    }
}
