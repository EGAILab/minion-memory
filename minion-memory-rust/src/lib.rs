//! Mneme Layers 01–03, implemented from the accepted contract and canonical corpus.
//!
//! All primitives validate structural inputs across every declared argument before
//! checking the generic value domain, then apply typed rules (SER-008/G-031).
//! JSON text uses a duplicate-aware, iterative decoder with deferred numbers.
//! No persistence or profile-specific projection semantics are implemented here.
//!
//! ```
//! use minion_memory::{decode_text, canonical_bytes, compare_versions, Input, Value};
//! let value = decode_text(r#"{"v":1.0}"#).unwrap();
//! assert_eq!(canonical_bytes(&value).unwrap(), br#"{"v":1}"#);
//! assert_eq!(compare_versions(Input::Text("2e0"), Input::Native(&Value::integer(2))).unwrap(),
//!            std::cmp::Ordering::Equal);
//! ```

mod errors;
mod history;
mod identity;
mod json;
mod projection;
mod scope;
mod time;
mod values;

pub use errors::{Error, ErrorCode};
pub use history::{
    HistoryAdmission, admit_history_record, compare_history_boundaries, history_boundaries_equal,
};
pub use identity::{
    OperationIdentity, RevisionComparison, compare_artifact_revisions,
    compare_operation_identities, compare_versions, operation_identity, validate_stable_id,
    validate_version,
};
pub use projection::project_history;
pub use scope::{AGENT_FACETS, scope_visible, scopes_equal, validate_scope};
pub use time::normalize_timestamp;
pub use values::{
    Input, SAFE_INTEGER_MAX, Value, canonical_bytes, content_hash, decode_json, decode_text,
    raw_content_hash, validate_arguments, validate_value,
};
