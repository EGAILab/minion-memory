"""Public-surface controls for reachable Layer 01 behaviour not otherwise executed (WP01-CR-010).

Each test exercises a documented public behaviour through the public API; none targets a private
helper to move a coverage counter.
"""

from __future__ import annotations

import hashlib
from typing import Any

import pytest

from minion_memory import core


@pytest.mark.parametrize(
    "text",
    ["[1 2]", '{"a":1 "b":2}', '{"a" 1}', '{"a",1}'],
    ids=["array-missing-comma", "object-missing-comma", "missing-colon", "comma-for-colon"],
)
def test_structural_punctuation_errors_are_invalid_json(text: str) -> None:
    with pytest.raises(core.MnemeValidationError) as raised:
        core.decode_json(text)
    assert raised.value.code == "value.invalid_json"


@pytest.mark.parametrize(
    "text", [123, None, ["[]"], memoryview(b"[]")], ids=["int", "none", "list", "memoryview"]
)
def test_json_text_must_be_str_or_byte_buffer(text: Any) -> None:
    with pytest.raises(TypeError):
        core.decode_json(text)


def test_jsontext_public_accessor_repr_and_immutability() -> None:
    wrapper = core.JsonText('{"a":1}')
    assert wrapper.text == '{"a":1}'
    assert repr(wrapper) == "JsonText(...)"  # never echoes caller content
    with pytest.raises(AttributeError):
        wrapper.text = "x"  # type: ignore[misc]
    with pytest.raises(AttributeError):
        wrapper.other = 1
    assert core.validate_arguments({"value": wrapper}) == {"value": {"a": 1}}


def test_jsontext_without_stored_text_is_caller_misuse() -> None:
    class NeverStored(core.JsonText):
        def __init__(self) -> None:  # does not call the base initializer
            pass

    with pytest.raises(TypeError):
        core.validate_arguments({"value": NeverStored()})


@pytest.mark.parametrize("name", [1, None, b"a"], ids=["int", "none", "bytes"])
def test_argument_names_must_be_strings(name: Any) -> None:
    with pytest.raises(TypeError):
        core.validate_arguments({name: 1})


def test_str_subclass_argument_names_are_read_as_their_base_string() -> None:
    class Name(str):
        def __str__(self) -> str:
            raise AssertionError("subclass __str__ must not be called")

    with pytest.raises(core.MnemeValidationError) as raised:
        core.validate_arguments({Name("a"): 1, "b": float("inf")})
    assert (raised.value.code, raised.value.argument) == ("value.number_not_finite", "b")


def test_raw_content_hash_reads_an_exact_memoryview() -> None:
    assert core.raw_content_hash(memoryview(b"abc")) == hashlib.sha256(b"abc").hexdigest()


@pytest.mark.parametrize("data", ["abc", 7, ["a"], None], ids=["str", "int", "list", "none"])
def test_raw_content_hash_requires_bytes(data: Any) -> None:
    with pytest.raises(TypeError):
        core.raw_content_hash(data)
