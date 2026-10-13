//! Stage-3 identity, edition and revision rules from CORE-001/002/003/012.

use crate::values::{Node, serialize};
use crate::{Error, ErrorCode, Input, Value, validate_arguments};
use std::cmp::Ordering;

pub(crate) fn staged(arguments: &[(&str, Input<'_>)]) -> Result<Vec<Value>, Error> {
    validate_arguments(arguments)
}
pub(crate) fn typed<T>(
    values: &[Value],
    names: &[&str],
    rule: impl Fn(&Value) -> Result<T, Error>,
) -> Result<Vec<T>, Error> {
    values
        .iter()
        .zip(names)
        .map(|(v, name)| rule(v).map_err(|e| e.named(name, names.len() > 1)))
        .collect()
}

pub(crate) fn id_rule(value: &Value) -> Result<String, Error> {
    let text = value
        .as_str()
        .ok_or_else(|| Error::new(ErrorCode::IdInvalid))?;
    let bytes = text.as_bytes();
    let alnum = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    let basename = text.split('.').next().unwrap_or("");
    let device = matches!(basename, "con" | "prn" | "aux" | "nul")
        || ((basename.starts_with("com") || basename.starts_with("lpt"))
            && basename.len() == 4
            && matches!(basename.as_bytes()[3], b'1'..=b'9'));
    if !(1..=128).contains(&bytes.len())
        || !alnum(bytes[0])
        || bytes.last() == Some(&b'.')
        || !bytes
            .iter()
            .all(|c| alnum(*c) || matches!(c, b'.' | b'_' | b'-'))
        || device
    {
        return Err(Error::new(ErrorCode::IdInvalid));
    }
    Ok(text.to_owned())
}

/// Validate a CORE-001 stable ID after both generic stages.
pub fn validate_stable_id(input: Input<'_>) -> Result<String, Error> {
    id_rule(&staged(&[("id", input)])?[0])
}

fn version_rule(value: &Value) -> Result<i128, Error> {
    value
        .as_integer()
        .filter(|n| *n > 0)
        .ok_or_else(|| Error::new(ErrorCode::VersionInvalid))
}

/// Validate a positive numeric edition, after generic normalization.
pub fn validate_version(input: Input<'_>) -> Result<i128, Error> {
    version_rule(&staged(&[("version", input)])?[0])
}

/// Compare numeric editions; all generic stages precede every typed rule.
pub fn compare_versions(a: Input<'_>, b: Input<'_>) -> Result<Ordering, Error> {
    let values = staged(&[("a", a), ("b", b)])?;
    let versions = typed(&values, &["a", "b"], version_rule)?;
    Ok(versions[0].cmp(&versions[1]))
}

/// A namespaced, exact operation identity (G-032/G-033); no persistence behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationIdentity {
    /// Producer uniqueness domain, in the stable-ID token domain.
    pub source_namespace: String,
    /// Exact opaque producer ID, 1–256 UTF-8 bytes.
    pub source_operation_id: String,
}

pub(crate) fn operation_rule(value: &Value) -> Result<OperationIdentity, Error> {
    let invalid = || Error::new(ErrorCode::OperationIdentityInvalid);
    let Node::Object(members) = value.node() else {
        return Err(invalid());
    };
    if members.len() != 2
        || members[0].0 != "source_namespace"
        || members[1].0 != "source_operation_id"
    {
        return Err(invalid());
    }
    let namespace = id_rule(&value.subtree(members[0].1)).map_err(|_| invalid())?;
    let Node::String(operation) = &value.nodes[members[1].1] else {
        return Err(invalid());
    };
    if !(1..=256).contains(&operation.len()) {
        return Err(invalid());
    }
    Ok(OperationIdentity {
        source_namespace: namespace,
        source_operation_id: operation.clone(),
    })
}

/// Validate the closed namespaced operation-identity pair.
pub fn operation_identity(input: Input<'_>) -> Result<OperationIdentity, Error> {
    operation_rule(&staged(&[("identity", input)])?[0])
}

/// Exact pair equality; no normalization, concatenation or case folding.
pub fn compare_operation_identities(a: Input<'_>, b: Input<'_>) -> Result<bool, Error> {
    let values = staged(&[("a", a), ("b", b)])?;
    let ids = typed(&values, &["a", "b"], operation_rule)?;
    Ok(ids[0] == ids[1])
}

/// Non-conflicting CORE-003 outcomes; conflict is an error, not an overwrite policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionComparison {
    /// Same immutable revision and canonical content.
    Identical,
    /// Different stable ID or artifact edition.
    Distinct,
    /// At least one completely valid record lacks an artifact version.
    NotApplicable,
}

struct Revision {
    id: String,
    version: Option<i128>,
    projection: usize,
}

fn revision_rule(value: &Value) -> Result<Revision, Error> {
    let Node::Object(members) = value.node() else {
        return Err(Error::new(ErrorCode::RevisionInvalid));
    };
    let find = |key| {
        members
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, i)| *i)
    };
    if find("stable_id").is_none()
        || find("projection").is_none()
        || members.iter().any(|(name, _)| {
            !matches!(
                name.as_str(),
                "stable_id" | "projection" | "artifact_version"
            )
        })
    {
        return Err(Error::new(ErrorCode::RevisionInvalid));
    }
    // Canonical typed-field order is artifact_version before stable_id, after the complete shape.
    let version = find("artifact_version")
        .map(|i| version_rule(&value.subtree(i)))
        .transpose()?;
    let id = id_rule(&value.subtree(find("stable_id").expect("checked member")))?;
    Ok(Revision {
        id,
        version,
        projection: find("projection").expect("checked member"),
    })
}

/// Compare closed revision records using their supplied semantic-content projections.
pub fn compare_artifact_revisions(a: Input<'_>, b: Input<'_>) -> Result<RevisionComparison, Error> {
    let values = staged(&[("a", a), ("b", b)])?;
    let revisions = typed(&values, &["a", "b"], revision_rule)?;
    let (a, b) = (&revisions[0], &revisions[1]);
    if a.version.is_none() || b.version.is_none() {
        return Ok(RevisionComparison::NotApplicable);
    }
    if a.id != b.id || a.version != b.version {
        return Ok(RevisionComparison::Distinct);
    }
    if serialize(&values[0], a.projection) == serialize(&values[1], b.projection) {
        return Ok(RevisionComparison::Identical);
    }
    Err(Error::new(ErrorCode::RevisionConflict))
}
