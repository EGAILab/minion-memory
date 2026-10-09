# minion-memory-rust

**Owner: Codex.** Claude does not create or modify Rust code here (process/roles/claude-role.md).

This directory holds only this ownership README; no Cargo workspace exists yet. The Cargo workspace, crate layout, pinned toolchain (`rust-toolchain.toml`) and
the thin canonical runner are created by Codex in its first `RUST_IMPLEMENTATION` pass, against the
accepted shared contract and `../conformance/`, never against the Python implementation. Toolchain
proposal for Codex to confirm or counter: `../../minion-memory-docs/plans/toolchain-proposal.md`.
OWN-001: `CARGO_TARGET_DIR` under the project-root `.tmp/cache/cargo-target/<platform>`.
