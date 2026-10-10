"""Layer 02 history envelope, replay admission and HistoryBoundary: public-seam controls beyond the
canonical corpus (HIST-002, HIST-005, SER-005, SER-006; Owner G-048..G-055)."""

from __future__ import annotations

import dataclasses
from typing import Any

import pytest

from minion_memory import core

READER: dict[str, Any] = {
    "profile": "learning",
    "recognized": [{"record_type": "attempt", "schema_version": 1}],
}
RECORD: dict[str, Any] = {
    "record_id": "ev-1",
    "profile": "learning",
    "record_type": "attempt",
    "schema_version": 1,
    "compatibility": "required",
    "recorded_at": "2026-10-11T08:00:00Z",
}


def code(call: Any) -> tuple[str, str | None]:
    with pytest.raises(core.MnemeValidationError) as raised:
        call()
    return raised.value.code, raised.value.argument


def admit(record: object, reader: object = READER) -> core.HistoryAdmission:
    return core.admit_history_record(reader, record)


def test_decode_returns_a_new_normalized_envelope_and_leaves_the_input_untouched() -> None:
    record = dict(RECORD, payload={"n": [1.0, {"k": "v"}]})
    admission = admit(record)
    assert admission.outcome == "decode" and admission.envelope is not None
    assert admission.envelope["recorded_at"] == "2026-10-11T08:00:00.000000Z"
    assert admission.envelope["payload"] == {"n": [1, {"k": "v"}]}
    payload: Any = admission.envelope["payload"]
    assert type(payload["n"][0]) is int
    assert record["recorded_at"] == "2026-10-11T08:00:00Z"  # caller value is not mutated
    with pytest.raises(dataclasses.FrozenInstanceError):
        admission.outcome = "skip"  # type: ignore[misc]


def test_json_text_record_is_accepted_at_the_seam() -> None:
    text = (
        '{"record_id":"ev-1","profile":"learning","record_type":"attempt","schema_version":1,'
        '"compatibility":"required","recorded_at":"2026-10-11T08:00:00Z"}'
    )
    assert admit(core.JsonText(text)).outcome == "decode"


def test_skip_and_excluded_carry_no_envelope() -> None:
    assert admit(dict(RECORD, record_type="ping", compatibility="ignorable")) == core.HistoryAdmission("skip")
    assert admit(dict(RECORD, profile="agent")) == core.HistoryAdmission("excluded")


def test_unknown_required_names_no_argument() -> None:
    assert code(lambda: admit(dict(RECORD, record_type="correction"))) == ("history.unknown_required", None)


@pytest.mark.parametrize(
    "change",
    [
        {"schema_version": True},
        {"schema_version": 9007199254740991 + 0.5},
        {"profile": 7},
        {"record_type": "con"},
        {"compatibility": ["required"]},
        {"compatibility": {"x": 1}},
    ],
    ids=[
        "bool-version",
        "huge-fraction",
        "numeric-profile",
        "device-name-type",
        "list-compat",
        "object-compat",
    ],
)
def test_identity_and_compatibility_domains(change: dict[str, Any]) -> None:
    observed = code(lambda: admit(dict(RECORD, **change)))
    if "schema_version" in change and change["schema_version"] is not True:
        assert observed == ("value.integer_out_of_range", "record")  # rounds to 2^53: generic stage 2
    else:
        assert observed == ("history.envelope_invalid", "record")


def test_largest_schema_edition_is_recognized_when_declared() -> None:
    big = 9007199254740991
    reader = {"profile": "learning", "recognized": [{"record_type": "attempt", "schema_version": big}]}
    assert admit(dict(RECORD, schema_version=big), reader).outcome == "decode"


@pytest.mark.parametrize(
    "reader",
    [
        [],
        {"profile": "learning", "recognized": {}},
        {"profile": "learning", "recognized": ["attempt"]},
        {"profile": "learning", "recognized": [{"record_type": "Attempt", "schema_version": 1}]},
        {"profile": "learning", "recognized": [{"record_type": "attempt", "schema_version": True}]},
    ],
    ids=["array", "object-recognized", "string-entry", "bad-type", "bool-version"],
)
def test_reader_shape(reader: object) -> None:
    assert code(lambda: admit(RECORD, reader)) == ("history.reader_invalid", "reader")


def test_empty_recognized_reader_skips_or_stops() -> None:
    reader = {"profile": "learning", "recognized": []}
    assert admit(dict(RECORD, compatibility="ignorable"), reader).outcome == "skip"
    assert code(lambda: admit(RECORD, reader)) == ("history.unknown_required", None)


def test_generic_stage_one_in_record_outranks_reader_stage_three() -> None:
    bad_reader = {"profile": "Learning", "recognized": []}
    text = core.JsonText('{"profile":"learning","profile":"agent"}')
    assert code(lambda: admit(text, bad_reader)) == ("value.duplicate_key", "record")


@pytest.mark.parametrize(
    ("a", "b", "expected"), [(0, 0, "equal"), (0, 9007199254740991, "less"), (7, 3, "greater")]
)
def test_boundary_order(a: int, b: int, expected: str) -> None:
    assert (
        core.compare_history_boundaries({"store_id": "s", "position": a}, {"store_id": "s", "position": b})
        == expected
    )


@pytest.mark.parametrize("position", [True, None, [1], {"p": 1}], ids=["bool", "null", "array", "object"])
def test_boundary_position_domain(position: object) -> None:
    assert code(
        lambda: core.history_boundaries_equal(
            {"store_id": "s", "position": 1}, {"store_id": "s", "position": position}
        )
    ) == ("history.boundary_invalid", "b")


def test_boundary_non_object_and_store_id() -> None:
    assert code(lambda: core.compare_history_boundaries("s:1", {"store_id": "s", "position": 1})) == (
        "history.boundary_invalid",
        "a",
    )
    assert code(
        lambda: core.compare_history_boundaries(
            {"store_id": "con", "position": 1}, {"store_id": "s", "position": 1}
        )
    ) == (
        "history.boundary_invalid",
        "a",
    )


def test_cross_store_incomparability_names_no_argument() -> None:
    assert code(
        lambda: core.compare_history_boundaries(
            {"store_id": "s", "position": 1}, {"store_id": "t", "position": 1}
        )
    ) == ("history.boundary_incomparable", None)
    assert (
        core.history_boundaries_equal({"store_id": "s", "position": 1}, {"store_id": "t", "position": 1})
        is False
    )


def test_boundary_json_text_seam() -> None:
    a = core.JsonText('{"store_id":"s","position":2.0}')
    assert core.compare_history_boundaries(a, {"store_id": "s", "position": 2}) == "equal"


# ------------------------------------------------- WP02-CR-006: gate 4 order is canonical, not source
# JSON-text decoding keeps source member order, so gate 4 must sort (G-034, G-055). Every source
# order of the typed fields, with every combination of two or three invalid ones, must report the
# first invalid field in canonical order: occurred_at, then record_id, then recorded_at.

_TYPED_ORDER = ("occurred_at", "record_id", "recorded_at")
_BAD: dict[str, tuple[str, Any]] = {
    "occurred_at": ("timestamp.invalid", None),
    "record_id": ("id.invalid", "BAD"),
    "recorded_at": ("timestamp.invalid", "now"),
}
_GOOD = {"occurred_at": "2026-10-11T07:00:00Z", "record_id": "ev-1", "recorded_at": "2026-10-11T08:00:00Z"}
_ROUTING = {"profile": "learning", "record_type": "attempt", "schema_version": 1, "compatibility": "required"}


def _cases() -> list[tuple[tuple[str, ...], frozenset[str], str]]:
    from itertools import combinations, permutations

    cases = []
    for order in permutations(_TYPED_ORDER):
        for size in (2, 3):
            for bad in combinations(_TYPED_ORDER, size):
                first = next(name for name in _TYPED_ORDER if name in bad)
                cases.append((order, frozenset(bad), _BAD[first][0]))
    return cases


@pytest.mark.parametrize("seam", ["json-text", "native"])
@pytest.mark.parametrize(
    ("order", "bad", "expected"),
    _cases(),
    ids=lambda v: "-".join(sorted(v)) if isinstance(v, frozenset) else str(v),
)
def test_gate4_reports_the_first_invalid_field_in_canonical_order(
    seam: str, order: tuple[str, ...], bad: frozenset[str], expected: str
) -> None:
    import json

    members: list[tuple[str, Any]] = [(name, _BAD[name][1] if name in bad else _GOOD[name]) for name in order]
    members[1:1] = list(_ROUTING.items())  # routing members between typed ones: source order is mixed
    if seam == "json-text":
        text = "{" + ",".join(f"{json.dumps(k)}:{json.dumps(v)}" for k, v in members) + "}"
        record: object = core.JsonText(text)
    else:
        record = dict(members)
    assert code(lambda: admit(record)) == (expected, "record")


def test_codex_witness_json_text_null_occurred_at_after_invalid_record_id() -> None:
    text = (
        '{"record_id":"BAD","profile":"learning","record_type":"attempt","schema_version":1,'
        '"compatibility":"required","recorded_at":"2026-10-11T08:00:00Z","occurred_at":null}'
    )
    assert code(lambda: admit(core.JsonText(text))) == ("timestamp.invalid", "record")
