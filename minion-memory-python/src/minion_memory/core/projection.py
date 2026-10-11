"""The canonical HistoryProjector and lineage resolution (Layer 03: PROJ-001, PROJ-002, HIST-006,
HIST-007; Owner G-067..G-075).

`project_history` is the single semantic path from stored records to a purpose-neutral projection.
It fails atomically: an error anywhere returns no partial result (G-070). Order of evaluation:
generic stages across all arguments (G-031); reader, stream and early lineage shape checks; entries
in position order through the Layer 02 gates, with repeated record IDs coalesced or rejected
(G-073); late lineage graph checks (G-071, G-072); resolution of set-valued current heads and
direct-only interpretations (G-069, G-074). No ordering ever selects among heads.
"""

from __future__ import annotations

import re
from collections.abc import Iterable
from typing import Any

from .errors import MnemeValidationError, describe
from .history import _admit, _is_edition, _is_token, _reader_rule, _UnknownRequired
from .values import SAFE_INTEGER_MAX, Value, canonical_bytes, validate_arguments

RESOLUTION = ("supersedes", "corrects", "reinterprets")
REPLACING = ("supersedes", "corrects")
VOCABULARY = (*RESOLUTION, "derived_from")
_CONTENT_HASH = re.compile(r"[0-9a-f]{64}", re.ASCII)  # CORE-011 canonical SHA-256 form (G-075)

# A normalized relation: (from_record_id, relation, target), target = ("record", id) |
# ("versioned", stable_id, edition) | ("hash", content_hash).
_Relation = tuple[str, str, tuple[Any, ...]]


def _stream_invalid(detail: str) -> MnemeValidationError:
    return MnemeValidationError("history.stream_invalid", detail)


def _lineage_invalid(detail: str) -> MnemeValidationError:
    return MnemeValidationError("lineage.invalid", detail)


# ------------------------------------------------------------------ argument shapes (step 2)


def _stream_rule(value: Value) -> tuple[str, list[tuple[int, Value]]]:
    if not isinstance(value, dict) or set(value) != {"store_id", "entries"}:
        raise _stream_invalid("a stream is exactly {store_id, entries}")
    store_id, entries = value["store_id"], value["entries"]
    if not _is_token(store_id):
        raise _stream_invalid(f"store_id {describe(store_id)} is not a stable ID")
    if not isinstance(entries, list):
        raise _stream_invalid("entries is an array of {position, record}")
    result: list[tuple[int, Value]] = []
    previous = 0
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {"position", "record"}:
            raise _stream_invalid("an entry is exactly {position, record}")
        position = entry["position"]
        if (
            isinstance(position, bool)
            or not isinstance(position, int)
            or not previous < position <= SAFE_INTEGER_MAX
        ):
            raise _stream_invalid("entry positions are integers strictly increasing from 1 (G-054)")
        previous = position
        result.append((position, entry["record"]))
    assert isinstance(store_id, str)
    return store_id, result


def _target(relation: str, item: dict[str, Value]) -> tuple[Any, ...]:
    if set(item) == {"from_record_id", "relation", "to_record_id"}:
        if not _is_token(item["to_record_id"]):
            raise _lineage_invalid("to_record_id is not a stable ID")
        return ("record", item["to_record_id"])
    if set(item) != {"from_record_id", "relation", "to_artifact"}:
        raise _lineage_invalid("a relation has exactly one target: to_record_id or to_artifact")
    if relation != "derived_from":
        raise _lineage_invalid("only derived_from may target an external artifact (G-072)")
    artifact = item["to_artifact"]
    if isinstance(artifact, dict) and set(artifact) == {"stable_id", "artifact_version"}:
        if _is_token(artifact["stable_id"]) and _is_edition(artifact["artifact_version"]):
            return ("versioned", artifact["stable_id"], artifact["artifact_version"])
    elif isinstance(artifact, dict) and set(artifact) == {"content_hash"}:
        digest = artifact["content_hash"]
        if isinstance(digest, str) and _CONTENT_HASH.fullmatch(digest):
            return ("hash", digest)
    raise _lineage_invalid("to_artifact is exactly {stable_id, artifact_version} or {content_hash} (G-075)")


def _relations_rule(value: Value) -> list[_Relation]:
    """Early (shape) checks; equal relations collapse to one (G-072)."""
    if not isinstance(value, list):
        raise _lineage_invalid("relations is an array")
    seen: dict[_Relation, None] = {}
    for item in value:
        if not isinstance(item, dict) or "from_record_id" not in item or "relation" not in item:
            raise _lineage_invalid("a relation is a closed object")
        source, relation = item["from_record_id"], item["relation"]
        if not _is_token(source):
            raise _lineage_invalid("from_record_id is not a stable ID")
        if not isinstance(relation, str) or relation not in VOCABULARY:
            raise _lineage_invalid(f"relation {describe(relation)} is outside the vocabulary")
        assert isinstance(source, str)
        seen.setdefault((source, relation, _target(relation, item)), None)
    return list(seen)


# ------------------------------------------------------------------ late graph checks and resolution


def _acyclic(nodes: Iterable[str], edges: list[tuple[str, str]]) -> bool:
    """Kahn's algorithm, iterative."""
    indegree = dict.fromkeys(nodes, 0)
    outgoing: dict[str, list[str]] = {node: [] for node in indegree}
    for source, target in edges:
        outgoing[source].append(target)
        indegree[target] += 1
    ready = [node for node, degree in indegree.items() if degree == 0]
    removed = 0
    while ready:
        node = ready.pop()
        removed += 1
        for target in outgoing[node]:
            indegree[target] -= 1
            if indegree[target] == 0:
                ready.append(target)
    return removed == len(indegree)


def _relation_value(relation: _Relation) -> dict[str, Value]:
    source, kind, target = relation
    out: dict[str, Value] = {"from_record_id": source, "relation": kind}
    if target[0] == "record":
        out["to_record_id"] = target[1]
    elif target[0] == "versioned":
        out["to_artifact"] = {"stable_id": target[1], "artifact_version": target[2]}
    else:
        out["to_artifact"] = {"content_hash": target[1]}
    return out


def project_history(reader: object, stream: object, relations: object) -> dict[str, Any]:
    """PROJ-001 (G-067, G-070): the one deterministic, fail-safe projection of a history stream."""
    values = validate_arguments({"reader": reader, "stream": stream, "relations": relations})
    try:
        parsed_reader = _reader_rule(values["reader"])
    except MnemeValidationError as error:
        raise error.for_argument("reader") from None
    try:
        store_id, entries = _stream_rule(values["stream"])
    except MnemeValidationError as error:
        raise error.for_argument("stream") from None
    try:
        lineage = _relations_rule(values["relations"])
    except MnemeValidationError as error:
        raise error.for_argument("relations") from None

    # Step 3: entries in position order; the first failing entry fails the projection.
    records: list[dict[str, Value]] = []
    first: dict[str, tuple[int, bytes]] = {}
    skipped: list[int] = []
    excluded: list[int] = []
    duplicates: list[dict[str, Value]] = []
    for position, record in entries:
        try:
            admission = _admit(parsed_reader, record)
        except MnemeValidationError as error:
            raise error.for_argument("stream").at_position(position) from None
        except _UnknownRequired as stop:
            raise MnemeValidationError("history.unknown_required", str(stop), position=position) from None
        if admission.outcome == "skip":
            skipped.append(position)
            continue
        if admission.outcome == "excluded":
            excluded.append(position)
            continue
        envelope = admission.envelope
        assert envelope is not None
        record_id = envelope["record_id"]
        assert isinstance(record_id, str)
        content = canonical_bytes(envelope)
        if record_id in first:
            if first[record_id][1] != content:
                raise MnemeValidationError(
                    "history.duplicate_record_id",
                    f"{record_id} reused with different content (G-073)",
                    "stream",
                    position,
                )
            duplicates.append({"position": position, "record_id": record_id})
            continue
        first[record_id] = (position, content)
        records.append(envelope)

    # Step 4: late graph checks (G-071, G-072).
    available = {rid: pos for rid, (pos, _) in first.items()}
    resolution_edges: list[tuple[str, str]] = []
    derivation_edges: list[tuple[str, str]] = []
    unresolved: list[_Relation] = []
    for relation in lineage:
        source, kind, target = relation
        if source not in available:
            raise _lineage_invalid(f"from_record_id {source} is not available").for_argument("relations")
        if target[0] == "record" and target[1] == source:
            raise _lineage_invalid(f"self-link on {source} (G-071)").for_argument("relations")
        if kind in RESOLUTION:
            if target[1] not in available:
                raise _lineage_invalid(f"{kind} target {target[1]} is not available (G-072)").for_argument(
                    "relations"
                )
            resolution_edges.append((source, target[1]))
        elif target[0] == "record" and target[1] in available:
            derivation_edges.append((source, target[1]))
        else:
            unresolved.append(relation)
    if not _acyclic(available, resolution_edges) or not _acyclic(available, derivation_edges):
        raise _lineage_invalid("a lineage cycle (G-071)").for_argument("relations")

    # Step 5: resolution (G-069, G-074). Position order is presentation only, never selection.
    replaced = {target[1] for _, kind, target in lineage if kind in REPLACING}
    current = [rid for rid in available if rid not in replaced]
    current_set = set(current)
    interpreters: dict[str, list[str]] = {}
    for source, kind, target in lineage:
        if kind == "reinterprets":
            interpreters.setdefault(target[1], []).append(source)
    interpretations: list[Value] = [
        {
            "observation": obs,
            "current": [rid for rid in available if rid in current_set and rid in interpreters[obs]],
        }
        for obs in sorted(interpreters, key=available.__getitem__)
    ]

    def order(relation: _Relation) -> tuple[Any, ...]:
        source, kind, target = relation
        if target[0] == "record":
            key: tuple[Any, ...] = (0, available[target[1]]) if target[1] in available else (1, target[1])
        else:
            key = (2, target[1], target[2]) if target[0] == "versioned" else (3, target[1])
        return (available[source], kind, key)

    through = entries[-1][0] if entries else 0
    return {
        "through": {"store_id": store_id, "position": through},
        "records": records,
        "skipped": skipped,
        "excluded": excluded,
        "duplicates": duplicates,
        "relations": [_relation_value(r) for r in sorted(lineage, key=order)],
        "unresolved_derivations": [_relation_value(r) for r in sorted(unresolved, key=order)],
        "current": current,
        "interpretations": interpretations,
    }
