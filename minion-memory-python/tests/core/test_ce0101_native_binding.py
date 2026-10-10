"""CE-01-01 controls: Python native binding of built-in-kind subclasses (agreed checkpoint 8e8d30a8).

Owner decisions G-037 (subclasses admitted, base values only, no user hook), G-039 (native duplicate
names), G-040 (cycles), G-041 (byte buffers and control objects), G-042 (exact built-in returns) and
G-043 (non-dict argument maps). Every hostile hook records itself and raises; a correct
implementation never reaches one. These controls were run against the frozen code eb47635c first.
"""

from __future__ import annotations

import collections
import enum
import math
import subprocess
import sys
import typing
from collections.abc import Iterator, Mapping
from pathlib import Path
from typing import Any

import pytest

from minion_memory import core

HOOKS = [
    "__repr__", "__str__", "__format__", "__int__", "__index__", "__float__", "__hash__", "__eq__",
    "__ne__", "__lt__", "__le__", "__gt__", "__ge__", "__iter__", "__reversed__", "__len__",
    "__getitem__", "__contains__", "__bool__", "__abs__", "__neg__", "items", "keys", "values", "get",
    "encode", "is_integer", "bit_length", "startswith", "endswith", "split", "__getattribute__",
]  # fmt: skip
CALLS: list[str] = []
ACTIVE = False


class HookFired(Exception):
    pass


def _hostile(name: str) -> Any:
    def hook(self: Any, *args: Any, **kwargs: Any) -> Any:
        if ACTIVE:
            CALLS.append(name)
            raise HookFired(name)
        return getattr(super(type(self), self), name)(*args, **kwargs)

    return hook


def hostile(base: type, *hooks: str) -> type:
    attrs: dict[str, Any] = {name: _hostile(name) for name in hooks}
    if "__eq__" in hooks and "__hash__" not in hooks and base.__hash__ is not None:
        attrs["__hash__"] = base.__hash__
    return type(f"Hostile_{base.__name__}", (base,), attrs)


def observe(call: Any) -> tuple[Any, list[str]]:
    global ACTIVE
    CALLS.clear()
    ACTIVE = True
    try:
        result: Any = ("value", call())
    except core.MnemeValidationError as error:
        result = ("error", error.code)
    except HookFired as error:
        result = ("hook", str(error))
    except Exception as error:
        result = ("host", type(error).__name__)
    finally:
        ACTIVE = False
    return result, list(CALLS)


def exact(value: object) -> bool:
    """Recursively exact built-ins (G-042); checked without invoking any subclass behaviour."""
    pending = [value]
    while pending:
        item = pending.pop()
        kind = type(item)
        if item is None or kind in (bool, int, float, str, bytes):
            continue
        if kind is list:
            pending.extend(list.__iter__(item))  # type: ignore[arg-type]
            continue
        if kind is dict:
            key: object
            member: object
            for key, member in dict.items(item):  # type: ignore[arg-type]
                if type(key) is not str:
                    return False
                pending.append(member)
            continue
        return False
    return True


PAYLOADS: list[tuple[type, Any, Any, bytes]] = [
    (int, 1, 1, b"1"),
    (float, 1.5, 1.5, b"1.5"),
    (str, "a", "a", b'"a"'),
    (list, [1], [1], b"[1]"),
    (tuple, (1,), [1], b"[1]"),
    (dict, {"a": 1}, {"a": 1}, b'{"a":1}'),
]


# ------------------------------------------------------------------ G-037: no hook, base value only


@pytest.mark.parametrize("hook", [*HOOKS, "ALL"])
@pytest.mark.parametrize(
    ("base", "payload", "expected", "canonical"), PAYLOADS, ids=lambda v: getattr(v, "__name__", None)
)
def test_hostile_subclass_is_read_by_base_protocol(
    base: type, payload: Any, expected: Any, canonical: bytes, hook: str
) -> None:
    value = hostile(base, *(HOOKS if hook == "ALL" else [hook]))(payload)
    result, calls = observe(lambda: core.validate_value(value))
    assert calls == [] and result == ("value", expected) and exact(result[1]), (result, calls)
    result, calls = observe(lambda: core.canonical_bytes(value))
    assert calls == [] and result == ("value", canonical), (result, calls)
    result, calls = observe(lambda: core.content_hash([value]))
    assert calls == [] and result == ("value", core.content_hash([expected])), (result, calls)


@pytest.mark.parametrize(
    ("value_factory", "code"),
    [
        (lambda: hostile(float, *HOOKS)(math.inf), "value.number_not_finite"),
        (lambda: hostile(float, *HOOKS)(math.nan), "value.number_not_finite"),
        (lambda: hostile(int, *HOOKS)(2**53), "value.integer_out_of_range"),
        (lambda: hostile(int, *HOOKS)(10**5000), "value.integer_out_of_range"),
        (lambda: hostile(str, *HOOKS)("a\ud800"), "value.lone_surrogate"),
        (lambda: {hostile(str, *HOOKS)("\udc00"): 1}, "value.lone_surrogate"),
    ],
    ids=["inf", "nan", "2^53", "10^5000", "surrogate", "surrogate-name"],
)
def test_hostile_values_keep_their_stage2_codes(value_factory: Any, code: str) -> None:
    value = value_factory()
    assert observe(lambda: core.validate_value(value)) == (("error", code), [])


@pytest.mark.parametrize(
    ("call", "code"),
    [
        (lambda: core.validate_version(hostile(int, *HOOKS)(0)), "version.invalid"),
        (lambda: core.validate_stable_id(hostile(str, *HOOKS)("BAD")), "id.invalid"),
        (lambda: core.normalize_timestamp(hostile(str, *HOOKS)("nope")), "timestamp.invalid"),
        (
            lambda: core.operation_identity(hostile(dict, *HOOKS)({"source_namespace": "ns"})),
            "operation_identity.invalid",
        ),
    ],
    ids=["version", "id", "timestamp", "operation"],
)
def test_hostile_typed_rejections_keep_their_codes(call: Any, code: str) -> None:
    assert observe(call) == (("error", code), [])


def test_hostile_values_pass_typed_rules() -> None:
    assert observe(lambda: core.validate_version(hostile(int, *HOOKS)(3))) == (("value", 3), [])
    assert observe(lambda: core.validate_stable_id(hostile(str, *HOOKS)("ku-1"))) == (("value", "ku-1"), [])
    stamp = hostile(str, *HOOKS)("2026-10-10T16:30:00+11:00")
    assert observe(lambda: core.normalize_timestamp(stamp)) == (("value", "2026-10-10T05:30:00.000000Z"), [])


def test_conflict_diagnostics_never_format_hostile_values() -> None:
    a = {"stable_id": "ku-1", "artifact_version": hostile(int, *HOOKS)(1), "projection": {"t": "x"}}
    b = {"stable_id": "ku-1", "artifact_version": 1, "projection": {"t": "y"}}
    assert observe(lambda: core.compare_artifact_revisions(a, b)) == (
        ("error", "artifact.revision_conflict"),
        [],
    )


# ------------------------------------------------------------------ forged display and traversal


class DisplayFloat(float):
    def __repr__(self) -> str:
        return "2.5"


class DisplayInt(int):
    def __str__(self) -> str:
        return "2"


class ForgedItems(dict):  # type: ignore[type-arg]
    def items(self) -> Any:
        return [("forged", 9)]


class ForgedIter(list):  # type: ignore[type-arg]
    def __iter__(self) -> Iterator[Any]:
        return iter([9])


class StrEnumLike(str, enum.Enum):  # noqa: UP042 - the characterized `str, Enum` form
    A = "a"


class Colour(enum.IntEnum):
    RED = 1


class Pair(typing.NamedTuple):
    x: int
    y: str


def test_forged_display_and_traversal_never_change_bytes() -> None:
    assert core.canonical_bytes(DisplayFloat(1.5)) == b"1.5"
    assert core.canonical_bytes(DisplayInt(1)) == b"1"
    assert core.canonical_bytes(ForgedItems({"a": 1})) == b'{"a":1}'
    assert core.canonical_bytes(ForgedIter([1, 2])) == b"[1,2]"


def test_standard_subtypes_are_their_base_values() -> None:
    for value, expected in (
        (StrEnumLike.A, "a"),
        (Colour.RED, 1),
        (Pair(1, "a"), [1, "a"]),
        (collections.OrderedDict([("b", 2), ("a", 1)]), {"a": 1, "b": 2}),
        (collections.defaultdict(int, {"a": 1}), {"a": 1}),
    ):
        normalized = core.validate_value(value)
        assert normalized == expected and exact(normalized), (value, normalized)


# ------------------------------------------------------------------ G-042: exact built-in returns


def test_returned_values_are_exact_without_added_state() -> None:
    class Tagged(int):
        pass

    tagged = Tagged(1)
    tagged.extra = 99  # type: ignore[attr-defined]
    for value in (tagged, hostile(float)(1.5), [Tagged(2)], {"a": Tagged(3)}, (Tagged(4),)):
        normalized = core.validate_value(value)
        assert exact(normalized) and normalized is not value, normalized
    assert type(core.validate_version(Tagged(5))) is int


# ------------------------------------------------------------------ CH: spoofed __class__ (G-037)


def _spoof(kind: type, payload: object) -> object:
    class Spoof:
        @property  # type: ignore[misc]
        def __class__(self) -> type:  # isinstance() consults __class__ when type() does not match
            if ACTIVE:
                CALLS.append("__class__")
            return kind

        def __iter__(self) -> Iterator[Any]:
            CALLS.append("__iter__")
            return typing.cast(Iterator[Any], iter(payload))  # type: ignore[call-overload]

    return Spoof()


@pytest.mark.parametrize(
    ("kind", "payload"), [(int, 1), (float, 1.5), (str, "a"), (list, [1]), (dict, {"a": 1})]
)
def test_ch_spoofed_class_is_unsupported(kind: type, payload: object) -> None:
    spoof = _spoof(kind, payload)
    assert observe(lambda: core.validate_value(spoof)) == (("error", "value.unsupported_type"), [])


# ------------------------------------------------------------------ M01/M02: genuine ancestry only


class _RaisingMeta(type):
    def __getattribute__(cls, name: str) -> Any:
        if name == "__mro__" and ACTIVE:
            CALLS.append("metaclass __mro__")
            raise HookFired("__mro__")
        return type.__getattribute__(cls, name)


class _ForgedMRO(type):
    @property
    def __mro__(cls) -> tuple[type, ...]:
        if ACTIVE:
            CALLS.append("forged __mro__")
        return (int, object)


def test_m01_genuine_subclass_with_hostile_metaclass() -> None:
    real_int = _RaisingMeta("RealInt", (int,), {})
    result, calls = observe(lambda: core.validate_value(real_int(1)))
    assert calls == [] and result == ("value", 1) and type(result[1]) is int


def test_m02_forged_mro_is_not_a_subclass() -> None:
    not_int = _ForgedMRO("NotInt", (), {})
    assert observe(lambda: core.validate_value(not_int())) == (("error", "value.unsupported_type"), [])


# ------------------------------------------------------------------ G-041: control objects, buffers


def _text_descriptor_class() -> type:
    def getter(self: Any) -> str:
        if ACTIVE:
            CALLS.append("text descriptor")
            raise HookFired("text")
        return "unused"

    def setter(self: Any, value: Any) -> None:
        object.__getattribute__(self, "__dict__")["text"] = value

    return type("TextDescriptor", (core.JsonText,), {"text": property(getter, setter)})


def test_d01_jsontext_text_property_is_bypassed() -> None:
    wrapper = _text_descriptor_class()("1")
    assert observe(lambda: core.validate_arguments({"value": wrapper})) == (("value", {"value": 1}), [])


def test_d02_jsontext_dict_property_is_bypassed() -> None:
    def dict_hook(self: Any) -> Any:
        if ACTIVE:
            CALLS.append("__dict__ descriptor")
            raise HookFired("__dict__")
        return object.__getattribute__(self, "__dict__")

    wrapper = type("DictProperty", (core.JsonText,), {"__dict__": property(dict_hook)})("1")
    assert observe(lambda: core.validate_arguments({"value": wrapper})) == (("value", {"value": 1}), [])


def test_b066_jsontext_getattribute_is_bypassed() -> None:
    def getattribute(self: Any, name: str) -> Any:
        if ACTIVE:
            CALLS.append("__getattribute__")
            raise HookFired(name)
        return object.__getattribute__(self, name)

    wrapper = type("Wrapper", (core.JsonText,), {"__getattribute__": getattribute})("1")
    assert observe(lambda: core.validate_arguments({"value": wrapper})) == (("value", {"value": 1}), [])


@pytest.mark.parametrize("hook", ["__len__", "items", "__getattribute__", "__iter__", "keys"])
def test_dict_subclass_argument_map_is_read_by_base_entries(hook: str) -> None:
    arguments = hostile(dict, hook)({"a": 1, "b": 2})
    assert observe(lambda: core.validate_arguments(arguments)) == (("value", {"a": 1, "b": 2}), [])


class _CustomMapping(Mapping[str, int]):
    def __getitem__(self, key: str) -> int:
        CALLS.append("__getitem__")
        return 1

    def __iter__(self) -> Iterator[str]:
        CALLS.append("__iter__")
        return iter(["a"])

    def __len__(self) -> int:
        CALLS.append("__len__")
        return 1


def test_cm01_userdict_argument_map_is_caller_typeerror() -> None:
    arguments = collections.UserDict({"a": 1, "b": 2})
    assert observe(lambda: core.validate_arguments(arguments)) == (("host", "TypeError"), [])  # type: ignore[arg-type]


def test_cm02_custom_mapping_argument_map_is_caller_typeerror() -> None:
    arguments = _CustomMapping()
    assert observe(lambda: core.validate_arguments(arguments)) == (("host", "TypeError"), [])  # type: ignore[arg-type]


@pytest.mark.parametrize("base", [bytes, bytearray])
@pytest.mark.parametrize(
    "hook", ["__bytes__", "__iter__", "__len__", "__getitem__", "decode", "__repr__", "__getattribute__"]
)
def test_byte_buffer_subclasses_are_read_as_underlying_bytes(base: type, hook: str) -> None:
    data = hostile(base, hook)(b"1")
    assert observe(lambda: core.raw_content_hash(data)) == (("value", core.raw_content_hash(b"1")), [])
    assert observe(lambda: core.decode_json(data)) == (("value", 1), [])


def test_exact_bytearray_is_json_bytes() -> None:
    assert core.decode_json(bytearray(b'{"a":1}')) == {"a": 1}


# ------------------------------------------------------------------ G-039 duplicates, G-040 cycles


class _IdentityKey(str):
    __hash__ = object.__hash__

    def __eq__(self, other: object) -> bool:
        return self is other


def test_native_duplicate_base_names_are_duplicate_key() -> None:
    value = {_IdentityKey("a"): 1, _IdentityKey("a"): 2}
    assert observe(lambda: core.validate_value(value)) == (("error", "value.duplicate_key"), [])


def test_earlier_unsupported_kind_beats_a_later_duplicate() -> None:
    value = {"a": {1, 2}, "b": {_IdentityKey("x"): 1, _IdentityKey("x"): 2}}
    assert observe(lambda: core.validate_value(value)) == (("error", "value.unsupported_type"), [])


def test_shared_aliases_are_legal() -> None:
    shared = [1]
    assert core.validate_value([shared, shared, {"a": shared}]) == [[1], [1], {"a": [1]}]


CYCLE_PROGRAM = """
import sys
sys.path.insert(0, sys.argv[1])
from minion_memory import core
kind = sys.argv[2]
if kind == "list":
    value = []; value.append(value)
elif kind == "dict":
    value = {}; value["a"] = value
else:
    inner = []; value = (inner,); inner.append(value)
try:
    core.validate_value(value)
    print("returned")
except core.MnemeValidationError as error:
    print(error.code)
"""


@pytest.mark.parametrize("kind", ["list", "dict", "tuple-through-list"])
def test_cycles_are_rejected(kind: str) -> None:
    src = str(Path(core.__file__).resolve().parents[1])
    try:
        done = subprocess.run(
            [sys.executable, "-c", CYCLE_PROGRAM, src, kind],
            capture_output=True,
            text=True,
            timeout=20,
            check=False,
        )
    except subprocess.TimeoutExpired:
        pytest.fail(f"cyclic {kind} did not return within 20 s")
    assert done.stdout.strip() == "value.cyclic", (done.stdout, done.stderr[-300:])


# ------------------------------------------------------------------ stages across arguments


def test_stage1_in_later_argument_beats_hostile_stage2_in_earlier() -> None:
    result, calls = observe(lambda: core.compare_versions(hostile(float, *HOOKS)(math.inf), object()))
    assert (result, calls) == (("error", "value.unsupported_type"), [])
