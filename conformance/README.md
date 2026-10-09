# Canonical conformance corpus

One corpus, consumed unchanged by both language runners. Python and Rust never keep copies.

```text
conformance/
├── schema/      support infrastructure (not a behavior family)
├── core/        shared-core scenarios
├── learning/    Learning Profile scenarios
└── agent/       Agent Memory Profile scenarios
```

## Format (scenario_format 1)

One scenario per `.yaml` file, envelope fixed by `schema/scenario.schema.json`:

| Field | Rule |
|---|---|
| `id` | `<family>.<dotted-kebab-name>`; unique across the corpus; prefix equals `family` and the directory |
| `family` | `core` / `learning` / `agent` |
| `layer` | owning layer, two digits |
| `status` | `PLACEHOLDER` (structure only), `DRAFT` (under contract review), `ACTIVE` (normative evidence), `RETIRED` |
| `requirements` | ≥1 requirement ID; each must exist in `mneme-contract-manifest.yaml`, and the row must list this scenario back |
| `given` / `when` / `expect` | layer-defined operation vocabulary, written as calls on public library operations and observable results — never runner logic |
| `negative_controls` | realistic wrong implementations this scenario must kill |

**YAML profile (hazard H6).** Files are read through the YAML-1.2-core boundary
(`minion-memory-docs/process/tools`, `yamlcore.py`): JSON-domain values only; no anchors/aliases,
tags or duplicate keys; any scalar whose YAML 1.1 and 1.2 readings differ must be quoted (e.g.
`"yes"`, `"2026-10-09"`, `"010"`). Both runners must load with an equivalent profile.

**Placeholders.** Any string value beginning `TO_BE_` marks the scenario expected-failure. Both
runners detect it recursively from the document and report `xfail` with a reason referencing this
rule; they never skip silently or keep a name list. A scenario with status `ACTIVE` must contain no
`TO_BE_` value.

**Runner rule.** A runner parses, constructs public values, invokes the real implementation,
normalizes observable output and compares. It never implements semantics (ordering, idempotency,
scope resolution, consolidation, evidence inference, serialization repair, expected results).

## Current contents (WP-00)

Only `PLACEHOLDER` scenarios exist. They pin intended behaviors from upstream acceptance criteria
so the machinery (schema, uniqueness, manifest back-links, placeholder detection) is exercised.
Their operation vocabulary and expectations are defined by the owning layer's contract review.
