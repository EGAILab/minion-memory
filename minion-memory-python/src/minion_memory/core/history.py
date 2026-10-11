"""History envelope, replay admission and HistoryBoundary (Layer 02: HIST-002, HIST-005, SER-005,
SER-006; Owner G-048..G-055).

Arguments pass SER-008 stages 1 and 2 together (`validate_arguments`, G-031). Stage 3 then applies:
the reader first, then the record through four ordered gates (G-055): identity/routing, profile
admission and recognition, closed shape, typed fields in canonical member order. Argument errors
name their argument; the relation errors `history.unknown_required` and
`history.boundary_incomparable` name none.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal

from .errors import MnemeValidationError, describe
from .identity import _stable_id_rule
from .time import _timestamp_rule
from .values import SAFE_INTEGER_MAX, Value, _utf16_key, validate_arguments

_ROUTING = ("profile", "record_type", "schema_version")
_REQUIRED = frozenset({"record_id", *_ROUTING, "compatibility", "recorded_at"})
_MEMBERS = _REQUIRED | {"occurred_at", "payload", "provenance"}
# A record draft (Layer 04, G-082/G-083) is an envelope without `recorded_at`, which the store owns.
_DRAFT_REQUIRED = _REQUIRED - {"recorded_at"}
_DRAFT_MEMBERS = _MEMBERS - {"recorded_at"}
_COMPATIBILITY = ("required", "ignorable")


def _is_token(value: Value) -> bool:
    """G-052: the CORE-001 lowercase portable-token grammar, compared exactly."""
    try:
        _stable_id_rule(value)
    except MnemeValidationError:
        return False
    return True


def _is_edition(value: Value) -> bool:
    """G-052: an integral schema edition 1..2^53-1 (normalized numbers are `int` iff integral)."""
    return isinstance(value, int) and not isinstance(value, bool) and 1 <= value <= SAFE_INTEGER_MAX


# ------------------------------------------------------------------ replay reader (SER-005, G-053)


def _reader_rule(value: Value) -> tuple[str, frozenset[tuple[str, int]]]:
    def invalid(detail: str) -> MnemeValidationError:
        return MnemeValidationError("history.reader_invalid", detail)

    if not isinstance(value, dict) or set(value) != {"profile", "recognized"}:
        raise invalid("a reader is exactly {profile, recognized}")
    profile, recognized = value["profile"], value["recognized"]
    if not _is_token(profile):
        raise invalid(f"reader profile {describe(profile)} is not a portable token")
    if not isinstance(recognized, list):
        raise invalid("recognized is an array of {record_type, schema_version}")
    pairs: set[tuple[str, int]] = set()
    for entry in recognized:
        if not isinstance(entry, dict) or set(entry) != {"record_type", "schema_version"}:
            raise invalid("a recognized entry is exactly {record_type, schema_version}")
        record_type, schema_version = entry["record_type"], entry["schema_version"]
        if not _is_token(record_type) or not _is_edition(schema_version):
            raise invalid("a recognized entry needs a token record_type and an edition 1..2^53-1")
        assert isinstance(record_type, str) and isinstance(schema_version, int)
        if (record_type, schema_version) in pairs:
            raise invalid("duplicate recognized (record_type, schema_version)")  # incl. 1 and 1.0 (G-053)
        pairs.add((record_type, schema_version))
    assert isinstance(profile, str)
    return profile, frozenset(pairs)


# ------------------------------------------------------------------ envelope gates (HIST-002, G-055)


@dataclass(frozen=True)
class HistoryAdmission:
    """The outcome of admitting one envelope; only `decode` carries the normalized envelope."""

    outcome: Literal["decode", "skip", "excluded"]
    envelope: dict[str, Value] | None = None


class _UnknownRequired(Exception):
    """Gate 2 replay stop; raised as `history.unknown_required` outside argument naming."""


def _envelope_invalid(detail: str) -> MnemeValidationError:
    return MnemeValidationError("history.envelope_invalid", detail)


def _admit(
    reader: tuple[str, frozenset[tuple[str, int]]], value: Value, *, draft: bool = False
) -> HistoryAdmission:
    """The four G-055 gates. With `draft`, gate 3's closed member set omits `recorded_at` (Layer 04
    draft gates D1-D4), so a supplied `recorded_at` is an unknown member."""
    profile, recognized = reader
    # Gate 1: identity and routing — the schema-identity triple, for every record (G-052).
    if not isinstance(value, dict):
        raise _envelope_invalid(f"an envelope is an object, not {describe(value)}")
    for name in _ROUTING:
        if name not in value:
            raise _envelope_invalid(f"missing {name}")
    record_profile, record_type, schema_version = (value[name] for name in _ROUTING)
    if not _is_token(record_profile) or not _is_token(record_type):
        raise _envelope_invalid("profile and record_type are portable tokens")
    if not _is_edition(schema_version):
        raise _envelope_invalid("schema_version is an integral edition 1..2^53-1")
    # Gate 2: profile admission (G-050), then compatibility, then recognition (SER-005).
    if record_profile != profile:
        return HistoryAdmission("excluded")
    compatibility = value.get("compatibility")
    if compatibility not in _COMPATIBILITY or not isinstance(compatibility, str):
        raise _envelope_invalid('compatibility is exactly "required" or "ignorable"')
    if (record_type, schema_version) not in recognized:
        if compatibility == "ignorable":
            return HistoryAdmission("skip")
        raise _UnknownRequired(f"{record_type} v{schema_version} is required but not recognized")
    # Gate 3: closed shape of a recognized same-profile schema (G-048, G-049).
    members = set(value)
    allowed, required = (_DRAFT_MEMBERS, _DRAFT_REQUIRED) if draft else (_MEMBERS, _REQUIRED)
    if members - allowed or required - members:
        raise _envelope_invalid(
            "a recognized draft has the G-048 members without recorded_at"
            if draft
            else "a recognized envelope has exactly the G-048 members"
        )
    # Gate 4: typed fields in canonical member order (G-034).
    envelope = dict(value)
    for name in sorted(value, key=_utf16_key):
        if name in ("occurred_at", "recorded_at"):
            envelope[name] = _timestamp_rule(value[name])
        elif name == "record_id":
            _stable_id_rule(value[name])
    return HistoryAdmission("decode", envelope)


def admit_history_record(reader: object, record: object) -> HistoryAdmission:
    """SER-005 replay admission of one history envelope (HIST-002) by one profile reader."""
    values = validate_arguments({"reader": reader, "record": record})
    try:
        parsed_reader = _reader_rule(values["reader"])
    except MnemeValidationError as error:
        raise error.for_argument("reader") from None
    try:
        return _admit(parsed_reader, values["record"])
    except MnemeValidationError as error:
        raise error.for_argument("record") from None
    except _UnknownRequired as stop:
        raise MnemeValidationError("history.unknown_required", str(stop)) from None


# ------------------------------------------------------------------ HistoryBoundary (HIST-005, G-051, G-054)


def _boundary_rule(value: Value) -> tuple[str, int]:
    def invalid(detail: str) -> MnemeValidationError:
        return MnemeValidationError("history.boundary_invalid", detail)

    if not isinstance(value, dict) or set(value) != {"store_id", "position"}:
        raise invalid("a boundary is exactly {store_id, position}")
    store_id, position = value["store_id"], value["position"]
    if not _is_token(store_id):
        raise invalid(f"store_id {describe(store_id)} is not a stable ID")
    if isinstance(position, bool) or not isinstance(position, int) or not 0 <= position <= SAFE_INTEGER_MAX:
        raise invalid(f"position {describe(position)} is not an integer 0..2^53-1")
    assert isinstance(store_id, str)
    return store_id, position


def _boundaries(a: object, b: object) -> tuple[tuple[str, int], tuple[str, int]]:
    values = validate_arguments({"a": a, "b": b})
    parsed = []
    for name in ("a", "b"):
        try:
            parsed.append(_boundary_rule(values[name]))
        except MnemeValidationError as error:
            raise error.for_argument(name) from None
    return parsed[0], parsed[1]


def compare_history_boundaries(a: object, b: object) -> Literal["less", "equal", "greater"]:
    """Order two boundaries of one store; different stores are incomparable (G-051)."""
    (store_a, position_a), (store_b, position_b) = _boundaries(a, b)
    if store_a != store_b:
        raise MnemeValidationError("history.boundary_incomparable", "boundaries of different stores")
    return "less" if position_a < position_b else "greater" if position_a > position_b else "equal"


def history_boundaries_equal(a: object, b: object) -> bool:
    """Structural equality over the complete pair; different stores are simply unequal (G-051)."""
    first, second = _boundaries(a, b)
    return first == second
