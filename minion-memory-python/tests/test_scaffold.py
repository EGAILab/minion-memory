"""Scaffold checks: the package root exposes no behavior; layers live in explicit namespaces (ARCH-003)."""

import minion_memory


def test_package_imports_without_behavior() -> None:
    # Layer 01 (WP-01) adds `minion_memory.core`; nothing else may appear at the package root.
    public = {name for name in dir(minion_memory) if not name.startswith("_")}
    assert public <= {"core"}, f"unexpected public API at the package root: {public}"
    assert minion_memory.__version__ == "0.0.0"
