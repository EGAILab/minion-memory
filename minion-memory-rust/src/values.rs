//! Owned, flat-arena value binding. Private indices prevent cycles and invalid graphs.

use crate::{Error, ErrorCode, json};
use sha2::{Digest, Sha256};

/// Maximum magnitude of an admitted integral value (G-016/G-025).
pub const SAFE_INTEGER_MAX: i128 = (1_i128 << 53) - 1;

#[derive(Debug, Clone)]
pub(crate) enum Node {
    Null,
    Bool(bool),
    Integer(i128),
    Float(f64),
    Numeral(String), // decoder-owned, never exposed as a native number kind
    String(String),
    Array(Vec<usize>),
    Object(Vec<(String, usize)>),
}

/// A native JSON-shaped tree. Inputs may contain invalid numbers or duplicate names;
/// public validation rejects them before returning a normalized value.
///
/// Arena storage keeps clone/debug/drop iterative even for deeply nested trees.
/// Rust strings cannot contain surrogates; non-string keys and cycles cannot be built.
#[derive(Debug, Clone)]
pub struct Value {
    pub(crate) nodes: Vec<Node>,
    pub(crate) root: usize,
}

impl Value {
    pub(crate) fn scalar(node: Node) -> Self {
        Self {
            nodes: vec![node],
            root: 0,
        }
    }
    /// Null.
    pub fn null() -> Self {
        Self::scalar(Node::Null)
    }
    /// A boolean, distinct from the numeric kind.
    pub fn boolean(value: bool) -> Self {
        Self::scalar(Node::Bool(value))
    }
    /// An exact native integer, checked against the safe bound at the seam.
    pub fn integer(value: i128) -> Self {
        Self::scalar(Node::Integer(value))
    }
    /// A native binary64 value, checked for finiteness and integral bounds at the seam.
    pub fn number(value: f64) -> Self {
        Self::scalar(Node::Float(value))
    }
    /// A Unicode-scalar string; Rust's String already enforces that domain.
    pub fn string(value: &str) -> Self {
        Self::scalar(Node::String(value.to_owned()))
    }
    /// An ordered array. Passing a Vec/slice/array of cloned values preserves element order.
    pub fn array(values: Vec<Self>) -> Self {
        let mut result = Self {
            nodes: Vec::new(),
            root: 0,
        };
        let children = values
            .into_iter()
            .map(|value| result.append(value))
            .collect();
        result.root = result.nodes.len();
        result.nodes.push(Node::Array(children));
        result
    }
    /// Bind a Rust slice or array as an ordered native array, cloning exact stored values.
    pub fn array_from_slice(values: &[Self]) -> Self {
        Self::array(values.to_vec())
    }
    /// An object retaining duplicate names for stage-1 rejection. Insertion order is not semantic.
    pub fn object(values: Vec<(String, Self)>) -> Self {
        let mut result = Self {
            nodes: Vec::new(),
            root: 0,
        };
        let children = values
            .into_iter()
            .map(|(name, value)| (name, result.append(value)))
            .collect();
        result.root = result.nodes.len();
        result.nodes.push(Node::Object(children));
        result
    }
    fn append(&mut self, mut value: Self) -> usize {
        let offset = self.nodes.len();
        for node in &mut value.nodes {
            match node {
                Node::Array(children) => {
                    for child in children {
                        *child += offset;
                    }
                }
                Node::Object(children) => {
                    for (_, child) in children {
                        *child += offset;
                    }
                }
                _ => {}
            }
        }
        self.nodes.extend(value.nodes);
        value.root + offset
    }
    pub(crate) fn node(&self) -> &Node {
        &self.nodes[self.root]
    }
    pub(crate) fn subtree(&self, root: usize) -> Self {
        // A flat clone is safe at arbitrary depth; unreachable arena nodes are never interpreted.
        Self {
            nodes: self.nodes.clone(),
            root,
        }
    }
    pub(crate) fn detached(&self, root: usize) -> Self {
        // Copy reachable nodes only. Extracting every record must not clone its entire stream.
        // An explicit work stack keeps payload extraction and drop safe at arbitrary depth.
        let mut nodes = vec![Node::Null];
        let mut pending = vec![(root, 0)];
        while let Some((source, destination)) = pending.pop() {
            nodes[destination] = match &self.nodes[source] {
                Node::Array(children) => Node::Array(
                    children
                        .iter()
                        .map(|child| {
                            let index = nodes.len();
                            nodes.push(Node::Null);
                            pending.push((*child, index));
                            index
                        })
                        .collect(),
                ),
                Node::Object(members) => Node::Object(
                    members
                        .iter()
                        .map(|(name, child)| {
                            let index = nodes.len();
                            nodes.push(Node::Null);
                            pending.push((*child, index));
                            (name.clone(), index)
                        })
                        .collect(),
                ),
                scalar => scalar.clone(),
            };
        }
        Self { nodes, root: 0 }
    }
    /// The normalized root string, if this value is a string.
    pub fn as_str(&self) -> Option<&str> {
        if let Node::String(value) = self.node() {
            Some(value)
        } else {
            None
        }
    }
    /// The normalized root integer, if this value is integral.
    pub fn as_integer(&self) -> Option<i128> {
        if let Node::Integer(value) = self.node() {
            Some(*value)
        } else {
            None
        }
    }
}

/// An explicit borrowed public seam; no caller conversion hooks are dispatched.
#[derive(Debug, Clone, Copy)]
pub enum Input<'a> {
    /// A native tree.
    Native(&'a Value),
    /// JSON text, decoded by the library (not by a runner).
    Text(&'a str),
    /// UTF-8 JSON bytes; invalid encoding participates in source-order error selection.
    Bytes(&'a [u8]),
}

pub(crate) fn utf16_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

fn structural(value: &Value) -> Result<Value, Error> {
    let mut value = value.clone();
    let mut pending = vec![value.root];
    while let Some(index) = pending.pop() {
        match &mut value.nodes[index] {
            Node::Object(members) => {
                members.sort_by(|a, b| utf16_cmp(&a.0, &b.0));
                if members.windows(2).any(|pair| pair[0].0 == pair[1].0) {
                    return Err(Error::new(ErrorCode::DuplicateKey));
                }
                pending.extend(members.iter().rev().map(|(_, child)| *child));
            }
            Node::Array(children) => pending.extend(children.iter().rev().copied()),
            _ => {}
        }
    }
    Ok(value)
}

fn normalized_number(number: f64) -> Result<Node, Error> {
    if !number.is_finite() {
        return Err(Error::new(ErrorCode::NumberNotFinite));
    }
    if number.fract() == 0.0 {
        if number.abs() > SAFE_INTEGER_MAX as f64 {
            return Err(Error::new(ErrorCode::IntegerOutOfRange));
        }
        return Ok(Node::Integer(number as i128));
    }
    Ok(Node::Float(number))
}

fn normalize(mut value: Value) -> Result<Value, Error> {
    let mut pending = vec![value.root];
    while let Some(index) = pending.pop() {
        let replacement = match &mut value.nodes[index] {
            Node::Numeral(text) => Some(normalized_number(
                text.parse::<f64>()
                    .map_err(|_| Error::new(ErrorCode::NumberNotFinite))?,
            )?),
            Node::Float(number) => Some(normalized_number(*number)?),
            Node::Integer(number) if !(-SAFE_INTEGER_MAX..=SAFE_INTEGER_MAX).contains(number) => {
                return Err(Error::new(ErrorCode::IntegerOutOfRange));
            }
            Node::Array(children) => {
                pending.extend(children.iter().rev().copied());
                None
            }
            Node::Object(members) => {
                members.sort_by(|a, b| utf16_cmp(&a.0, &b.0));
                pending.extend(members.iter().rev().map(|(_, child)| *child));
                None
            }
            _ => None,
        };
        if let Some(node) = replacement {
            value.nodes[index] = node;
        }
    }
    Ok(value)
}

/// Validate a native tree and return normalized owned values (SER-008).
pub fn validate_value(value: &Value) -> Result<Value, Error> {
    normalize(structural(value)?)
}

/// Decode JSON bytes: structural decoding first, then the numeric domain.
pub fn decode_json(text: &[u8]) -> Result<Value, Error> {
    normalize(json::decode(text)?)
}

/// Decode JSON string input with the same structural/value stages.
pub fn decode_text(text: &str) -> Result<Value, Error> {
    decode_json(text.as_bytes())
}

/// Run each generic stage across all arguments in the supplied declaration order.
///
/// Returns values positionally. Typed primitives declare fixed argument names themselves;
/// argument labels are diagnostics, not object member names. The Rust call shape is statically typed.
pub fn validate_arguments(arguments: &[(&str, Input<'_>)]) -> Result<Vec<Value>, Error> {
    let several = arguments.len() > 1;
    let decoded = arguments
        .iter()
        .map(|(name, input)| {
            match input {
                Input::Native(value) => structural(value),
                Input::Text(text) => json::decode(text.as_bytes()),
                Input::Bytes(bytes) => json::decode(bytes),
            }
            .map_err(|error| error.named(name, several))
        })
        .collect::<Result<Vec<_>, _>>()?;
    decoded
        .into_iter()
        .zip(arguments)
        .map(|(value, (name, _))| normalize(value).map_err(|error| error.named(name, several)))
        .collect()
}

fn quote(text: &str) -> String {
    let mut result = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\u{8}' => result.push_str("\\b"),
            '\u{c}' => result.push_str("\\f"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c < ' ' => {
                use std::fmt::Write;
                write!(result, "\\u{:04x}", u32::from(c)).expect("String write");
            }
            c => result.push(c),
        }
    }
    result.push('"');
    result
}

pub(crate) fn serialize(value: &Value, root: usize) -> Vec<u8> {
    enum Piece {
        Node(usize),
        Text(String),
    }
    let mut pending = vec![Piece::Node(root)];
    let mut output = String::new();
    while let Some(piece) = pending.pop() {
        let Piece::Node(index) = piece else {
            if let Piece::Text(text) = piece {
                output.push_str(&text);
            }
            continue;
        };
        match &value.nodes[index] {
            Node::Null => output.push_str("null"),
            Node::Bool(b) => output.push_str(if *b { "true" } else { "false" }),
            Node::Integer(number) => output.push_str(&number.to_string()),
            Node::Float(number) => output.push_str(ryu_js::Buffer::new().format_finite(*number)),
            Node::String(text) => output.push_str(&quote(text)),
            Node::Numeral(_) => unreachable!("serialization receives normalized values"),
            Node::Array(children) => {
                pending.push(Piece::Text("]".into()));
                for (i, child) in children.iter().enumerate().rev() {
                    pending.push(Piece::Node(*child));
                    if i > 0 {
                        pending.push(Piece::Text(",".into()));
                    }
                }
                pending.push(Piece::Text("[".into()));
            }
            Node::Object(members) => {
                pending.push(Piece::Text("}".into()));
                for (i, (name, child)) in members.iter().enumerate().rev() {
                    pending.push(Piece::Node(*child));
                    pending.push(Piece::Text(format!("{}:", quote(name))));
                    if i > 0 {
                        pending.push(Piece::Text(",".into()));
                    }
                }
                pending.push(Piece::Text("{".into()));
            }
        }
    }
    output.into_bytes()
}

/// RFC 8785 bytes of a native value after full domain validation.
pub fn canonical_bytes(value: &Value) -> Result<Vec<u8>, Error> {
    let value = validate_value(value)?;
    Ok(serialize(&value, value.root))
}

/// SHA-256, lowercase hex, of the canonical structured value bytes (SER-009).
pub fn content_hash(value: &Value) -> Result<String, Error> {
    Ok(raw_content_hash(&canonical_bytes(value)?))
}

/// SHA-256 over raw bytes exactly as supplied, without any normalization (CORE-011).
pub fn raw_content_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
