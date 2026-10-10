"""Controls for Codex implementation review 716d7218 (WP01-PY-001..007) and Owner G-035/G-036.

Each control reproduces a review witness against the agreed contract; they were run against the
reviewed candidate f10c7b4f first and failed there before any fix.
"""

from __future__ import annotations

import hashlib
import math
from typing import Any

import pytest

from minion_memory import core


def failure(call: Any, *args: Any) -> tuple[str, str | None]:
    with pytest.raises(core.MnemeValidationError) as raised:
        call(*args)
    return raised.value.code, raised.value.argument


# ------------------------------------------------- PY-001: each stage across all arguments first


def test_py001_stage1_in_later_argument_beats_stage2_in_earlier_native() -> None:
    assert failure(core.compare_versions, math.inf, b"x") == ("value.unsupported_type", "b")
    assert failure(core.compare_versions, 2**53, {1: "x"}) == ("value.unsupported_type", "b")


def test_py001_stage1_in_later_argument_beats_stage2_in_earlier_json() -> None:
    a, b = core.JsonText("1e400"), core.JsonText('{"a":1,"a":2}')
    assert failure(core.compare_operation_identities, a, b) == ("value.duplicate_key", "b")


def test_py001_mixed_json_and_native_arguments() -> None:
    assert failure(core.compare_versions, core.JsonText("1e400"), b"x") == ("value.unsupported_type", "b")
    assert core.compare_versions(core.JsonText("2.0"), 2) == "equal"


# ------------------------------------------------- PY-002 and G-035: revision record shape and order


VALID = {"stable_id": "ku-1", "artifact_version": 1, "projection": {"t": "x"}}


@pytest.mark.parametrize(
    "record",
    [
        {"stable_id": "BAD", "artifact_version": 0, "projection": None},
        {"projection": None, "artifact_version": 0, "stable_id": "BAD"},
        {"artifact_version": 0, "stable_id": "BAD", "projection": None},
    ],
    ids=["order-1", "order-2", "order-3"],
)
def test_py002_typed_fields_in_canonical_member_order(record: dict[str, Any]) -> None:
    # Canonical (UTF-16) member order visits artifact_version before stable_id (G-034).
    assert failure(core.compare_artifact_revisions, record, VALID) == ("version.invalid", "a")


@pytest.mark.parametrize(
    "record",
    [
        [],
        "ku-1",
        {"stable_id": "ku-1", "artifact_version": 1},
        {"artifact_version": 1, "projection": {}},
        {"stable_id": "ku-1"},
        {"stable_id": "ku-1", "projection": {}, "extra": 1},
        {"stable_id": "BAD", "projection": {}, "extra": 1},
    ],
    ids=[
        "array",
        "string",
        "no-projection",
        "no-stable-id",
        "no-version-no-projection",
        "extra",
        "extra-beats-field",
    ],
)
def test_g035_record_shape(record: object) -> None:
    assert failure(core.compare_artifact_revisions, record, VALID) == ("artifact.revision_invalid", "a")


def test_g035_absent_version_is_not_applicable_only_when_complete() -> None:
    complete = {"stable_id": "ku-1", "projection": {"t": "x"}}
    assert core.compare_artifact_revisions(complete, VALID) == "not_applicable"


def test_g035_present_invalid_id_after_valid_shape() -> None:
    record = {"stable_id": "BAD", "projection": {}}
    assert failure(core.compare_artifact_revisions, record, VALID) == ("id.invalid", "a")


# ------------------------------------------------- PY-003: stage-1 source order across representation


def test_py003_escaped_surrogate_before_raw_surrogate() -> None:
    text = '["\\ud800", "' + chr(0xD800) + '"]'
    assert failure(core.decode_json, text)[0] == "value.lone_surrogate"


def test_py003_duplicate_before_invalid_utf8() -> None:
    assert failure(core.decode_json, b'[{"a":1,"a":2},"\xff"]')[0] == "value.duplicate_key"


def test_py003_unpaired_high_escape_before_malformed_escape() -> None:
    assert failure(core.decode_json, '"\\ud800\\uZZZZ"')[0] == "value.lone_surrogate"


def test_py003_encoding_defect_still_reported_where_it_is_first() -> None:
    assert failure(core.decode_json, b'["\xff", {"a":1,"a":2}]')[0] == "value.invalid_json"
    assert failure(core.decode_json, b"[1]\xff")[0] == "value.invalid_json"
    assert failure(core.decode_json, "[1]" + chr(0xDC00))[0] == "value.invalid_json"


# ------------------------------------------------- PY-004: diagnostics never replace the stable code


def test_py004_huge_integer_keeps_its_code() -> None:
    assert failure(core.validate_value, 10**5000)[0] == "value.integer_out_of_range"
    assert failure(core.decode_json, "1" + "0" * 5000)[0] == "value.number_not_finite"


def test_py004_deep_wrong_type_keeps_its_code() -> None:
    deep: list[Any] = []
    for _ in range(100_000):
        deep = [deep]
    assert failure(core.validate_version, deep) == ("version.invalid", None)
    assert failure(core.validate_stable_id, deep)[0] == "id.invalid"
    assert failure(core.normalize_timestamp, deep)[0] == "timestamp.invalid"


def test_py004_huge_string_diagnostics_are_bounded() -> None:
    with pytest.raises(core.MnemeValidationError) as raised:
        core.validate_stable_id("X" * 1_000_000)
    assert raised.value.code == "id.invalid" and len(str(raised.value)) < 500


# ------------------------------------------------- G-036: list and tuple are arrays


def test_g036_tuple_is_an_array() -> None:
    assert core.validate_value((1, (2.0, "x"))) == [1, [2, "x"]]
    assert core.canonical_bytes((1, 2)) == core.canonical_bytes([1, 2]) == b"[1,2]"


@pytest.mark.parametrize("value", [range(3), (x for x in [1]), bytearray(b"x"), memoryview(b"x")])
def test_g036_other_sequences_stay_unsupported(value: object) -> None:
    assert failure(core.validate_value, value)[0] == "value.unsupported_type"


def test_g036_tuple_order_and_stage2_by_index() -> None:
    assert failure(core.validate_value, (math.inf, 2**53))[0] == "value.number_not_finite"


# ------------------------------------------------- PY-007: raw content hash (CORE-011, SER-009)


@pytest.mark.parametrize(
    "data",
    [b"abc", b"line\r\n", b"line\n", "é".encode(), "é".encode(), b"", b"\xff\x00"],
    ids=["abc", "crlf", "lf", "nfc", "nfd", "empty", "binary"],
)
def test_py007_raw_content_hash_is_sha256_of_bytes_as_given(data: bytes) -> None:
    assert core.raw_content_hash(data) == hashlib.sha256(data).hexdigest()


def test_py007_raw_and_structured_hashes_differ_for_text() -> None:
    assert core.raw_content_hash(b"abc") == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    assert core.content_hash("abc") == hashlib.sha256(b'"abc"').hexdigest()
