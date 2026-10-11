"""Core primitives: Layer 01 (certified; docs 71e2b12e / code cc3c5d54) and Layer 02 history envelope
(agreed contract: docs 6f761081 / code 15b71ca3).

Public API: value validation and canonical bytes (SER-008, SER-009), stable IDs and versions
(CORE-001, CORE-002), revision comparison (CORE-003), operation identity (CORE-012) and timestamps
(HIST-004); Layer 02 history envelope admission and HistoryBoundary (HIST-002, HIST-005, SER-005,
SER-006); Layer 02 MemoryScope value and matching (SCOPE-001..006); Layer 03
HistoryProjector and lineage (PROJ-001, HIST-007). Every rejection raises
`MnemeValidationError` with one stable code (G-031).
"""

from .errors import MnemeValidationError
from .history import (
    HistoryAdmission,
    admit_history_record,
    compare_history_boundaries,
    history_boundaries_equal,
)
from .identity import (
    OperationIdentity,
    compare_artifact_revisions,
    compare_operation_identities,
    compare_versions,
    operation_identity,
    validate_stable_id,
    validate_version,
)
from .projection import project_history
from .scope import AGENT_FACETS, scope_visible, scopes_equal, validate_scope
from .time import normalize_timestamp
from .values import (
    JsonText,
    Value,
    canonical_bytes,
    content_hash,
    decode_json,
    raw_content_hash,
    validate_arguments,
    validate_value,
)

__all__ = [
    "AGENT_FACETS",
    "HistoryAdmission",
    "JsonText",
    "MnemeValidationError",
    "OperationIdentity",
    "Value",
    "admit_history_record",
    "canonical_bytes",
    "compare_artifact_revisions",
    "compare_history_boundaries",
    "compare_operation_identities",
    "compare_versions",
    "content_hash",
    "decode_json",
    "history_boundaries_equal",
    "normalize_timestamp",
    "operation_identity",
    "project_history",
    "raw_content_hash",
    "scope_visible",
    "scopes_equal",
    "validate_arguments",
    "validate_scope",
    "validate_stable_id",
    "validate_value",
    "validate_version",
]
