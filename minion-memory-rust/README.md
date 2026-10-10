# minion-memory-rust — Layers 01–02

**Owner: Codex.** Implements the accepted shared contract and unchanged canonical corpus;
the Python implementation is not an oracle. Workspace/package `minion-memory`, public crate
`minion_memory`, Rust **1.97.1** pinned in `rust-toolchain.toml`, dependencies pinned in
`Cargo.toml` and `Cargo.lock`. Layer 02 includes history admission, store-local boundary relations and MemoryScope.
No persistence or Layer 03+ behavior.

## Public seams

`Value` is an owned JSON-shaped tree with a flat arena. Construct native nulls, booleans,
exact `i128` integers, binary64 numbers, strings, arrays and objects through its constructors.
`array(Vec<Value>)` and `array_from_slice(&[Value])` bind Vec, slices and arrays in element order.
Objects retain duplicate names until validation. Negative zero and whole-number floats normalize
at the seam; exact native integers are checked without binary64 rounding.

`Input::Native`, `Input::Text` and `Input::Bytes` make the native/JSON-text/UTF-8-byte boundary
explicit. `decode_text`/`decode_json` own JSON decoding. `validate_arguments` runs each generic
stage across every argument before the next stage. The comparison primitives declare `a`, then
`b`; their validation errors carry that argument name. `ErrorCode::as_str()` exposes the stable
contract vocabulary; diagnostic detail is bounded and never formats an input value.

`validate_value`, `canonical_bytes`, `content_hash` and `raw_content_hash` provide SER-008/009
and CORE-011. Typed operations are `validate_stable_id`, `validate_version`, `compare_versions`,
`operation_identity`, `compare_operation_identities`, `compare_artifact_revisions` and
`normalize_timestamp`. A revision conflict is an error, never an overwrite result.

Rust strings cannot hold surrogates. This owned tree cannot contain cycles, non-string keys or
arbitrary host objects; those are prevented by the binding's types. There are no trait-based
conversion callbacks, subclasses or dynamic argument maps at the validation seam. Duplicate
names and invalid numeric values remain constructible and are rejected by validation.

The iterative, duplicate-aware decoder keeps numeric tokens uninterpreted until all structural
decoding succeeds. It selects JSON errors in source order, including invalid UTF-8, and then
validates numbers in canonical traversal order. Native traversal, serialization, clone, debug
and drop also avoid recursive tree descent. Memory allocation remains bounded by available memory;
no contract depth limit is added. Native construction by repeatedly wrapping existing arrays
copies arenas; use the decoder for large textual trees.

Canonical serialization explicitly sorts UTF-16 units and escapes strings per RFC 8785.
`ryu-js` supplies ECMAScript binary64 formatting; `sha2` hashes bytes. `chrono` is used only for
checked calendar arithmetic after the HIST-004 ASCII grammar and year bounds have passed.
`serde_jcs` is not used. `serde_json`, YAML, regex and JSON Schema dependencies are test-only.

## Gates and canonical runner

Source `minion-memory-docs/process/environment/project-environment.ps1` (or `.sh`) before each
shell. Set `CARGO_HOME` and `CARGO_TARGET_DIR` to project-local `.tmp/cache/` directories.
Keep Rustup's home project-local too when installing a toolchain. With the pinned toolchain:

```text
cargo fmt --check
cargo clippy --locked --offline --workspace --all-targets -- -D warnings
cargo test --locked --offline -- --nocapture
cargo doc --locked --offline --no-deps
```

Set `RUSTDOCFLAGS=-D warnings` for the documentation gate. Fetch locked dependencies before
offline gates. On Windows an already-installed compiler outside the project may be executed;
its outputs and all caches must still stay inside the project.

`tests/conformance.rs` discovers the unchanged `../conformance/{core,learning,agent}` corpus,
loads it using an equivalent portable YAML profile, validates each envelope with its shared
Draft 2020-12 schema, and calls public operations. It performs no canonicalization or repair.
It reports the discovered ACTIVE scenario/step counts and explicit expected failures; this Layer 01
and Layer 02 candidate has 15 ACTIVE scenarios / 289 steps and 5 recursive `TO_BE_` expected failures.
Every DRAFT scenario reports xfail with its layer and the corpus rule as the reason. Drafts never
execute as evidence. The Rust runner declares `IMPLEMENTED_LAYER = 2`: ACTIVE scenarios above
Layer 02 report xfail naming their layer, Rust and the Implemented layers rule. This declaration
moves only with a Rust implementation candidate. At or below the declared layer, promotion to
ACTIVE executes every step and fails on unsupported operations or wrong results. Counts derive
from dispatch, not a pinned inventory. Unknown eligible ACTIVE operations fail. Native scenario integers are constructed exactly as Rust `i128`;
JSON-text inputs are handed to the library untouched.

`tests/layer01.rs` covers source/stage/canonical precedence, native values, typed rules, timestamps,
deep trees and realistic known-bad controls. `tests/vectors/` contains an independently generated
ECMAScript number oracle and its reproducible generator. No Node or network is needed at test time.


## Layer 02 operations

`admit_history_record(reader, record)` takes two explicit `Input` seams. Generic
stages run across both before typed rules. The closed reader validates its exact
profile and duplicate-free recognized `(record_type, schema_version)` pairs. The
record passes the four accepted gates: routing triple, profile/compatibility/recognition,
closed recognized shape, then typed fields in canonical order. Only HIST-004 timestamps
are rewritten; normalized payload and provenance survive unchanged.

`HistoryAdmission::Decode(Value)` carries the admitted envelope;
`Skip` identifies unknown ignorable schemas and `Excluded` identifies foreign
profiles. Unknown required schemas return the unnamed `history.unknown_required`
error. All argument-validation errors name `reader` or `record`.

`compare_history_boundaries(a, b)` returns `std::cmp::Ordering` only for a common
store, otherwise unnamed `history.boundary_incomparable`. `history_boundaries_equal`
compares the complete pair, returning false for different stores. Position zero is
valid and precedes the first physical record. Neither operation implements persistence,
range reads or a global/causal/cross-device order.

`validate_scope`, `scopes_equal` and `scope_visible` accept the same explicit seams.
A scope is the exact resolved-with-facets or unresolved record; unresolved is distinct
from global. G-064 facet names are not paths or CORE-001 values. `AGENT_FACETS` is the
six-name constant vocabulary. Stored unknown names survive validation but hide records;
unknown query names are rejected, including for otherwise hidden records. Matching
uses every explicitly declared record constraint, with no inferred hierarchy or
wildcard for query omissions. Capture, retrieval absence and Learning adoption remain
at their owning later layers.

`tests/layer02.rs` supplies independent public-seam controls, including all typed-field
source permutations, generic-stage precedence, a 20,000-level payload and 46,656
six-facet matching cases. No Python outputs are used as expectations.
