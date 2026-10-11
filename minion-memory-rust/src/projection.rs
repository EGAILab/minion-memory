//! Canonical HistoryProjector (HIST-006/007, PROJ-001/002; Owner G-066…G-075).

use std::collections::{BTreeSet, HashMap};

use crate::history::{envelope_rule, integer, member, object, reader_rule, text, token};
use crate::identity::staged;
use crate::values::Node;
use crate::{Error, ErrorCode, HistoryAdmission, Input, Value, canonical_bytes};

// Variant order is the contract's lexical relation-kind presentation order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Corrects,
    DerivedFrom,
    Reinterprets,
    Supersedes,
}
impl Kind {
    fn parse(value: Option<&str>) -> Result<Self, Error> {
        match value {
            Some("corrects") => Ok(Self::Corrects),
            Some("derived_from") => Ok(Self::DerivedFrom),
            Some("reinterprets") => Ok(Self::Reinterprets),
            Some("supersedes") => Ok(Self::Supersedes),
            _ => Err(Error::new(ErrorCode::LineageInvalid)),
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Corrects => "corrects",
            Self::DerivedFrom => "derived_from",
            Self::Reinterprets => "reinterprets",
            Self::Supersedes => "supersedes",
        }
    }
    fn resolution(self) -> bool {
        self != Self::DerivedFrom
    }
    fn replaces(self) -> bool {
        matches!(self, Self::Corrects | Self::Supersedes)
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Target {
    Record(String),
    Versioned(String, i128),
    Hash(String),
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Relation {
    source: String,
    kind: Kind,
    target: Target,
}

fn vobject(members: Vec<(&str, Value)>) -> Value {
    Value::object(
        members
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect(),
    )
}

fn stream_rule(value: &Value) -> Result<(String, Vec<(i128, usize)>), Error> {
    let code = ErrorCode::HistoryStreamInvalid;
    let members = object(value, value.root, code)?;
    if members.len() != 2 {
        return Err(Error::new(code));
    }
    let store = token(value, member(members, "store_id", code)?, code)?;
    let Node::Array(entries) = &value.nodes[member(members, "entries", code)?] else {
        return Err(Error::new(code));
    };
    let mut result = Vec::with_capacity(entries.len());
    let mut previous = 0;
    for index in entries {
        let entry = object(value, *index, code)?;
        if entry.len() != 2 {
            return Err(Error::new(code));
        }
        let position = integer(value, member(entry, "position", code)?, 1, code)?;
        if position <= previous {
            return Err(Error::new(code));
        }
        previous = position;
        result.push((position, member(entry, "record", code)?));
    }
    Ok((store, result))
}

fn target_rule(value: &Value, members: &[(String, usize)], kind: Kind) -> Result<Target, Error> {
    let code = ErrorCode::LineageInvalid;
    if members.len() != 3 {
        return Err(Error::new(code));
    }
    if let Ok(index) = member(members, "to_record_id", code) {
        return Ok(Target::Record(token(value, index, code)?));
    }
    let index = member(members, "to_artifact", code)?;
    if kind != Kind::DerivedFrom {
        return Err(Error::new(code));
    }
    let artifact = object(value, index, code)?;
    if artifact.len() == 2 {
        let id = token(value, member(artifact, "stable_id", code)?, code)?;
        let edition = integer(value, member(artifact, "artifact_version", code)?, 1, code)?;
        return Ok(Target::Versioned(id, edition));
    }
    if artifact.len() == 1 {
        let hash = text(value, member(artifact, "content_hash", code)?)
            .filter(|hash| {
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            .ok_or_else(|| Error::new(code))?;
        return Ok(Target::Hash(hash.to_owned()));
    }
    Err(Error::new(code))
}

fn relations_rule(value: &Value) -> Result<Vec<Relation>, Error> {
    let code = ErrorCode::LineageInvalid;
    let Node::Array(items) = value.node() else {
        return Err(Error::new(code));
    };
    let mut unique = BTreeSet::new();
    for index in items {
        let members = object(value, *index, code)?;
        let source = token(value, member(members, "from_record_id", code)?, code)?;
        let kind = Kind::parse(text(value, member(members, "relation", code)?))?;
        unique.insert(Relation {
            source,
            kind,
            target: target_rule(value, members, kind)?,
        });
    }
    Ok(unique.into_iter().collect())
}

fn acyclic(count: usize, edges: &[(usize, usize)]) -> bool {
    let mut indegree = vec![0usize; count];
    let mut outgoing = vec![Vec::new(); count];
    for &(source, target) in edges {
        outgoing[source].push(target);
        indegree[target] += 1;
    }
    let mut ready: Vec<usize> = (0..count).filter(|i| indegree[*i] == 0).collect();
    let mut removed = 0;
    while let Some(source) = ready.pop() {
        removed += 1;
        for &target in &outgoing[source] {
            indegree[target] -= 1;
            if indegree[target] == 0 {
                ready.push(target);
            }
        }
    }
    removed == count
}

fn relation_value(relation: &Relation) -> Value {
    let mut members = vec![
        ("from_record_id", Value::string(&relation.source)),
        ("relation", Value::string(relation.kind.name())),
    ];
    match &relation.target {
        Target::Record(id) => members.push(("to_record_id", Value::string(id))),
        Target::Versioned(id, edition) => members.push((
            "to_artifact",
            vobject(vec![
                ("stable_id", Value::string(id)),
                ("artifact_version", Value::integer(*edition)),
            ]),
        )),
        Target::Hash(hash) => members.push((
            "to_artifact",
            vobject(vec![("content_hash", Value::string(hash))]),
        )),
    }
    vobject(members)
}

// Snapshot only routing scalars, never clone a payload just to preserve diagnostic context.
// This is consumed solely after admission has successfully validated gate 1.
fn routing_identity(value: &Value) -> Option<(String, String, i128)> {
    let code = ErrorCode::HistoryEnvelopeInvalid;
    let members = object(value, value.root, code).ok()?;
    Some((
        text(value, member(members, "profile", code).ok()?)?.to_owned(),
        text(value, member(members, "record_type", code).ok()?)?.to_owned(),
        integer(
            value,
            member(members, "schema_version", code).ok()?,
            1,
            code,
        )
        .ok()?,
    ))
}

/// Build the purpose-neutral projection of a closed store-local stream and decoder relations.
///
/// Generic stages run across all arguments before reader/stream/early-lineage shapes. Entries
/// are admitted in position order, then late graph checks and set-valued resolution run. Every
/// failure is atomic. An entry failure carries its store position; unknown-required also names
/// the full routing triple without naming an argument. No time, ID or position chooses a head.
pub fn project_history(
    reader: Input<'_>,
    stream: Input<'_>,
    relations: Input<'_>,
) -> Result<Value, Error> {
    let values = staged(&[
        ("reader", reader),
        ("stream", stream),
        ("relations", relations),
    ])?;
    let reader = reader_rule(&values[0]).map_err(|e| e.named("reader", true))?;
    let (store, entries) = stream_rule(&values[1]).map_err(|e| e.named("stream", true))?;
    let mut lineage = relations_rule(&values[2]).map_err(|e| e.named("relations", true))?;
    let mut records = Vec::new();
    let mut ids = Vec::new();
    let mut positions = Vec::new();
    let mut first: HashMap<String, (usize, Vec<u8>)> = HashMap::new();
    let mut skipped = Vec::new();
    let mut excluded = Vec::new();
    let mut duplicates = Vec::new();
    for &(position, index) in &entries {
        let record = values[1].detached(index);
        let routing = routing_identity(&record);
        let admission = envelope_rule(&reader, record).map_err(|mut e| {
            if e.code == ErrorCode::HistoryUnknownRequired {
                let (profile, kind, edition) =
                    routing.expect("unknown-required has passed routing validation");
                e.detail = format!("({profile}, {kind}, {edition}) is required but not recognized");
            } else {
                e = e.named("stream", true);
            }
            e.positioned(position)
        })?;
        match admission {
            HistoryAdmission::Skip => skipped.push(Value::integer(position)),
            HistoryAdmission::Excluded => excluded.push(Value::integer(position)),
            HistoryAdmission::Decode(envelope) => {
                let members = object(&envelope, envelope.root, ErrorCode::HistoryEnvelopeInvalid)?;
                let id = text(
                    &envelope,
                    member(members, "record_id", ErrorCode::HistoryEnvelopeInvalid)?,
                )
                .expect("decoded record ID is valid")
                .to_owned();
                let bytes = canonical_bytes(&envelope)?;
                if let Some((_, prior)) = first.get(&id) {
                    if *prior != bytes {
                        return Err(Error::new(ErrorCode::HistoryDuplicateRecordId)
                            .named("stream", true)
                            .positioned(position));
                    }
                    duplicates.push(vobject(vec![
                        ("position", Value::integer(position)),
                        ("record_id", Value::string(&id)),
                    ]));
                } else {
                    first.insert(id.clone(), (records.len(), bytes));
                    ids.push(id);
                    positions.push(position);
                    records.push(envelope);
                }
            }
        }
    }
    let mut resolution = Vec::new();
    let mut derivation = Vec::new();
    let mut replaced = vec![false; records.len()];
    let mut interpreters = vec![BTreeSet::new(); records.len()];
    let mut unresolved = Vec::new();
    for relation in &lineage {
        let invalid = || Error::new(ErrorCode::LineageInvalid).named("relations", true);
        let source = first.get(&relation.source).ok_or_else(invalid)?.0;
        let target = match &relation.target {
            Target::Record(id) => {
                if *id == relation.source {
                    return Err(invalid());
                }
                first.get(id).map(|(index, _)| *index)
            }
            _ => None,
        };
        if relation.kind.resolution() {
            let target = target.ok_or_else(invalid)?;
            resolution.push((source, target));
            if relation.kind.replaces() {
                replaced[target] = true;
            }
            if relation.kind == Kind::Reinterprets {
                interpreters[target].insert(source);
            }
        } else if let Some(target) = target {
            derivation.push((source, target));
        } else {
            unresolved.push(relation.clone());
        }
    }
    if !acyclic(records.len(), &resolution) || !acyclic(records.len(), &derivation) {
        return Err(Error::new(ErrorCode::LineageInvalid).named("relations", true));
    }
    let order = |relation: &Relation| {
        let target = match &relation.target {
            Target::Record(id) => match first.get(id) {
                Some((index, _)) => (0, positions[*index], String::new(), 0),
                None => (1, 0, id.clone(), 0),
            },
            Target::Versioned(id, edition) => (2, 0, id.clone(), *edition),
            Target::Hash(hash) => (3, 0, hash.clone(), 0),
        };
        (positions[first[&relation.source].0], relation.kind, target)
    };
    lineage.sort_by_cached_key(order);
    unresolved.sort_by_cached_key(order);
    let current = Value::array(
        ids.iter()
            .enumerate()
            .filter(|(i, _)| !replaced[*i])
            .map(|(_, id)| Value::string(id))
            .collect(),
    );
    let interpretations = Value::array(
        interpreters
            .iter()
            .enumerate()
            .filter(|(_, members)| !members.is_empty())
            .map(|(observation, members)| {
                vobject(vec![
                    ("observation", Value::string(&ids[observation])),
                    (
                        "current",
                        Value::array(
                            members
                                .iter()
                                .filter(|i| !replaced[**i])
                                .map(|i| Value::string(&ids[*i]))
                                .collect(),
                        ),
                    ),
                ])
            })
            .collect(),
    );
    Ok(vobject(vec![
        (
            "through",
            vobject(vec![
                ("store_id", Value::string(&store)),
                (
                    "position",
                    Value::integer(entries.last().map_or(0, |(p, _)| *p)),
                ),
            ]),
        ),
        ("records", Value::array(records)),
        ("skipped", Value::array(skipped)),
        ("excluded", Value::array(excluded)),
        ("duplicates", Value::array(duplicates)),
        (
            "relations",
            Value::array(lineage.iter().map(relation_value).collect()),
        ),
        (
            "unresolved_derivations",
            Value::array(unresolved.iter().map(relation_value).collect()),
        ),
        ("current", current),
        ("interpretations", interpretations),
    ]))
}
