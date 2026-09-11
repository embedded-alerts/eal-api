#!/usr/bin/env python3
"""Exhaustive finite model for alert-rule source scoping.

The production refinement is exercised independently in
`tests/source_filter_scope_contract.rs`. This model states the abstract policy
without importing application code so the two can disagree loudly.
"""

from __future__ import annotations

from itertools import product

UNIVERSE = frozenset(range(3))


def members(mask: int) -> frozenset[int]:
    return frozenset(index for index in UNIVERSE if mask & (1 << index))


def constrain(
    rule_scope: frozenset[int], request_scope: frozenset[int]
) -> tuple[bool, frozenset[int]]:
    if not rule_scope:
        return True, request_scope
    if not request_scope:
        return True, rule_scope
    if not request_scope.issubset(rule_scope):
        return False, frozenset()
    return True, request_scope


def check() -> tuple[int, int]:
    states = 0
    accepted = 0
    for rule_mask, request_mask in product(range(1 << len(UNIVERSE)), repeat=2):
        states += 1
        rule_scope = members(rule_mask)
        request_scope = members(request_mask)
        ok, effective = constrain(rule_scope, request_scope)

        if not rule_scope:
            assert ok
            assert effective == request_scope
        elif not request_scope:
            assert ok
            assert effective == rule_scope
        elif request_scope.issubset(rule_scope):
            assert ok
            assert effective == request_scope
        else:
            assert not ok
            assert not effective

        if ok:
            accepted += 1
            assert effective.issubset(UNIVERSE)
            if rule_scope:
                assert effective.issubset(rule_scope)
            if request_scope:
                assert effective.issubset(request_scope)

    return states, accepted


if __name__ == "__main__":
    state_count, accepted_count = check()
    print(
        f"source-scope model: {state_count} states, "
        f"{accepted_count} accepted; all invariants hold"
    )
