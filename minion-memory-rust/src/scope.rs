//! SCOPE-001..006: the two-state MemoryScope and flat, fail-closed Agent matching.

use crate::history::{object, text, token};
use crate::identity::staged;
use crate::{Error, ErrorCode, Input, Value};

/// G-061's complete Layer 02 Agent vocabulary. Aliases and Learning facets are not registered.
pub const AGENT_FACETS: [&str; 6] = [
    "agent.principal",
    "agent.agent",
    "agent.workspace",
    "agent.conversation",
    "agent.session",
    "agent.task",
];

fn facet_name(name: &str) -> bool {
    let segment = |s: &str| {
        let alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
        (1..=128).contains(&s.len())
            && alnum(s.as_bytes()[0])
            && s.bytes().all(|b| alnum(b) || matches!(b, b'_' | b'-'))
    };
    name.split_once('.')
        .is_some_and(|(a, b)| segment(a) && segment(b))
}

fn facets(value: &Value, index: usize, code: ErrorCode) -> Result<&[(String, usize)], Error> {
    let members = object(value, index, code)?;
    for (name, index) in members {
        if !facet_name(name) {
            return Err(Error::new(code));
        }
        token(value, *index, code)?;
    }
    Ok(members)
}

// None is unresolved; an empty resolved map is Some(&[]), so global never aliases unresolved.
fn scope_rule(value: &Value) -> Result<Option<&[(String, usize)]>, Error> {
    let code = ErrorCode::ScopeInvalid;
    let members = object(value, value.root, code)?;
    let find = |name| members.iter().find(|(k, _)| k == name).map(|(_, i)| *i);
    match find("scope_state").and_then(|i| text(value, i)) {
        Some("unresolved") if members.len() == 1 => Ok(None),
        Some("resolved") if members.len() == 2 => {
            let index = find("facets").ok_or_else(|| Error::new(code))?;
            Ok(Some(facets(value, index, code)?))
        }
        _ => Err(Error::new(code)),
    }
}

fn query_rule(value: &Value) -> Result<&[(String, usize)], Error> {
    let code = ErrorCode::ScopeQueryInvalid;
    let members = facets(value, value.root, code)?;
    if members
        .iter()
        .any(|(k, _)| !AGENT_FACETS.contains(&k.as_str()))
    {
        return Err(Error::new(code));
    }
    Ok(members)
}

/// Validate one closed MemoryScope and return its normalized value, preserving unknown facets.
/// Single-argument validation errors name no argument.
pub fn validate_scope(scope: Input<'_>) -> Result<Value, Error> {
    let mut values = staged(&[("scope", scope)])?;
    scope_rule(&values[0])?;
    Ok(values.pop().expect("one staged argument"))
}

/// Exact structural scope equality; absent and present facets differ, regardless of map order.
pub fn scopes_equal(a: Input<'_>, b: Input<'_>) -> Result<bool, Error> {
    let values = staged(&[("a", a), ("b", b)])?;
    let a = scope_rule(&values[0]).map_err(|e| e.named("a", true))?;
    let b = scope_rule(&values[1]).map_err(|e| e.named("b", true))?;
    Ok(match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.len() == b.len()
                && a.iter().zip(b).all(|((ka, ia), (kb, ib))| {
                    ka == kb && text(&values[0], *ia) == text(&values[1], *ib)
                })
        }
        _ => false,
    })
}

/// True only if every resolved record facet explicitly matches a known query facet.
/// Unresolved or unknown-facet records are hidden, but an invalid query is always rejected first.
pub fn scope_visible(record: Input<'_>, query: Input<'_>) -> Result<bool, Error> {
    let values = staged(&[("record", record), ("query", query)])?;
    let record = scope_rule(&values[0]).map_err(|e| e.named("record", true))?;
    let query = query_rule(&values[1]).map_err(|e| e.named("query", true))?;
    let Some(record) = record else {
        return Ok(false);
    };
    Ok(record.iter().all(|(name, index)| {
        AGENT_FACETS.contains(&name.as_str())
            && query
                .iter()
                .any(|(key, q)| key == name && text(&values[0], *index) == text(&values[1], *q))
    }))
}
