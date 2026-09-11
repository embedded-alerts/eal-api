#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use eal_api::source_scope::{SourceScopeError, constrain_search_sources, normalize_source_filters};
use uuid::Uuid;

fn source(value: u128) -> Uuid {
    Uuid::from_u128(value + 1)
}

fn scope(mask: u8) -> Vec<Uuid> {
    (0..3)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(source)
        .collect()
}

fn filters(mask: u8) -> Vec<String> {
    scope(mask)
        .into_iter()
        .map(|source_id| format!("source:{source_id}"))
        .collect()
}

#[test]
fn production_source_scope_refines_the_complete_three_source_model() {
    let mut explored = 0_u16;
    let universe = scope(0b111).into_iter().collect::<BTreeSet<_>>();

    for rule_mask in 0_u8..8 {
        for request_mask in 0_u8..8 {
            explored += 1;
            let rule = filters(rule_mask);
            let request = scope(request_mask);
            let result = constrain_search_sources(&rule, &request);
            let request_set = request.iter().copied().collect::<BTreeSet<_>>();
            let rule_set = scope(rule_mask).into_iter().collect::<BTreeSet<_>>();

            if rule_set.is_empty() {
                assert_eq!(
                    result.expect("an unrestricted rule must preserve request scope"),
                    request
                );
                continue;
            }
            if request_set.is_empty() {
                assert_eq!(
                    result.expect("an omitted request scope must inherit rule scope"),
                    rule_set.iter().copied().collect::<Vec<_>>()
                );
                continue;
            }
            if request_set.is_subset(&rule_set) {
                let effective = result.expect("a request subset must be accepted");
                let effective = effective.into_iter().collect::<BTreeSet<_>>();
                assert_eq!(effective, request_set);
                assert!(effective.is_subset(&rule_set));
                assert!(effective.is_subset(&universe));
            } else {
                assert_eq!(
                    result,
                    Err(SourceScopeError::RequestedSourceOutsideRuleScope)
                );
            }
        }
    }

    assert_eq!(explored, 64);
}

#[test]
fn canonicalization_is_idempotent_and_deduplicating() {
    let first = source(0);
    let second = source(1);
    let mut values = vec![
        first.to_string(),
        format!("source:{second}"),
        format!(" source:{first} "),
    ];

    normalize_source_filters(&mut values).expect("first normalization");
    let once = values.clone();
    normalize_source_filters(&mut values).expect("second normalization");

    assert_eq!(values, once);
    assert_eq!(values.len(), 2);
}
