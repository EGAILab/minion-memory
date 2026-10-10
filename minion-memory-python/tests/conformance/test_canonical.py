"""Thin canonical-conformance runner (Python side).

Parse → construct public values → invoke the real library → normalize the observation → compare. It
never implements semantics: every operation maps to one public `minion_memory.core` function, and a
rejection is observed only through `MnemeValidationError.code` (and `.argument` where a scenario pins
it).

* a scenario containing any `TO_BE_` value is reported xfail (conformance/README.md), detected
  recursively from the document itself — never from a list of names;
* a DRAFT scenario is reported xfail (its layer's contract is not yet reviewed);
* an ACTIVE scenario above `IMPLEMENTED_LAYER` is reported xfail: that layer is not yet implemented
  in Python (conformance/README.md, implemented-layer rule);
* an ACTIVE scenario must pass, and an ACTIVE scenario using an operation this runner does not know
  fails loudly: a missing operation is a gap, not a skip.

Loading uses `yaml.safe_load`. That is sound only because `mneme-process check conformance`
guarantees every corpus document reads identically under YAML 1.1 and the YAML-1.2-core profile;
the runner relies on that gate rather than re-implementing it.
"""

from __future__ import annotations

import hashlib
import json
from collections.abc import Callable
from pathlib import Path
from typing import Any

import jsonschema
import pytest
import yaml

from minion_memory import core

CORPUS = Path(__file__).resolve().parents[3] / "conformance"
FAMILIES = ("core", "learning", "agent")
# Highest layer the Python implementation covers; it moves only with an implementation candidate.
IMPLEMENTED_LAYER = 2


def _scenario_files() -> list[Path]:
    return sorted(p for family in FAMILIES for p in (CORPUS / family).rglob("*.yaml"))


def _has_placeholder(value: Any) -> bool:
    if isinstance(value, str):
        return value.startswith("TO_BE_")
    if isinstance(value, dict):
        return any(_has_placeholder(v) for v in value.values())
    if isinstance(value, list):
        return any(_has_placeholder(v) for v in value)
    return False


def _input(params: dict[str, Any], native: str, text: str) -> object:
    """A step input given either as a native YAML value or as JSON text. JSON text is handed to the
    library as `core.JsonText`, so the library alone applies stage precedence across arguments."""
    return core.JsonText(params[text]) if text in params else params[native]


def _canonicalize(params: dict[str, Any]) -> dict[str, Any]:
    value = core.decode_json(params["value_json"])
    raw = core.canonical_bytes(value)
    assert core.content_hash(value) == hashlib.sha256(raw).hexdigest()
    return {"canonical": raw.decode("utf-8"), "sha256": core.content_hash(value)}


def _admission(admission: core.HistoryAdmission) -> dict[str, Any]:
    observed: dict[str, Any] = {"outcome": admission.outcome}
    if admission.envelope is not None:
        observed["envelope"] = admission.envelope
    return observed


OPERATIONS: dict[str, Callable[[dict[str, Any]], dict[str, Any]]] = {
    "canonicalize": _canonicalize,
    "compare_versions": lambda p: {"result": core.compare_versions(p["a"], p["b"])},
    "validate_stable_id": lambda p: (core.validate_stable_id(p["id"]), {"result": "valid"})[1],
    "normalize_timestamp": lambda p: {
        "canonical": core.normalize_timestamp(_input(p, "input", "value_json"))
    },
    "validate_operation_identity": lambda p: (
        core.operation_identity(core.JsonText(p["value_json"])),
        {"result": "valid"},
    )[1],
    "compare_operation_identities": lambda p: {
        "result": core.compare_operation_identities(core.JsonText(p["a_json"]), core.JsonText(p["b_json"]))
    },
    "hash_content": lambda p: {"sha256": core.raw_content_hash(bytes.fromhex(p["hex"]))},
    "compare_artifact_revisions": lambda p: {"result": core.compare_artifact_revisions(p["a"], p["b"])},
    "admit_history_record": lambda p: _admission(
        core.admit_history_record(p["reader"], _input(p, "record", "record_json"))
    ),
    "compare_history_boundaries": lambda p: {"result": core.compare_history_boundaries(p["a"], p["b"])},
    "history_boundaries_equal": lambda p: {"result": core.history_boundaries_equal(p["a"], p["b"])},
    "validate_scope": lambda p: {"result": core.validate_scope(p["scope"])},
    "scopes_equal": lambda p: {"result": core.scopes_equal(p["a"], p["b"])},
    "scope_visible": lambda p: {"result": core.scope_visible(p["record"], p["query"])},
}


def _observe(operation: str, params: dict[str, Any]) -> dict[str, Any]:
    try:
        return OPERATIONS[operation](params)
    except core.MnemeValidationError as error:
        observed: dict[str, Any] = {"error": error.code}
        if error.argument is not None:
            observed["argument"] = error.argument
        return observed


def test_corpus_is_discovered() -> None:
    assert _scenario_files(), f"no canonical scenarios found under {CORPUS}"


@pytest.mark.parametrize("path", _scenario_files(), ids=lambda p: p.relative_to(CORPUS).as_posix())
def test_canonical_scenario(path: Path) -> None:
    scenario = yaml.safe_load(path.read_text(encoding="utf-8"))
    schema = json.loads((CORPUS / "schema" / "scenario.schema.json").read_text(encoding="utf-8"))
    jsonschema.validate(scenario, schema)
    if _has_placeholder(scenario):
        pytest.xfail("TO_BE_ placeholder scenario (conformance/README.md)")
    if scenario["status"] == "DRAFT":
        layer, sid = scenario["layer"], scenario["id"]
        pytest.xfail(f"DRAFT scenario, layer {layer} contract not yet reviewed ({sid})")
    if int(scenario["layer"]) > IMPLEMENTED_LAYER:
        pytest.xfail(f"layer {scenario['layer']} not yet implemented in Python ({scenario['id']})")
    expected = scenario["expect"]["results"]
    assert len(expected) == len(scenario["when"]), "one expectation per step"
    failures = []
    for step, expect in zip(scenario["when"], expected, strict=True):
        ((operation, params),) = step.items()
        assert operation in OPERATIONS, (
            f"no operation vocabulary implemented for {operation!r} ({scenario['id']})"
        )
        observed = _observe(operation, params)
        wanted = {key: value for key, value in expect.items() if key != "case"}
        if "argument" not in wanted:
            observed.pop("argument", None)  # only compared where the scenario pins it
        if observed != wanted:
            failures.append(f"{params.get('case')}: expected {wanted}, observed {observed}")
    assert not failures, "\n".join(failures)
