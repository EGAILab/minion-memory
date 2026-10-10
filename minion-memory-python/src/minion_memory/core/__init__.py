"""Layer 01 core primitives (agreed contract checkpoint: docs 62adec5f / code 860e91c1).

Public API: value validation and canonical bytes (SER-008, SER-009), stable IDs and versions
(CORE-001, CORE-002), revision comparison (CORE-003), operation identity (CORE-012) and timestamps
(HIST-004). Every rejection raises `MnemeValidationError` with one stable code (G-031).
"""

from .errors import MnemeValidationError
from .identity import (
    OperationIdentity,
    compare_artifact_revisions,
    compare_operation_identities,
    compare_versions,
    operation_identity,
    validate_stable_id,
    validate_version,
)
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
    "JsonText",
    "MnemeValidationError",
    "OperationIdentity",
    "Value",
    "canonical_bytes",
    "compare_artifact_revisions",
    "compare_operation_identities",
    "compare_versions",
    "content_hash",
    "decode_json",
    "normalize_timestamp",
    "operation_identity",
    "raw_content_hash",
    "validate_arguments",
    "validate_stable_id",
    "validate_value",
    "validate_version",
]
