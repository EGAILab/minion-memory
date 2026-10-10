"""Layer 02 MemoryScope: public-seam controls beyond the canonical corpus (SCOPE-001..006; Owner
G-056..G-064)."""

from __future__ import annotations

from typing import Any

import pytest

from minion_memory import core


def resolved(**facets: str) -> dict[str, Any]:
    return {"scope_state": "resolved", "facets": {f"agent.{k}": v for k, v in facets.items()}}


def code(call: Any) -> tuple[str, str | None]:
    with pytest.raises(core.MnemeValidationError) as raised:
        call()
    return raised.value.code, raised.value.argument


def test_agent_registry_is_exactly_the_six_canonical_facets() -> None:
    assert {
        f"agent.{n}" for n in ("principal", "agent", "workspace", "conversation", "session", "task")
    } == core.AGENT_FACETS


@pytest.mark.parametrize(
    "name",
    ["a" * 128 + "." + "b" * 128, "0.9", "agent.con", "con.prn", "a-b.c_d", "agent.session-2"],
    ids=["two-128-byte-segments", "digits", "device-facet", "device-both", "punctuation", "dash"],
)
def test_well_formed_facet_names_are_preserved(name: str) -> None:
    scope = {"scope_state": "resolved", "facets": {name: "v-1"}}
    assert core.validate_scope(scope) == scope


@pytest.mark.parametrize(
    "name",
    [
        "a" * 129 + ".b",
        "a." + "b" * 129,
        "agent.",
        ".session",
        "agent..session",
        "-agent.session",
        "agent._x",
        "agent.Session",
        "agent session",
        "agent:session",
    ],
)
def test_malformed_facet_names_are_invalid_with_no_argument(name: str) -> None:
    assert code(lambda: core.validate_scope({"scope_state": "resolved", "facets": {name: "v-1"}})) == (
        "scope.invalid",
        None,
    )


@pytest.mark.parametrize(
    "value", ["con", "v.", "V-1", "", "a" * 129], ids=["device", "trailing-dot", "upper", "empty", "too-long"]
)
def test_facet_values_use_the_stable_id_rule(value: str) -> None:
    assert code(lambda: core.validate_scope(resolved(session=value))) == ("scope.invalid", None)


def test_validate_scope_returns_a_new_value() -> None:
    scope = resolved(principal="u-1")
    result = core.validate_scope(scope)
    assert result == scope and result is not scope and result["facets"] is not scope["facets"]


def test_json_text_seam() -> None:
    text = core.JsonText('{"facets":{"agent.session":"s-1"},"scope_state":"resolved"}')
    assert core.validate_scope(text) == resolved(session="s-1")
    assert core.scope_visible(text, core.JsonText('{"agent.session":"s-1"}')) is True


def test_unknown_query_facet_is_rejected_even_for_hidden_records() -> None:
    assert code(lambda: core.scope_visible({"scope_state": "unresolved"}, {"agent.mood": "x"})) == (
        "scope.query_invalid",
        "query",
    )
    unknown_record = {"scope_state": "resolved", "facets": {"agent.mood": "x"}}
    assert code(lambda: core.scope_visible(unknown_record, {"agent.mood": "x"})) == (
        "scope.query_invalid",
        "query",
    )


def test_query_shapes() -> None:
    assert code(lambda: core.scope_visible(resolved(), ["agent.session"])) == ("scope.query_invalid", "query")
    assert code(lambda: core.scope_visible(resolved(), {"agent.session": 1})) == (
        "scope.query_invalid",
        "query",
    )


def test_scopes_equal_names_a_then_b() -> None:
    assert code(lambda: core.scopes_equal({"scope_state": "x"}, {"scope_state": "y"})) == (
        "scope.invalid",
        "a",
    )
    assert code(lambda: core.scopes_equal(resolved(), {"scope_state": "unresolved", "facets": {}})) == (
        "scope.invalid",
        "b",
    )


def test_generic_stage_precedes_scope_rules_across_arguments() -> None:
    assert code(lambda: core.scope_visible({"scope_state": "maybe"}, core.JsonText('{"a":1,"a":2}'))) == (
        "value.duplicate_key",
        "query",
    )


def test_global_record_and_global_query() -> None:
    assert core.scope_visible(resolved(), {}) is True
    assert core.scope_visible(resolved(task="t-1"), {}) is False
