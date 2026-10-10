//! HIST-002/005/010 and SER-005/006: gated envelope admission and store-local cursors.

use crate::identity::{id_rule, staged, typed};
use crate::time::timestamp_rule;
use crate::values::Node;
use crate::{Error, ErrorCode, Input, Value};
use std::cmp::Ordering;
use std::collections::HashSet;

/// The three successful replay decisions. Only a recognized record returns an envelope.
#[derive(Debug, Clone)]
pub enum HistoryAdmission {
    /// A same-profile, recognized envelope, with normalized timestamps and values.
    Decode(Value),
    /// An admitted profile's unknown, explicitly ignorable schema.
    Skip,
    /// A different profile, excluded before consulting compatibility or typed payloads.
    Excluded,
}

pub(crate) fn object(
    value: &Value,
    index: usize,
    code: ErrorCode,
) -> Result<&[(String, usize)], Error> {
    match &value.nodes[index] {
        Node::Object(members) => Ok(members),
        _ => Err(Error::new(code)),
    }
}

fn member(members: &[(String, usize)], name: &str, code: ErrorCode) -> Result<usize, Error> {
    members
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, i)| *i)
        .ok_or_else(|| Error::new(code))
}

pub(crate) fn text(value: &Value, index: usize) -> Option<&str> {
    match &value.nodes[index] {
        Node::String(s) => Some(s),
        _ => None,
    }
}

pub(crate) fn token(value: &Value, index: usize, code: ErrorCode) -> Result<String, Error> {
    // Only the scalar is copied: validating one field must not repeatedly clone a large payload.
    let s = text(value, index).ok_or_else(|| Error::new(code))?;
    id_rule(&Value::string(s)).map_err(|_| Error::new(code))
}

fn integer(value: &Value, index: usize, minimum: i128, code: ErrorCode) -> Result<i128, Error> {
    // Generic stage 2 has already enforced the safe upper bound and normalized integral floats.
    match value.nodes[index] {
        Node::Integer(n) if n >= minimum => Ok(n),
        _ => Err(Error::new(code)),
    }
}

struct Reader {
    profile: String,
    recognized: HashSet<(String, i128)>,
}

fn reader_rule(value: &Value) -> Result<Reader, Error> {
    let code = ErrorCode::HistoryReaderInvalid;
    let members = object(value, value.root, code)?;
    if members.len() != 2 {
        return Err(Error::new(code));
    }
    let profile = token(value, member(members, "profile", code)?, code)?;
    let Node::Array(entries) = &value.nodes[member(members, "recognized", code)?] else {
        return Err(Error::new(code));
    };
    let mut recognized = HashSet::new();
    for index in entries {
        let entry = object(value, *index, code)?;
        if entry.len() != 2 {
            return Err(Error::new(code));
        }
        let kind = token(value, member(entry, "record_type", code)?, code)?;
        let edition = integer(value, member(entry, "schema_version", code)?, 1, code)?;
        if !recognized.insert((kind, edition)) {
            return Err(Error::new(code)); // G-053: normalized aliases are duplicates, not deduplication.
        }
    }
    Ok(Reader {
        profile,
        recognized,
    })
}

fn envelope_rule(reader: &Reader, mut value: Value) -> Result<HistoryAdmission, Error> {
    let code = ErrorCode::HistoryEnvelopeInvalid;
    let members = object(&value, value.root, code)?;
    // Gate 1: validate the entire routing triple, even on foreign-profile records.
    let profile = token(&value, member(members, "profile", code)?, code)?;
    let kind = token(&value, member(members, "record_type", code)?, code)?;
    let edition = integer(&value, member(members, "schema_version", code)?, 1, code)?;
    // Gate 2: profile admission precedes compatibility; recognition precedes closed shape.
    if profile != reader.profile {
        return Ok(HistoryAdmission::Excluded);
    }
    let compatibility = text(&value, member(members, "compatibility", code)?);
    if !matches!(compatibility, Some("required" | "ignorable")) {
        return Err(Error::new(code));
    }
    if !reader.recognized.contains(&(kind, edition)) {
        return if compatibility == Some("ignorable") {
            Ok(HistoryAdmission::Skip)
        } else {
            Err(Error::new(ErrorCode::HistoryUnknownRequired))
        };
    }
    // Gate 3: the recognized envelope is closed, before any remaining typed field.
    member(members, "record_id", code)?;
    member(members, "recorded_at", code)?;
    if members.iter().any(|(name, _)| {
        !matches!(
            name.as_str(),
            "record_id"
                | "profile"
                | "record_type"
                | "schema_version"
                | "compatibility"
                | "recorded_at"
                | "occurred_at"
                | "payload"
                | "provenance"
        )
    }) {
        return Err(Error::new(code));
    }
    // Gate 4: structural normalization has sorted these members by UTF-16 code units.
    // Never use JSON source order for typed errors (WP02-CR-006's discriminating seam).
    let mut timestamps = Vec::new();
    for (name, index) in members {
        match name.as_str() {
            "occurred_at" | "recorded_at" => {
                let s =
                    text(&value, *index).ok_or_else(|| Error::new(ErrorCode::TimestampInvalid))?;
                timestamps.push((*index, timestamp_rule(&Value::string(s))?));
            }
            "record_id" => {
                token(&value, *index, ErrorCode::IdInvalid)?;
            }
            _ => {} // payload/provenance were normalized generically, never profile-decoded here.
        }
    }
    for (index, timestamp) in timestamps {
        value.nodes[index] = Node::String(timestamp);
    }
    Ok(HistoryAdmission::Decode(value))
}

/// Admit one history envelope through G-055's four gates, after generic stages on both inputs.
/// Validation errors name `reader` or `record`; `history.unknown_required` is an unnamed replay stop.
pub fn admit_history_record(
    reader: Input<'_>,
    record: Input<'_>,
) -> Result<HistoryAdmission, Error> {
    let mut values = staged(&[("reader", reader), ("record", record)])?;
    let reader = reader_rule(&values[0]).map_err(|e| e.named("reader", true))?;
    envelope_rule(&reader, values.pop().expect("two staged arguments")).map_err(|e| {
        if e.code == ErrorCode::HistoryUnknownRequired {
            e
        } else {
            e.named("record", true)
        }
    })
}

#[derive(PartialEq, Eq)]
struct Boundary {
    store_id: String,
    position: i128,
}

fn boundary_rule(value: &Value) -> Result<Boundary, Error> {
    let code = ErrorCode::HistoryBoundaryInvalid;
    let members = object(value, value.root, code)?;
    if members.len() != 2 {
        return Err(Error::new(code));
    }
    let position = integer(value, member(members, "position", code)?, 0, code)?;
    let store_id = token(value, member(members, "store_id", code)?, code)?;
    Ok(Boundary { store_id, position })
}

/// Compare valid boundaries by position in one store; different stores are incomparable.
pub fn compare_history_boundaries(a: Input<'_>, b: Input<'_>) -> Result<Ordering, Error> {
    let values = staged(&[("a", a), ("b", b)])?;
    let boundaries = typed(&values, &["a", "b"], boundary_rule)?;
    if boundaries[0].store_id != boundaries[1].store_id {
        return Err(Error::new(ErrorCode::HistoryBoundaryIncomparable));
    }
    Ok(boundaries[0].position.cmp(&boundaries[1].position))
}

/// Exact pair equality, including store identity; cross-store equality is false, not an error.
pub fn history_boundaries_equal(a: Input<'_>, b: Input<'_>) -> Result<bool, Error> {
    let values = staged(&[("a", a), ("b", b)])?;
    let boundaries = typed(&values, &["a", "b"], boundary_rule)?;
    Ok(boundaries[0] == boundaries[1])
}
