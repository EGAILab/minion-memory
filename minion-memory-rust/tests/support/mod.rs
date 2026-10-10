//! Runner infrastructure only: portable YAML, schema and public-value construction.
use regex::Regex;
use serde_json::{Map, Value as Json};
use std::sync::LazyLock;
use yaml_rust2::parser::{Event, EventReceiver, Parser};
use yaml_rust2::scanner::TScalarStyle;

static INTEGER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^-?(?:0|[1-9][0-9]*)$").unwrap());
static FLOAT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+(?:[eE][-+]?[0-9]+)?|[eE][-+]?[0-9]+)$").unwrap()
});
// PyYAML's YAML-1.1 implicit rules. Anything resolved differently from the
// project's decimal JSON subset must be quoted, including exponent-only floats.
static LEGACY_NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)^(?:
 [-+]?0b[0-1_]+ | [-+]?0[0-7_]+ | [-+]?(?:0|[1-9][0-9_]*) |
 [-+]?0x[0-9a-fA-F_]+ | [-+]?[1-9][0-9_]*(?::[0-5]?[0-9])+ |
 [-+]?(?:[0-9][0-9_]*)\.[0-9_]*(?:[eE][-+][0-9]+)? |
 [-+]?\.[0-9_]+(?:[eE][-+][0-9]+)? |
 [-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]* |
 [-+]?\.(?:inf|Inf|INF) | \.(?:nan|NaN|NAN))$",
    )
    .unwrap()
});
static LEGACY_DATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:[0-9]{4}-[0-9]{2}-[0-9]{2}|[0-9]{4}-[0-9]{1,2}-[0-9]{1,2}(?:[Tt]|[ \t]+)[0-9]{1,2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]*)?(?:[ \t]*(?:Z|[-+][0-9]{1,2}(?::[0-9]{2})?))?)$").unwrap()
});

fn scalar(text: &str, style: TScalarStyle) -> Result<Json, String> {
    if style != TScalarStyle::Plain {
        return Ok(Json::String(text.into()));
    }
    match text {
        "true" | "True" | "TRUE" => return Ok(Json::Bool(true)),
        "false" | "False" | "FALSE" => return Ok(Json::Bool(false)),
        "" | "~" | "null" | "Null" | "NULL" => return Ok(Json::Null),
        _ => {}
    }
    let integer = INTEGER.is_match(text);
    let float = FLOAT.is_match(text);
    if integer || float {
        if float && !LEGACY_NUMBER.is_match(text) {
            return Err("ambiguous YAML numeric scalar: quote it".into());
        }
        let value: Json =
            serde_json::from_str(text).map_err(|e| format!("JSON-domain YAML number: {e}"))?;
        if float && !value.as_f64().is_some_and(f64::is_finite) {
            return Err("non-finite YAML float".into());
        }
        return Ok(value);
    }
    if matches!(
        text,
        "yes" | "Yes" | "YES" | "no" | "No" | "NO" | "on" | "On" | "ON" | "off" | "Off" | "OFF"
    ) || LEGACY_NUMBER.is_match(text)
        || LEGACY_DATE.is_match(text)
    {
        return Err("YAML 1.1/core ambiguity: quote the scalar".into());
    }
    Ok(Json::String(text.into()))
}

enum Frame {
    Array(Vec<Json>),
    Object(Map<String, Json>, Option<String>),
}
#[derive(Default)]
struct Loader {
    stack: Vec<Frame>,
    root: Option<Json>,
    error: Option<String>,
    nodes: usize,
    documents: usize,
}
impl Loader {
    fn add(&mut self, value: Json) -> Result<(), String> {
        match self.stack.last_mut() {
            Some(Frame::Array(values)) => values.push(value),
            Some(Frame::Object(values, key)) => {
                if let Some(key) = key.take() {
                    values.insert(key, value);
                } else {
                    let Json::String(name) = value else {
                        return Err("YAML mapping keys must be strings".into());
                    };
                    if values.contains_key(&name) {
                        return Err("duplicate YAML key".into());
                    }
                    *key = Some(name);
                }
            }
            None => {
                if self.root.replace(value).is_some() {
                    return Err("multiple YAML roots".into());
                }
            }
        }
        Ok(())
    }
    fn event(&mut self, event: Event) -> Result<(), String> {
        let sequence = matches!(&event, Event::SequenceStart(..));
        let key = matches!(self.stack.last(), Some(Frame::Object(_, None)));
        if matches!(&event, Event::SequenceStart(..) | Event::MappingStart(..))
            || (matches!(&event, Event::Scalar(..)) && !key)
        {
            self.nodes += 1;
        }
        if self.nodes > 200_000 {
            return Err("YAML document too large".into());
        }
        match event {
            Event::Scalar(text, style, anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    return Err("YAML anchors/tags forbidden".into());
                }
                self.add(scalar(&text, style)?)?;
            }
            Event::SequenceStart(anchor, tag) | Event::MappingStart(anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    return Err("YAML anchors/tags forbidden".into());
                }
                if self.stack.len() >= 64 {
                    return Err("YAML container depth exceeds 64".into());
                }
                self.stack.push(if sequence {
                    Frame::Array(vec![])
                } else {
                    Frame::Object(Map::new(), None)
                });
            }
            Event::SequenceEnd | Event::MappingEnd => {
                let value = match self.stack.pop().ok_or("unbalanced YAML")? {
                    Frame::Array(values) => Json::Array(values),
                    Frame::Object(values, None) => Json::Object(values),
                    Frame::Object(_, Some(_)) => return Err("missing YAML mapping value".into()),
                };
                self.add(value)?;
            }
            Event::Alias(_) => return Err("YAML aliases forbidden".into()),
            Event::DocumentStart => {
                self.documents += 1;
                if self.documents > 1 {
                    return Err("multiple YAML documents".into());
                }
            }
            _ => {}
        }
        Ok(())
    }
}
impl EventReceiver for Loader {
    fn on_event(&mut self, event: Event) {
        if self.error.is_none()
            && let Err(error) = self.event(event)
        {
            self.error = Some(error);
        }
    }
}
pub fn load_yaml(text: &str) -> Result<Json, String> {
    if text.trim().is_empty() {
        return Err("empty YAML".into());
    }
    let mut loader = Loader::default();
    Parser::new_from_str(text)
        .load(&mut loader, true)
        .map_err(|e| e.to_string())?;
    if let Some(error) = loader.error {
        return Err(error);
    }
    loader.root.ok_or_else(|| "empty YAML".into())
}
pub fn native(value: &Json) -> minion_memory::Value {
    use minion_memory::Value;
    match value {
        Json::Null => Value::null(),
        Json::Bool(b) => Value::boolean(*b),
        Json::String(s) => Value::string(s),
        Json::Number(n) => {
            if let Some(n) = n.as_i64() {
                Value::integer(n.into())
            } else if let Some(n) = n.as_u64() {
                Value::integer(n.into())
            } else {
                let spelling = n.to_string();
                if !spelling.contains(['.', 'e', 'E']) {
                    Value::integer(
                        spelling
                            .parse()
                            .expect("native Rust corpus integer must fit i128"),
                    )
                } else {
                    Value::number(n.as_f64().expect("finite YAML JSON number"))
                }
            }
        }
        Json::Array(a) => Value::array(a.iter().map(native).collect()),
        Json::Object(o) => Value::object(o.iter().map(|(k, v)| (k.clone(), native(v))).collect()),
    }
}
pub fn placeholder(value: &Json) -> bool {
    match value {
        Json::String(s) => s.starts_with("TO_BE_"),
        Json::Array(a) => a.iter().any(placeholder),
        Json::Object(o) => o.values().any(placeholder),
        _ => false,
    }
}
