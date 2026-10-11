"""Layer 03 HistoryProjector and lineage: public-seam controls beyond the canonical corpus
(PROJ-001, HIST-006/007; Owner G-067..G-075)."""

from __future__ import annotations

import copy
import random
from typing import Any

import pytest

from minion_memory import core

READER = {"profile": "learning", "recognized": [{"record_type": "attempt", "schema_version": 1}]}


def rec(rid: str, **extra: object) -> dict[str, Any]:
    r: dict[str, Any] = {
        "record_id": rid,
        "profile": "learning",
        "record_type": "attempt",
        "schema_version": 1,
        "compatibility": "required",
        "recorded_at": "2026-10-11T08:00:00Z",
    }
    r.update(extra)
    return r


def stream(*records: dict[str, Any]) -> dict[str, Any]:
    return {"store_id": "s-1", "entries": [{"position": i + 1, "record": r} for i, r in enumerate(records)]}


def rel(a: str, kind: str, b: str) -> dict[str, Any]:
    return {"from_record_id": a, "relation": kind, "to_record_id": b}


def code(call: Any) -> tuple[str, str | None, int | None]:
    with pytest.raises(core.MnemeValidationError) as raised:
        call()
    return raised.value.code, raised.value.argument, raised.value.position


def test_inputs_are_never_mutated() -> None:
    s, r = stream(rec("a"), rec("b")), [rel("b", "supersedes", "a")]
    before = copy.deepcopy((s, r))
    core.project_history(READER, s, r)
    assert (s, r) == before


def test_relation_order_and_member_order_never_change_the_result() -> None:
    ids = [f"r-{i}" for i in range(12)]
    s = stream(*(rec(i) for i in ids))
    relations = [rel(ids[i + 1], "supersedes", ids[i]) for i in range(0, 10, 2)] + [
        rel(ids[i], "reinterprets", ids[0]) for i in range(2, 9)
    ]
    expected = core.project_history(READER, s, relations)
    rng = random.Random(7)
    for _ in range(25):
        shuffled = [dict(reversed(list(x.items()))) for x in rng.sample(relations, len(relations))]
        assert core.project_history(READER, s, shuffled) == expected


def test_long_supersession_chain_is_iterative() -> None:
    ids = [f"r-{i}" for i in range(3000)]
    result = core.project_history(
        READER, stream(*(rec(i) for i in ids)), [rel(ids[i + 1], "supersedes", ids[i]) for i in range(2999)]
    )
    assert result["current"] == [ids[-1]]


def test_long_cycle_is_detected() -> None:
    ids = [f"r-{i}" for i in range(500)]
    cycle = [rel(ids[i], "supersedes", ids[(i + 1) % 500]) for i in range(500)]
    assert code(lambda: core.project_history(READER, stream(*(rec(i) for i in ids)), cycle)) == (
        "lineage.invalid",
        "relations",
        None,
    )


@pytest.mark.parametrize("profile", ["learning", "agent"])
@pytest.mark.parametrize(
    ("record_type", "schema_version"),
    [("mystery", 4), ("attempt", 2)],
    ids=["unknown-type", "unknown-schema-of-known-type"],
)
@pytest.mark.parametrize("seam", ["native", "json-text"])
def test_unknown_required_carries_position_and_full_routing_identity(
    profile: str, record_type: str, schema_version: int, seam: str
) -> None:
    """WP03-CR-006: the detail names (profile, record_type, schema_version); profiles differ."""
    import json

    reader = {"profile": profile, "recognized": [{"record_type": "attempt", "schema_version": 1}]}
    s = {
        "store_id": "s-1",
        "entries": [
            {"position": 3, "record": rec("a", profile=profile)},
            {
                "position": 7,
                "record": rec("u", profile=profile, record_type=record_type, schema_version=schema_version),
            },
        ],
    }
    reader_arg: object = reader if seam == "native" else core.JsonText(json.dumps(reader))
    stream_arg: object = s if seam == "native" else core.JsonText(json.dumps(s))
    with pytest.raises(core.MnemeValidationError) as raised:
        core.project_history(reader_arg, stream_arg, [])
    error = raised.value
    assert (error.code, error.argument, error.position) == ("history.unknown_required", None, 7)
    assert f"({profile}, {record_type}, {schema_version})" in error.detail


def test_early_lineage_errors_still_precede_unknown_required() -> None:
    bad = [{"from_record_id": "a", "relation": "replaces", "to_record_id": "u"}]
    assert code(lambda: core.project_history(READER, stream(rec("u", record_type="mystery")), bad)) == (
        "lineage.invalid",
        "relations",
        None,
    )


def test_json_text_seams() -> None:
    s = core.JsonText(
        '{"store_id":"s-1","entries":[{"position":1,"record":{"record_id":"a","profile":"learning",'
        '"record_type":"attempt","schema_version":1,"compatibility":"required","recorded_at":"2026-10-11T08:00:00Z"}}]}'
    )
    result = core.project_history(
        core.JsonText('{"profile":"learning","recognized":[{"record_type":"attempt","schema_version":1}]}'),
        s,
        core.JsonText("[]"),
    )
    assert result["current"] == ["a"] and result["through"] == {"store_id": "s-1", "position": 1}


def test_generic_stage_one_in_relations_outranks_reader_stage_three() -> None:
    assert code(
        lambda: core.project_history({"profile": "X"}, stream(), core.JsonText('[{"a":1,"a":2}]'))
    ) == ("value.duplicate_key", "relations", None)


@pytest.mark.parametrize(
    ("relations", "argument"),
    [
        ("x", "relations"),
        ([1], "relations"),
        ([{"relation": "supersedes", "to_record_id": "a"}], "relations"),
        ([{"from_record_id": "a", "relation": "supersedes"}], "relations"),
        ([{"from_record_id": "a", "relation": "derived_from", "to_artifact": "ku-1"}], "relations"),
        (
            [
                {
                    "from_record_id": "a",
                    "relation": "derived_from",
                    "to_artifact": {"stable_id": "K", "artifact_version": 1},
                }
            ],
            "relations",
        ),
        ([{"from_record_id": "a", "relation": "supersedes", "to_record_id": "B"}], "relations"),
    ],
)
def test_early_relation_shapes(relations: object, argument: str) -> None:
    assert code(lambda: core.project_history(READER, stream(rec("a")), relations)) == (
        "lineage.invalid",
        argument,
        None,
    )


def test_stream_shapes() -> None:
    for bad in [
        "s",
        {"store_id": "s-1", "entries": {}},
        {"store_id": "s-1", "entries": [1]},
        {"store_id": "s-1", "entries": [{"position": True, "record": rec("a")}]},
        {"store_id": "s-1", "entries": [{"position": 1.5, "record": rec("a")}]},
    ]:
        assert code(lambda bad=bad: core.project_history(READER, bad, [])) == (
            "history.stream_invalid",
            "stream",
            None,
        )


def test_presentation_order_of_relation_targets() -> None:
    s = stream(rec("a"), rec("b"))
    h = "0" * 64
    relations = [
        {"from_record_id": "b", "relation": "derived_from", "to_artifact": {"content_hash": h}},
        {
            "from_record_id": "b",
            "relation": "derived_from",
            "to_artifact": {"stable_id": "ku-1", "artifact_version": 2},
        },
        rel("b", "derived_from", "zz-external"),
        rel("b", "derived_from", "a"),
    ]
    result: Any = core.project_history(READER, s, relations)
    targets = [r.get("to_record_id", r.get("to_artifact")) for r in result["relations"]]
    assert targets == ["a", "zz-external", {"stable_id": "ku-1", "artifact_version": 2}, {"content_hash": h}]
    assert len(result["unresolved_derivations"]) == 3
    assert result["current"] == ["a", "b"]


def test_interpretations_list_observations_in_position_order() -> None:
    s = stream(rec("o-1"), rec("o-2"), rec("e-1"), rec("e-2"))
    result = core.project_history(
        READER, s, [rel("e-2", "reinterprets", "o-1"), rel("e-1", "reinterprets", "o-2")]
    )
    assert result["interpretations"] == [
        {"observation": "o-1", "current": ["e-2"]},
        {"observation": "o-2", "current": ["e-1"]},
    ]


def test_artifact_target_on_a_resolution_kind_is_an_early_shape_error() -> None:
    """G-072 restricts to_artifact to derived_from as an early check, so it precedes entry errors.
    (A late "unavailable endpoint" reading would report the entry's unknown_required instead.)"""
    bad = [
        {
            "from_record_id": "a",
            "relation": "supersedes",
            "to_artifact": {"stable_id": "ku-7", "artifact_version": 3},
        }
    ]
    unknown = stream(rec("u", record_type="mystery"))
    assert code(lambda: core.project_history(READER, unknown, bad)) == ("lineage.invalid", "relations", None)
