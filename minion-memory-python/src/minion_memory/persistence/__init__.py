"""Layer 04 canonical history store (WP-04): a backend-neutral contract (`spec/persistence.md`) on the
PostgreSQL reference backend with one shared schema (Owner G-079).

Public API: `PostgresHistoryStore.open(conninfo, clock)`, then `create_store`, `commit_history`,
`read_history` and `store_tail`. Every rejection raises `MnemeValidationError` with one stable code.
"""

from .postgres import (
    SCHEMA_OPEN_LOCK_KEY,
    SCHEMA_VERSION,
    Clock,
    ClockDefectError,
    PostgresHistoryStore,
)

__all__ = ["SCHEMA_OPEN_LOCK_KEY", "SCHEMA_VERSION", "Clock", "ClockDefectError", "PostgresHistoryStore"]
