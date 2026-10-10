"""Thin canonical-conformance runner (Python side).

Parse → construct public values → invoke the real library → normalize → compare. It never
implements semantics. In WP-00 no operation vocabulary exists, so:

* a scenario containing any `TO_BE_` value is reported xfail (conformance/README.md), detected
  recursively from the document itself — never from a list of names;
* any other scenario fails loudly: an ACTIVE scenario without an implemented operation vocabulary
  is a gap, not a skip.

Loading uses `yaml.safe_load`. That is sound only because `mneme-process check conformance`
guarantees every corpus document reads identically under YAML 1.1 and the YAML-1.2-core profile;
the runner relies on that gate rather than re-implementing it.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import jsonschema
import pytest
import yaml

CORPUS = Path(__file__).resolve().parents[3] / "conformance"
FAMILIES = ("core", "learning", "agent")


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
        # Contract under review: no implementation may exist before the layer's contract checkpoint.
        layer, sid = scenario["layer"], scenario["id"]
        pytest.xfail(f"DRAFT scenario, layer {layer} contract not yet reviewed ({sid})")
    pytest.fail(f"no operation vocabulary implemented for layer {scenario['layer']} ({scenario['id']})")
