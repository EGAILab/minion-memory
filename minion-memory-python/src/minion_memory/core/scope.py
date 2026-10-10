"""MemoryScope value, equality and applicability matching (Layer 02: SCOPE-001..006; Owner
G-056..G-064).

A scope is `{"scope_state": "resolved", "facets": {...}}` or `{"scope_state": "unresolved"}`. Matching
is fail-closed: a resolved record is visible only when every facet it declares appears in the
allowed scope with the same value; unresolved records and records with unknown facets are hidden.
Arguments pass SER-008 stages 1 and 2 together first (G-031); `scope.invalid` and
`scope.query_invalid` are stage 3. Multi-argument primitives name the failing argument.
"""

from __future__ import annotations

import re
from collections.abc import Callable

from .errors import MnemeValidationError, describe
from .identity import _stable_id_rule
from .values import Value, validate_arguments

# G-064: two lowercase ASCII segments, each 1-128 bytes `[a-z0-9][a-z0-9_-]*`, one literal dot; no
# device-name exclusion (facet names are not paths).
_FACET_NAME = re.compile(r"[a-z0-9][a-z0-9_-]{0,127}\.[a-z0-9][a-z0-9_-]{0,127}", re.ASCII)

# G-061: the reviewed vocabularies of profiles that have adopted scope. Learning is deferred (G-062).
AGENT_FACETS = frozenset(
    {"agent.principal", "agent.agent", "agent.workspace", "agent.conversation", "agent.session", "agent.task"}
)
_REGISTRY = AGENT_FACETS


def _is_facet_value(value: Value) -> bool:
    """G-056: one CORE-001 portable token."""
    try:
        _stable_id_rule(value)
    except MnemeValidationError:
        return False
    return True


def _facet_map(value: Value, code: str) -> dict[str, str]:
    if not isinstance(value, dict):
        raise MnemeValidationError(code, f"facets are an object, not {describe(value)}")
    facets: dict[str, str] = {}
    for name, facet_value in value.items():
        if not _FACET_NAME.fullmatch(name):
            raise MnemeValidationError(code, f"{describe(name)} is not a <profile>.<facet> name (G-064)")
        if not _is_facet_value(facet_value):
            raise MnemeValidationError(code, f"facet {name} value is not a portable token (G-056)")
        assert isinstance(facet_value, str)
        facets[name] = facet_value
    return facets


def _scope_rule(value: Value) -> dict[str, Value]:
    """G-056: exactly the resolved or the unresolved closed record."""
    if not isinstance(value, dict):
        raise MnemeValidationError("scope.invalid", f"a scope is an object, not {describe(value)}")
    state = value.get("scope_state")
    if state == "unresolved" and set(value) == {"scope_state"}:
        return {"scope_state": "unresolved"}
    if state == "resolved" and set(value) == {"scope_state", "facets"}:
        return {"scope_state": "resolved", "facets": dict(_facet_map(value["facets"], "scope.invalid"))}
    raise MnemeValidationError(
        "scope.invalid", "a scope is {scope_state: resolved, facets} or {scope_state: unresolved}"
    )


def _query_rule(value: Value) -> dict[str, str]:
    """G-058, G-061: a resolved facet map naming only known facets; there is no unresolved query."""
    facets = _facet_map(value, "scope.query_invalid")
    unknown = sorted(set(facets) - _REGISTRY)
    if unknown:
        raise MnemeValidationError("scope.query_invalid", f"unknown facet {unknown[0]} (G-061)")
    return facets


def _named[T](values: dict[str, Value], name: str, rule: Callable[[Value], T]) -> T:
    try:
        return rule(values[name])
    except MnemeValidationError as error:
        raise error.for_argument(name) from None


def validate_scope(scope: object) -> dict[str, Value]:
    """Validate one MemoryScope value and return it normalized; unknown well-formed facets are kept."""
    return _scope_rule(validate_arguments({"scope": scope})["scope"])


def scopes_equal(a: object, b: object) -> bool:
    """G-056: exact structural equality; member order is irrelevant, absent differs from present."""
    values = validate_arguments({"a": a, "b": b})
    return _named(values, "a", _scope_rule) == _named(values, "b", _scope_rule)


def scope_visible(record: object, query: object) -> bool:
    """G-058 applicability matching, fail-closed (G-061, G-062)."""
    values = validate_arguments({"record": record, "query": query})
    scope = _named(values, "record", _scope_rule)
    allowed = _named(values, "query", _query_rule)
    if scope["scope_state"] == "unresolved":
        return False
    facets = scope["facets"]
    assert isinstance(facets, dict)
    if set(facets) - _REGISTRY:
        return False  # G-061 fail-closed; also implied, since validated queries hold only known facets
    return all(allowed.get(name) == value for name, value in facets.items())
