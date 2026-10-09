"""WP-00 scaffold checks: the package exists and exposes no memory behavior yet."""

import minion_memory


def test_package_imports_without_behavior() -> None:
    public = [name for name in dir(minion_memory) if not name.startswith("_")]
    assert public == [], f"WP-00 must not ship public API: {public}"
    assert minion_memory.__version__ == "0.0.0"
