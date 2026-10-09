# minion-memory

Implementation repository for **Minion-Memory**, the Project Mneme durable memory engine: two
independent implementations (Python, Rust) of one shared contract, proven by two profiles — Human
Learning (Flashcard App) and Agent Memory (Minion-Agent).

> **State:** WP-00 bootstrap. **No memory behavior is implemented.** The contract is a normalized
> DRAFT awaiting independent review (`../minion-memory-docs/process/state/WP-00.yaml`).

```text
minion-memory/
├── mneme-contract-manifest.yaml   traceability: requirement -> source -> spec -> scenarios -> locations
├── schemas/                       JSON Schemas for shared artifacts (manifest)
├── conformance/                   ONE canonical corpus, consumed by both runners
│   ├── schema/                    scenario envelope schema (support, not a family)
│   ├── core/  learning/  agent/
├── minion-memory-python/          Claude-owned Python implementation (scaffold only)
└── minion-memory-rust/            Codex-owned; ownership README only, Cargo workspace not created yet
```

- Semantics come from the authority bundle via `../minion-memory-docs/spec/**`, never from either
  implementation. Python is not Rust's oracle; Rust is not Python's.
- The manifest is traceability, not authority. Implementation/certification status lives in
  `../minion-memory-docs/process/certification-registry.yaml`.
- Runners are thin: parse → construct public values → call the real implementation → normalize →
  compare. They never implement semantics (`conformance/README.md`).
- OWN-001: all temp/cache/build output under `E:\AI\Projects\OpenMinds\Minions\Minion-Memory\.tmp`
  (`../minion-memory-docs/process/environment/`).

## Checks

```sh
. ../minion-memory-docs/process/environment/project-environment.sh
(cd ../minion-memory-docs/process/tools && UV_PROJECT_ENVIRONMENT=$MNEME_VENV_ROOT/mneme-process \
   uv run mneme-process check --docs ../.. --code ../../../minion-memory)
(cd minion-memory-python && UV_PROJECT_ENVIRONMENT=$MNEME_VENV_ROOT/minion-memory-python uv run pytest)
```
