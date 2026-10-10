"""Layer 01 unit and differential tests beyond the JSON-text canonical scenarios.

The scenarios pin the shared contract at the JSON-text seam; these tests cover the native-value seam
(SER-008 stage 1 `value.unsupported_type`, native stage 2), G-034 order independence for native maps,
totality on deep values, and a randomized differential check of canonical bytes against the RFC 8785
reference library (`rfc8785`, a dev-only test oracle, never a runtime dependency).
"""

from __future__ import annotations

import math
import random
import struct
from datetime import datetime
from typing import Any

import pytest
import rfc8785

from minion_memory import core


def code(call: Any, *args: Any) -> str:
    with pytest.raises(core.MnemeValidationError) as raised:
        call(*args)
    return raised.value.code


# ------------------------------------------------------------------ native seam (SER-008 stage 1/2)


@pytest.mark.parametrize(
    "value",
    [b"bytes", {1, 2}, range(2), datetime(2026, 1, 1), {1: "non-string key"}, [object()], {"a": {"b": b"x"}}],
    ids=["bytes", "set", "range", "datetime", "int-key", "object", "nested-bytes"],
)
def test_native_kinds_outside_json_are_unsupported(value: object) -> None:
    assert code(core.validate_value, value) == "value.unsupported_type"


@pytest.mark.parametrize("number", [math.nan, math.inf, -math.inf])
def test_native_non_finite_numbers(number: float) -> None:
    assert code(core.validate_value, {"v": number}) == "value.number_not_finite"


def test_native_surrogates_in_strings_and_names() -> None:
    assert code(core.validate_value, "a\ud800b") == "value.lone_surrogate"
    assert code(core.validate_value, {"\udc00": 1}) == "value.lone_surrogate"


def test_native_integers_are_exact_and_bounded() -> None:
    assert core.validate_value(2**53 - 1) == 2**53 - 1
    assert code(core.validate_value, 2**53) == "value.integer_out_of_range"
    assert code(core.validate_value, -(2**53)) == "value.integer_out_of_range"


def test_one_numeric_kind_and_negative_zero() -> None:
    assert core.validate_value({"a": 1.0, "b": -0.0}) == {"a": 1, "b": 0}
    assert type(core.validate_value(2.0)) is int
    assert (
        core.canonical_bytes(1) == core.canonical_bytes(1.0) == core.canonical_bytes(core.decode_json("1e0"))
    )


def test_unsupported_kind_beats_a_value_domain_error() -> None:
    """Stage 1 (kinds) runs over the whole value before stage 2 (G-031)."""
    assert code(core.validate_value, {"a": 2**53, "b": b"x"}) == "value.unsupported_type"


# ------------------------------------------------------------------ G-034 order independence


def test_native_map_order_never_changes_the_error() -> None:
    first = {"b": 2**53, "a": math.inf}
    second = {"a": math.inf, "b": 2**53}
    assert code(core.validate_value, first) == code(core.validate_value, second) == "value.number_not_finite"


def test_member_name_checked_before_its_value() -> None:
    assert code(core.validate_value, {"\ud800": math.inf}) == "value.lone_surrogate"


def test_stage_two_in_any_argument_beats_stage_three() -> None:
    """compare_versions: stage 2 runs for all arguments before any typed rule (G-031)."""
    with pytest.raises(core.MnemeValidationError) as raised:
        core.compare_versions(0, 2**53)
    assert (raised.value.code, raised.value.argument) == ("value.integer_out_of_range", "b")


# ------------------------------------------------------------------ JSON text stage 1 details


@pytest.mark.parametrize(
    ("text", "expected"),
    [
        ("", "value.invalid_json"),
        ("[1,]", "value.invalid_json"),
        ('{"a":1,}', "value.invalid_json"),
        ("// c\n1", "value.invalid_json"),
        ("-", "value.invalid_json"),
        ("1.", "value.invalid_json"),
        ("1e", "value.invalid_json"),
        ('"\x01"', "value.invalid_json"),
        ('"\\x"', "value.invalid_json"),
        ('"\\u12"', "value.invalid_json"),
        ('"\\ud800\\u0041"', "value.lone_surrogate"),
        ('["\\udc00", {"a":1,"a":2}]', "value.lone_surrogate"),
        ('[{"a":1,"a":2}, "\\udc00"]', "value.duplicate_key"),
    ],
)
def test_stage_one_first_violation_in_source_order(text: str, expected: str) -> None:
    assert code(core.decode_json, text) == expected


def test_invalid_utf8_bytes() -> None:
    assert code(core.decode_json, b'"\xed\xa0\x80"') == "value.invalid_json"  # raw surrogate bytes
    assert core.decode_json(b'"\xc3\xa9"') == "\u00e9"


def test_escaped_surrogate_pair_decodes() -> None:
    assert core.decode_json('"\\ud83d\\ude00"') == "\U0001f600"


def test_deep_nesting_is_total() -> None:
    depth = 100_000
    value = core.decode_json("[" * depth + "]" * depth)
    assert core.canonical_bytes(value) == ("[" * depth + "]" * depth).encode()


# ------------------------------------------------------------------ differential: RFC 8785 reference


def _random_finite_double(rng: random.Random) -> float:
    while True:
        number = struct.unpack("<d", rng.getrandbits(64).to_bytes(8, "little"))[0]
        if math.isfinite(number) and not (number.is_integer() and abs(number) > 2**53 - 1):
            return float(number)


def test_number_formatting_matches_the_reference_on_random_doubles() -> None:
    rng = random.Random(20261010)
    samples = [_random_finite_double(rng) for _ in range(20_000)]
    samples += [
        5e-324,
        1e-7,
        1e-6,
        9.999999999999999e-7,
        123456789012345.67,
        1.5e-7,
        0.1,
        123456789.125,
        -2.5,
    ]
    for number in samples:
        expected = rfc8785.dumps(number)
        assert core.canonical_bytes(number) == expected, repr(number)


def test_strings_and_key_order_match_the_reference() -> None:
    rng = random.Random(7)
    alphabet = [
        chr(c) for c in (*range(0x20), 0x22, 0x5C, 0x41, 0x7F, 0xE9, 0x2028, 0xFF61, 0x1F600, 0x10FFFF)
    ]
    for _ in range(2_000):
        obj = {
            "".join(rng.choice(alphabet) for _ in range(rng.randint(0, 4))): "".join(
                rng.choice(alphabet) for _ in range(rng.randint(0, 6))
            )
            for _ in range(rng.randint(0, 5))
        }
        assert core.canonical_bytes(obj) == rfc8785.dumps(obj), ascii(obj)


# ------------------------------------------------------------------ typed rules


def test_operation_identity_is_exact_pair() -> None:
    identity = core.operation_identity({"source_namespace": "ns", "source_operation_id": "A "})
    assert identity == core.OperationIdentity("ns", "A ")
    extra = {"source_namespace": "ns", "source_operation_id": "a", "x": 1}
    assert code(core.operation_identity, extra) == "operation_identity.invalid"


def test_revision_conflict_is_a_visible_error() -> None:
    a = {"stable_id": "ku-1", "artifact_version": 1, "projection": {"t": "x"}}
    b = {"stable_id": "ku-1", "artifact_version": 1.0, "projection": {"t": "y"}}
    assert code(core.compare_artifact_revisions, a, b) == "artifact.revision_conflict"
