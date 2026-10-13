//! Stable contract errors; details are diagnostic, never an identity or oracle.

use std::fmt;

/// One error code selected by the ordered validation stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// Stage 1: malformed JSON or invalid UTF-8.
    InvalidJson,
    /// An unpaired JSON surrogate escape (stage 1).
    LoneSurrogate,
    /// Stage 1: duplicate decoded/base member names.
    DuplicateKey,
    /// A host kind outside the portable domain (reserved for bindings that admit such inputs).
    UnsupportedType,
    /// A cyclic host graph (not constructible in the Rust owned-tree binding).
    Cyclic,
    /// Stage 2: NaN, infinity or a numeral overflowing binary64.
    NumberNotFinite,
    /// Stage 2: an integral value outside the safe bound.
    IntegerOutOfRange,
    /// Stage 3: invalid stable ID.
    IdInvalid,
    /// Stage 3: invalid edition.
    VersionInvalid,
    /// Stage 3: invalid timestamp.
    TimestampInvalid,
    /// Stage 3: malformed operation identity.
    OperationIdentityInvalid,
    /// Stage 3: malformed revision record.
    RevisionInvalid,
    /// Different canonical content for the same immutable revision.
    RevisionConflict,
    /// Stage 3: invalid history reader configuration or duplicate recognized pairs.
    HistoryReaderInvalid,
    /// Stage 3: invalid gated history envelope.
    HistoryEnvelopeInvalid,
    /// An admitted, unrecognized schema marked required; replay must stop.
    HistoryUnknownRequired,
    /// Stage 3: malformed store-local boundary.
    HistoryBoundaryInvalid,
    /// Relation: valid boundaries from different stores cannot be ordered.
    HistoryBoundaryIncomparable,
    /// Stage 3: malformed MemoryScope.
    ScopeInvalid,
    /// Stage 3: malformed or unknown-facet allowed-scope query.
    ScopeQueryInvalid,
    /// Stage 3: malformed history stream or non-increasing entry positions.
    HistoryStreamInvalid,
    /// Early lineage shape or late endpoint/self-link/cycle failure.
    LineageInvalid,
    /// A later decoded occurrence reuses a record ID with different canonical content.
    HistoryDuplicateRecordId,
    /// Layer 04: records must be a nonempty array.
    PersistenceBatchInvalid,
    /// Layer 04: replay exclusion/skip/unknown is not an acceptable write.
    PersistenceRecordUnrecognized,
    /// Layer 04: page size outside integer 1..MAX.
    PersistenceLimitInvalid,
    /// Creating an existing store.
    PersistenceStoreExists,
    /// An operation names an absent store.
    PersistenceStoreUnknown,
    /// A committed operation identity is reused with different normalized drafts.
    PersistenceOperationConflict,
    /// An unseen batch does not fit after the tail (G-086).
    PersistenceStoreFull,
    /// The PostgreSQL environment or v1 catalog differs from the contract.
    PersistenceSchemaIncompatible,
}

impl ErrorCode {
    /// Language-independent spelling of the code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidJson => "value.invalid_json",
            Self::LoneSurrogate => "value.lone_surrogate",
            Self::DuplicateKey => "value.duplicate_key",
            Self::UnsupportedType => "value.unsupported_type",
            Self::Cyclic => "value.cyclic",
            Self::NumberNotFinite => "value.number_not_finite",
            Self::IntegerOutOfRange => "value.integer_out_of_range",
            Self::IdInvalid => "id.invalid",
            Self::VersionInvalid => "version.invalid",
            Self::TimestampInvalid => "timestamp.invalid",
            Self::OperationIdentityInvalid => "operation_identity.invalid",
            Self::RevisionInvalid => "artifact.revision_invalid",
            Self::RevisionConflict => "artifact.revision_conflict",
            Self::HistoryReaderInvalid => "history.reader_invalid",
            Self::HistoryEnvelopeInvalid => "history.envelope_invalid",
            Self::HistoryUnknownRequired => "history.unknown_required",
            Self::HistoryBoundaryInvalid => "history.boundary_invalid",
            Self::HistoryBoundaryIncomparable => "history.boundary_incomparable",
            Self::ScopeInvalid => "scope.invalid",
            Self::ScopeQueryInvalid => "scope.query_invalid",
            Self::HistoryStreamInvalid => "history.stream_invalid",
            Self::LineageInvalid => "lineage.invalid",
            Self::HistoryDuplicateRecordId => "history.duplicate_record_id",
            Self::PersistenceBatchInvalid => "persistence.batch_invalid",
            Self::PersistenceRecordUnrecognized => "persistence.record_unrecognized",
            Self::PersistenceLimitInvalid => "persistence.limit_invalid",
            Self::PersistenceStoreExists => "persistence.store_exists",
            Self::PersistenceStoreUnknown => "persistence.store_unknown",
            Self::PersistenceOperationConflict => "persistence.operation_conflict",
            Self::PersistenceStoreFull => "persistence.store_full",
            Self::PersistenceSchemaIncompatible => "persistence.schema_incompatible",
        }
    }
}

/// A stable code and, for multi-argument primitives, the failing argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// The contract rejection code.
    pub code: ErrorCode,
    /// Present only for errors within a multi-argument primitive's validation.
    pub argument: Option<String>,
    /// Bounded diagnostic context; not part of the shared equality contract.
    pub detail: String,
    /// Blocking store position for a projection entry failure; never a JSON byte offset.
    pub position: Option<i128>,
}

impl Error {
    pub(crate) fn new(code: ErrorCode) -> Self {
        Self {
            code,
            argument: None,
            detail: String::new(),
            position: None,
        }
    }

    pub(crate) fn at(mut self, position: usize) -> Self {
        self.detail = format!("at byte {position}");
        self
    }

    pub(crate) fn named(mut self, name: &str, several: bool) -> Self {
        if several {
            self.argument = Some(name.to_owned());
        }
        self
    }

    pub(crate) fn positioned(mut self, position: i128) -> Self {
        self.position = Some(position);
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code.as_str())?;
        if let Some(name) = &self.argument {
            write!(f, " (argument {name})")?;
        }
        if let Some(position) = self.position {
            write!(f, " (store position {position})")?;
        }
        if !self.detail.is_empty() {
            write!(f, ": {}", self.detail)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}
