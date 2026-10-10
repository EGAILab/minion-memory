"""Identity, versions, operation identity and revision comparison (CORE-001/002/003/012).

Typed field rules are stage 3 of SER-008 (G-031). A primitive with several arguments validates all
of them through stages 1-2 first, in declared order, and only then applies stage-3 rules in declared
order (G-034); the failing argument is named on the error.
"""

from __future__ import annotations

import re
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from typing import Literal

from .errors import MnemeValidationError
from .values import SAFE_INTEGER_MAX, Value, canonical_bytes, validate_value

_STABLE_ID = re.compile(r"[a-z0-9][a-z0-9._-]{0,127}", re.ASCII)
_DEVICE = re.compile(r"(con|prn|aux|nul|com[1-9]|lpt[1-9])", re.ASCII)


def _domain(arguments: Mapping[str, object]) -> dict[str, Value]:
    """Stages 1-2 for every argument in declared order; returns the normalized values."""
    values: dict[str, Value] = {}
    for name, value in arguments.items():
        try:
            values[name] = validate_value(value)
        except MnemeValidationError as error:
            raise error.for_argument(name) from None
    return values


def _typed[T](arguments: Mapping[str, Value], rule: Callable[[Value], T]) -> list[T]:
    """Stage 3 for every argument in declared order."""
    results: list[T] = []
    for name, value in arguments.items():
        try:
            results.append(rule(value))
        except MnemeValidationError as error:
            raise error.for_argument(name) from None
    return results


# ------------------------------------------------------------------------- CORE-001 stable ID (G-020)


def _stable_id_rule(value: Value) -> str:
    if not isinstance(value, str):
        raise MnemeValidationError("id.invalid", "a stable ID is a string")
    if not _STABLE_ID.fullmatch(value) or value.endswith("."):
        raise MnemeValidationError("id.invalid", f"{value!r} is not a portable lowercase token")
    if _DEVICE.fullmatch(value.split(".", 1)[0]):
        raise MnemeValidationError("id.invalid", f"{value!r} has a reserved device basename")
    return value


def validate_stable_id(value: object) -> str:
    """CORE-001 (G-020): 1-128 ASCII bytes `[a-z0-9][a-z0-9._-]*`, no trailing `.`, no device basename."""
    return _typed(_domain({"id": value}), _stable_id_rule)[0]


# ------------------------------------------------------------------------- CORE-002 version (G-012)


def _version_rule(value: Value) -> int:
    # A normalized number is an `int` exactly when it is integral (one numeric kind, G-023).
    if isinstance(value, bool) or not isinstance(value, int) or not 1 <= value <= SAFE_INTEGER_MAX:
        raise MnemeValidationError("version.invalid", f"{value!r} is not a positive integer edition")
    return value


def validate_version(value: object) -> int:
    return _typed(_domain({"version": value}), _version_rule)[0]


def compare_versions(a: object, b: object) -> Literal["greater", "equal", "less"]:
    """CORE-002: positive integer editions compared numerically."""
    first, second = _typed(_domain({"a": a, "b": b}), _version_rule)
    return "greater" if first > second else "less" if first < second else "equal"


# ------------------------------------------------------------ CORE-012 operation identity (G-032)


@dataclass(frozen=True)
class OperationIdentity:
    """`{source_namespace, source_operation_id}`; uniqueness applies to the pair (G-032)."""

    source_namespace: str
    source_operation_id: str


_PAIR = frozenset({"source_namespace", "source_operation_id"})


def _operation_rule(value: Value) -> OperationIdentity:
    if not isinstance(value, dict) or set(value) != _PAIR:
        raise MnemeValidationError(
            "operation_identity.invalid",
            "an operation identity is exactly {source_namespace, source_operation_id}",
        )
    namespace, operation_id = value["source_namespace"], value["source_operation_id"]
    try:
        _stable_id_rule(namespace)
    except MnemeValidationError:
        raise MnemeValidationError(
            "operation_identity.invalid", "source_namespace is not a stable-ID token"
        ) from None
    if not isinstance(operation_id, str) or not 1 <= len(operation_id.encode("utf-8")) <= 256:
        raise MnemeValidationError(
            "operation_identity.invalid", "source_operation_id is a string of 1-256 UTF-8 bytes"
        )
    assert isinstance(namespace, str)
    return OperationIdentity(namespace, operation_id)


def operation_identity(value: object) -> OperationIdentity:
    """Validate an operation identity (G-032/G-033); compared exactly, never normalized."""
    return _typed(_domain({"identity": value}), _operation_rule)[0]


def compare_operation_identities(a: object, b: object) -> Literal["same", "different"]:
    first, second = _typed(_domain({"a": a, "b": b}), _operation_rule)
    return "same" if first == second else "different"


# ------------------------------------------------------------ CORE-003 revision comparison (G-021)


def _revision_rule(value: Value) -> tuple[str, int | None, Value]:
    if not isinstance(value, dict) or "stable_id" not in value or "projection" not in value:
        raise MnemeValidationError("id.invalid", "a revision carries stable_id and projection")
    stable_id = _stable_id_rule(value["stable_id"])
    version = _version_rule(value["artifact_version"]) if "artifact_version" in value else None
    return stable_id, version, value["projection"]


def compare_artifact_revisions(a: object, b: object) -> Literal["identical", "distinct", "not_applicable"]:
    """CORE-003 Layer 01 comparison; raises `artifact.revision_conflict` on a visible conflict."""
    (id_a, version_a, projection_a), (id_b, version_b, projection_b) = _typed(
        _domain({"a": a, "b": b}), _revision_rule
    )
    if version_a is None or version_b is None:
        return "not_applicable"  # only immutable semantically versioned artifacts (G-021)
    if id_a != id_b or version_a != version_b:
        return "distinct"
    if canonical_bytes(projection_a) == canonical_bytes(projection_b):
        return "identical"
    raise MnemeValidationError(
        "artifact.revision_conflict", f"{id_a} version {version_a} has different canonical content"
    )
