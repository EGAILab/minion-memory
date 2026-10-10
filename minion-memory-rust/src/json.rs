//! Structural decoding without numeric conversion; no host map can discard a duplicate.

use crate::{Error, ErrorCode, Value, values::Node};
use std::collections::HashSet;

pub(crate) fn decode(bytes: &[u8]) -> Result<Value, Error> {
    let (text, cutoff) = match std::str::from_utf8(bytes) {
        Ok(text) => (text, false),
        Err(error) => (
            std::str::from_utf8(&bytes[..error.valid_up_to()]).expect("valid prefix"),
            true,
        ),
    };
    Parser {
        text,
        pos: 0,
        cutoff,
        nodes: Vec::new(),
    }
    .parse()
}

struct Frame {
    index: usize,
    key: Option<String>,
    names: HashSet<String>,
}

struct Parser<'a> {
    text: &'a str,
    pos: usize,
    cutoff: bool,
    nodes: Vec<Node>,
}

impl Parser<'_> {
    fn error_at(&self, code: ErrorCode, at: usize) -> Error {
        let code = if self.cutoff && at >= self.text.len() {
            ErrorCode::InvalidJson
        } else {
            code
        };
        Error::new(code).at(at)
    }
    fn invalid(&self) -> Error {
        self.error_at(ErrorCode::InvalidJson, self.pos)
    }
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }
    fn whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }
    fn push(&mut self, node: Node) -> usize {
        let index = self.nodes.len();
        self.nodes.push(node);
        index
    }
    fn parse(mut self) -> Result<Value, Error> {
        let mut frames = Vec::<Frame>::new();
        self.whitespace();
        loop {
            let (mut value, container) = self.start_value()?;
            if container {
                let mut frame = Frame {
                    index: value,
                    key: None,
                    names: HashSet::new(),
                };
                self.whitespace();
                let closer = if matches!(self.nodes[value], Node::Object(_)) {
                    b'}'
                } else {
                    b']'
                };
                if self.peek() == Some(closer) {
                    self.pos += 1;
                } else {
                    if closer == b'}' {
                        self.member_name(&mut frame)?;
                    }
                    frames.push(frame);
                    continue;
                }
            }
            loop {
                let Some(frame) = frames.last_mut() else {
                    self.whitespace();
                    if self.pos != self.text.len() || self.cutoff {
                        return Err(self.invalid());
                    }
                    return Ok(Value {
                        nodes: self.nodes,
                        root: value,
                    });
                };
                let closer = match &mut self.nodes[frame.index] {
                    Node::Array(children) => {
                        children.push(value);
                        b']'
                    }
                    Node::Object(members) => {
                        members.push((frame.key.take().expect("pending member"), value));
                        b'}'
                    }
                    _ => unreachable!("frame is a container"),
                };
                self.whitespace();
                match self.peek() {
                    Some(b',') => {
                        self.pos += 1;
                        self.whitespace();
                        if closer == b'}' {
                            self.member_name(frame)?;
                        }
                        break;
                    }
                    Some(c) if c == closer => {
                        self.pos += 1;
                        value = frames.pop().expect("open frame").index;
                    }
                    _ => return Err(self.invalid()),
                }
            }
        }
    }
    fn member_name(&mut self, frame: &mut Frame) -> Result<(), Error> {
        if self.peek() != Some(b'"') {
            return Err(self.invalid());
        }
        let at = self.pos;
        let name = self.string()?;
        if !frame.names.insert(name.clone()) {
            return Err(self.error_at(ErrorCode::DuplicateKey, at));
        }
        self.whitespace();
        if self.peek() != Some(b':') {
            return Err(self.invalid());
        }
        self.pos += 1;
        self.whitespace();
        frame.key = Some(name);
        Ok(())
    }
    fn start_value(&mut self) -> Result<(usize, bool), Error> {
        match self.peek() {
            Some(b'{') => {
                self.pos += 1;
                Ok((self.push(Node::Object(Vec::new())), true))
            }
            Some(b'[') => {
                self.pos += 1;
                Ok((self.push(Node::Array(Vec::new())), true))
            }
            Some(b'"') => {
                let text = self.string()?;
                Ok((self.push(Node::String(text)), false))
            }
            Some(b'-' | b'0'..=b'9') => {
                let text = self.number()?;
                Ok((self.push(Node::Numeral(text)), false))
            }
            _ => {
                for (literal, node) in [
                    ("null", Node::Null),
                    ("true", Node::Bool(true)),
                    ("false", Node::Bool(false)),
                ] {
                    if self.text[self.pos..].starts_with(literal) {
                        self.pos += literal.len();
                        return Ok((self.push(node), false));
                    }
                }
                Err(self.invalid())
            }
        }
    }
    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
    }
    fn number(&mut self) -> Result<String, Error> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(self.invalid()),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            let at = self.pos;
            self.digits();
            if at == self.pos {
                return Err(self.invalid());
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            let at = self.pos;
            self.digits();
            if at == self.pos {
                return Err(self.invalid());
            }
        }
        if matches!(self.peek(), Some(b'0'..=b'9')) {
            return Err(self.invalid());
        }
        Ok(self.text[start..self.pos].to_owned())
    }
    fn hex4(&self, at: usize) -> Option<u32> {
        let bytes = self.text.as_bytes().get(at..at + 4)?;
        bytes.iter().try_fold(0_u32, |n, c| {
            char::from(*c).to_digit(16).map(|v| n * 16 + v)
        })
    }
    fn string(&mut self) -> Result<String, Error> {
        self.pos += 1;
        let mut output = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.invalid());
            };
            if c == b'"' {
                self.pos += 1;
                return Ok(output);
            }
            if c < b' ' {
                return Err(self.invalid());
            }
            if c != b'\\' {
                let c = self.text[self.pos..]
                    .chars()
                    .next()
                    .expect("UTF-8 boundary");
                output.push(c);
                self.pos += c.len_utf8();
                continue;
            }
            let at = self.pos;
            let escape = self.text.as_bytes().get(at + 1).copied();
            let escaped = match escape {
                Some(b'"') => Some('"'),
                Some(b'\\') => Some('\\'),
                Some(b'/') => Some('/'),
                Some(b'b') => Some('\u{8}'),
                Some(b'f') => Some('\u{c}'),
                Some(b'n') => Some('\n'),
                Some(b'r') => Some('\r'),
                Some(b't') => Some('\t'),
                _ => None,
            };
            if let Some(c) = escaped {
                output.push(c);
                self.pos += 2;
                continue;
            }
            if escape != Some(b'u') {
                return Err(self.invalid());
            }
            let unit = self.hex4(at + 2).ok_or_else(|| self.invalid())?;
            let scalar = if (0xdc00..=0xdfff).contains(&unit) {
                return Err(self.error_at(ErrorCode::LoneSurrogate, at));
            } else if (0xd800..=0xdbff).contains(&unit) {
                let low = if self.text.as_bytes().get(at + 6..at + 8) == Some(b"\\u") {
                    self.hex4(at + 8)
                } else {
                    None
                };
                let Some(low) = low.filter(|low| (0xdc00..=0xdfff).contains(low)) else {
                    return Err(self.error_at(ErrorCode::LoneSurrogate, at));
                };
                self.pos += 6;
                0x10000 + (unit - 0xd800) * 1024 + low - 0xdc00
            } else {
                unit
            };
            output.push(char::from_u32(scalar).expect("decoded Unicode scalar"));
            self.pos += 6;
        }
    }
}
