//! Layer 02 contract controls independent of Python: gates, cursors and scope isolation.
use minion_memory::*;
use serde_json::{Value as Json, json};
use std::cmp::Ordering;

fn native(value: &Json) -> Value {
    match value {
        Json::Null => Value::null(),
        Json::Bool(b) => Value::boolean(*b),
        Json::String(s) => Value::string(s),
        Json::Number(n) => n.as_i64().map_or_else(
            || {
                n.as_u64().map_or_else(
                    || Value::number(n.as_f64().unwrap()),
                    |u| Value::integer(u.into()),
                )
            },
            |i| Value::integer(i.into()),
        ),
        Json::Array(a) => Value::array(a.iter().map(native).collect()),
        Json::Object(o) => Value::object(o.iter().map(|(k, v)| (k.clone(), native(v))).collect()),
    }
}
fn error<T: std::fmt::Debug>(result: Result<T, Error>, code: ErrorCode, argument: Option<&str>) {
    let e = result.unwrap_err();
    assert_eq!((e.code, e.argument.as_deref()), (code, argument), "{e}");
}
fn reader() -> Json {
    json!({"profile":"learning","recognized":[{"record_type":"attempt","schema_version":1}]})
}
fn record() -> Json {
    json!({"profile":"learning","record_type":"attempt","schema_version":1,"compatibility":"required","record_id":"ev-1","recorded_at":"2026-10-11t08:00:00z"})
}
fn admit(r: &Json, e: &Json) -> Result<HistoryAdmission, Error> {
    admit_history_record(Input::Native(&native(r)), Input::Native(&native(e)))
}
fn decoded(result: Result<HistoryAdmission, Error>) -> Value {
    match result.unwrap() {
        HistoryAdmission::Decode(v) => v,
        other => panic!("expected decode, got {other:?}"),
    }
}
fn as_json(v: &Value) -> Json {
    serde_json::from_slice(&canonical_bytes(v).unwrap()).unwrap()
}

#[test]
fn normalized_envelopes_preserve_optional_content_and_independent_times() {
    let mut e = record();
    e["schema_version"] = json!(1.0);
    e["occurred_at"] = json!("2026-10-11T12:00:00+02:00");
    e["payload"] = json!({"score":1.0,"answer":[null,true,{"💡":"origin"}]});
    e["provenance"] = json!(["hint",{"id":"h-1"}]);
    let r = reader();
    let rv = native(&r);
    let ev = native(&e);
    let rs = r.to_string();
    let es = e.to_string();
    for input in [
        Input::Native(&ev),
        Input::Text(&es),
        Input::Bytes(es.as_bytes()),
    ] {
        let got = as_json(&decoded(admit_history_record(Input::Native(&rv), input)));
        assert_eq!(got["schema_version"], json!(1));
        assert_eq!(got["recorded_at"], "2026-10-11T08:00:00.000000Z");
        assert_eq!(got["occurred_at"], "2026-10-11T10:00:00.000000Z");
        assert_eq!(got["payload"]["score"], json!(1));
        assert_eq!(got["payload"]["answer"], e["payload"]["answer"]);
        assert_eq!(got["provenance"], e["provenance"]);
    }
    decoded(admit_history_record(Input::Text(&rs), Input::Text(&es)));
    let bare = as_json(&decoded(admit(&r, &record())));
    assert!(bare.get("occurred_at").is_none());
    assert!(bare.get("payload").is_none());
    assert!(bare.get("provenance").is_none());
    let mut e = record();
    e["payload"] = Json::Null;
    e["provenance"] = Json::Null;
    let got = as_json(&decoded(admit(&r, &e)));
    assert_eq!(got["payload"], Json::Null);
    assert_eq!(got["provenance"], Json::Null);
}

#[test]
fn every_routing_member_is_validated_before_foreign_profile_exclusion() {
    for foreign in [false, true] {
        for field in ["profile", "record_type", "schema_version"] {
            let mut e = record();
            if foreign {
                e["profile"] = json!("agent");
            }
            e.as_object_mut().unwrap().remove(field);
            error(
                admit(&reader(), &e),
                ErrorCode::HistoryEnvelopeInvalid,
                Some("record"),
            );
            for bad in [
                Json::Null,
                json!(true),
                json!(0),
                json!(-1),
                json!(1.5),
                json!("Con"),
                json!("con"),
                json!("x."),
                json!([]),
                json!({}),
            ] {
                let mut e = record();
                if foreign {
                    e["profile"] = json!("agent");
                }
                e[field] = bad;
                error(
                    admit(&reader(), &e),
                    ErrorCode::HistoryEnvelopeInvalid,
                    Some("record"),
                );
            }
        }
    }
    for bad in [Json::Null, json!([]), json!("record"), json!(true)] {
        error(
            admit(&reader(), &bad),
            ErrorCode::HistoryEnvelopeInvalid,
            Some("record"),
        );
    }
}

#[test]
fn profile_and_unknown_schema_terminate_before_known_shape_or_typed_fields() {
    for compatibility in [
        None,
        Some(Json::Null),
        Some(json!("wrong")),
        Some(json!("required")),
        Some(json!("ignorable")),
    ] {
        let mut e = record();
        e["profile"] = json!("agent");
        e.as_object_mut().unwrap().remove("compatibility");
        if let Some(c) = compatibility {
            e["compatibility"] = c;
        }
        e.as_object_mut().unwrap().remove("record_id");
        e["recorded_at"] = Json::Null;
        e["future_field"] = json!(1);
        assert!(matches!(
            admit(&reader(), &e).unwrap(),
            HistoryAdmission::Excluded
        ));
    }
    for profile in ["unknown-profile", "agent"] {
        let e = json!({"profile":profile,"record_type":"attempt","schema_version":1});
        assert!(matches!(
            admit(&reader(), &e).unwrap(),
            HistoryAdmission::Excluded
        ));
    }
    for (kind, edition) in [
        ("future", 1),
        ("attempt", 2),
        ("future", 9007199254740991_i64),
    ] {
        let mut e = json!({"profile":"learning","record_type":kind,"schema_version":edition,"future_field":null,"occurred_at":null});
        e["compatibility"] = json!("ignorable");
        assert!(matches!(
            admit(&reader(), &e).unwrap(),
            HistoryAdmission::Skip
        ));
        e["compatibility"] = json!("required");
        error(
            admit(&reader(), &e),
            ErrorCode::HistoryUnknownRequired,
            None,
        );
        e.as_object_mut().unwrap().remove("compatibility");
        error(
            admit(&reader(), &e),
            ErrorCode::HistoryEnvelopeInvalid,
            Some("record"),
        );
        for bad in [Json::Null, json!(false), json!("Required"), json!([])] {
            e["compatibility"] = bad;
            error(
                admit(&reader(), &e),
                ErrorCode::HistoryEnvelopeInvalid,
                Some("record"),
            );
        }
    }
    // Empty recognition is valid and never implies a wildcard.
    let r = json!({"profile":"learning","recognized":[]});
    error(
        admit(&r, &record()),
        ErrorCode::HistoryUnknownRequired,
        None,
    );
}

#[test]
fn reader_is_closed_and_rejects_numeric_aliases_and_malformed_entries() {
    for bad in [
        Json::Null,
        json!([]),
        json!({}),
        json!({"profile":"learning"}),
        json!({"recognized":[]}),
        json!({"profile":"learning","recognized":[],"extra":1}),
        json!({"profile":"CON","recognized":[]}),
        json!({"profile":"learning","recognized":{}}),
    ] {
        error(
            admit(&bad, &record()),
            ErrorCode::HistoryReaderInvalid,
            Some("reader"),
        );
    }
    for entry in [
        Json::Null,
        json!(true),
        json!({}),
        json!({"record_type":"attempt"}),
        json!({"schema_version":1}),
        json!({"record_type":"attempt","schema_version":1,"extra":1}),
        json!({"record_type":"con","schema_version":1}),
        json!({"record_type":"attempt","schema_version":0}),
        json!({"record_type":"attempt","schema_version":true}),
    ] {
        let r = json!({"profile":"learning","recognized":[entry]});
        error(
            admit(&r, &record()),
            ErrorCode::HistoryReaderInvalid,
            Some("reader"),
        );
    }
    let r = r#"{"profile":"learning","recognized":[{"record_type":"attempt","schema_version":1},{"schema_version":1e0,"record_type":"attempt"}]}"#;
    error(
        admit_history_record(Input::Text(r), Input::Text(&record().to_string())),
        ErrorCode::HistoryReaderInvalid,
        Some("reader"),
    );
    let r = json!({"profile":"learning","recognized":[{"record_type":"attempt","schema_version":1},{"record_type":"attempt","schema_version":2},{"record_type":"exposure","schema_version":1}]});
    decoded(admit(&r, &record()));
}

#[test]
fn recognized_closed_shape_precedes_typed_fields() {
    for missing in ["record_id", "recorded_at"] {
        let mut e = record();
        e["occurred_at"] = Json::Null;
        e.as_object_mut().unwrap().remove(missing);
        error(
            admit(&reader(), &e),
            ErrorCode::HistoryEnvelopeInvalid,
            Some("record"),
        );
    }
    let mut e = record();
    e["extra"] = json!(1);
    e["occurred_at"] = Json::Null;
    e["record_id"] = json!("BAD");
    error(
        admit(&reader(), &e),
        ErrorCode::HistoryEnvelopeInvalid,
        Some("record"),
    );
    e.as_object_mut().unwrap().remove("extra");
    error(
        admit(&reader(), &e),
        ErrorCode::TimestampInvalid,
        Some("record"),
    );
    e.as_object_mut().unwrap().remove("occurred_at");
    error(admit(&reader(), &e), ErrorCode::IdInvalid, Some("record"));
}

#[test]
fn typed_fields_follow_canonical_order_on_all_text_source_permutations() {
    let fields = ["occurred_at", "record_id", "recorded_at"];
    let permutations = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    let r = native(&reader());
    for mask in 1..8 {
        let expected = if mask & 1 != 0 {
            ErrorCode::TimestampInvalid
        } else if mask & 2 != 0 {
            ErrorCode::IdInvalid
        } else {
            ErrorCode::TimestampInvalid
        };
        for order in permutations {
            let mut s = String::from(
                r#"{"profile":"learning","record_type":"attempt","schema_version":1,"compatibility":"required""#,
            );
            let mut entries = Vec::new();
            for i in order {
                let v = if mask >> i & 1 != 0 {
                    Value::null()
                } else if i == 1 {
                    Value::string("ev-1")
                } else {
                    Value::string("2026-10-11T08:00:00Z")
                };
                s.push_str(&format!(
                    ",\"{}\":{}",
                    fields[i],
                    String::from_utf8(canonical_bytes(&v).unwrap()).unwrap()
                ));
                entries.push((fields[i].to_string(), v));
            }
            s.push('}');
            for input in [Input::Text(&s), Input::Bytes(s.as_bytes())] {
                error(
                    admit_history_record(Input::Native(&r), input),
                    expected,
                    Some("record"),
                );
            }
            for (k, v) in [
                ("profile", Value::string("learning")),
                ("record_type", Value::string("attempt")),
                ("schema_version", Value::integer(1)),
                ("compatibility", Value::string("required")),
            ] {
                entries.push((k.into(), v));
            }
            error(
                admit_history_record(Input::Native(&r), Input::Native(&Value::object(entries))),
                expected,
                Some("record"),
            );
        }
    }
}

#[test]
fn history_generic_stages_across_both_arguments_precede_every_gate() {
    let invalid_reader = Value::null();
    let r = native(&reader());
    error(
        admit_history_record(
            Input::Native(&invalid_reader),
            Input::Text(r#"{"x":1,"x":2}"#),
        ),
        ErrorCode::DuplicateKey,
        Some("record"),
    );
    error(
        admit_history_record(
            Input::Native(&invalid_reader),
            Input::Text(r#"{"x":1e400}"#),
        ),
        ErrorCode::NumberNotFinite,
        Some("record"),
    );
    error(
        admit_history_record(
            Input::Text(r#"{"x":1e400}"#),
            Input::Text(r#"{"x":1,"x":2}"#),
        ),
        ErrorCode::DuplicateKey,
        Some("record"),
    );
    error(
        admit_history_record(Input::Native(&r), Input::Bytes(&[255])),
        ErrorCode::InvalidJson,
        Some("record"),
    );
    // Even otherwise excluded records still pass whole-value generic stages first.
    let v = Value::object(vec![
        ("profile".into(), Value::string("agent")),
        ("payload".into(), Value::number(f64::NAN)),
    ]);
    error(
        admit_history_record(Input::Native(&r), Input::Native(&v)),
        ErrorCode::NumberNotFinite,
        Some("record"),
    );
    error(
        admit_history_record(
            Input::Native(&r),
            Input::Text(
                r#"{"profile":"agent","record_type":"attempt","schema_version":1,"payload":{"x":1,"x":2}}"#,
            ),
        ),
        ErrorCode::DuplicateKey,
        Some("record"),
    );
}

#[test]
fn admitted_payload_remains_lossless_at_arbitrary_depth() {
    let payload = format!("{}0{}", "[".repeat(20_000), "]".repeat(20_000));
    let s = format!(
        r#"{{"profile":"learning","record_type":"attempt","schema_version":1,"compatibility":"required","record_id":"ev-1","recorded_at":"2026-10-11T08:00:00Z","payload":{payload}}}"#
    );
    let r = native(&reader());
    let v = decoded(admit_history_record(Input::Native(&r), Input::Text(&s)));
    let bytes = canonical_bytes(&v).unwrap();
    let normalized = String::from_utf8(bytes).unwrap();
    assert!(normalized.contains(&format!("\"payload\":{payload}")));
    assert!(normalized.contains("2026-10-11T08:00:00.000000Z"));
}

fn boundary(store: &str, position: i128) -> Value {
    native(&json!({"store_id":store,"position":position}))
}
#[test]
fn boundaries_order_only_inside_a_store_and_compare_the_complete_pair() {
    for a in [0, 1, SAFE_INTEGER_MAX] {
        for b in [0, 1, SAFE_INTEGER_MAX] {
            for sa in ["a", "z"] {
                for sb in ["a", "z"] {
                    let av = boundary(sa, a);
                    let bv = boundary(sb, b);
                    assert_eq!(
                        history_boundaries_equal(Input::Native(&av), Input::Native(&bv)).unwrap(),
                        sa == sb && a == b
                    );
                    if sa == sb {
                        assert_eq!(
                            compare_history_boundaries(Input::Native(&av), Input::Native(&bv))
                                .unwrap(),
                            a.cmp(&b)
                        );
                    } else {
                        error(
                            compare_history_boundaries(Input::Native(&av), Input::Native(&bv)),
                            ErrorCode::HistoryBoundaryIncomparable,
                            None,
                        );
                    }
                }
            }
        }
    }
    assert_eq!(
        compare_history_boundaries(
            Input::Text(r#"{"store_id":"a","position":-0.0}"#),
            Input::Bytes(br#"{"store_id":"a","position":0e0}"#)
        )
        .unwrap(),
        Ordering::Equal
    );
}

#[test]
fn boundary_errors_precede_relations_with_declared_argument_names() {
    let valid = boundary("z", 1);
    for bad in [
        Json::Null,
        json!([]),
        json!({}),
        json!({"position":0}),
        json!({"store_id":"a"}),
        json!({"store_id":"a","position":0,"extra":1}),
        json!({"store_id":"con","position":0}),
    ] {
        let v = native(&bad);
        error(
            compare_history_boundaries(Input::Native(&v), Input::Native(&valid)),
            ErrorCode::HistoryBoundaryInvalid,
            Some("a"),
        );
        error(
            history_boundaries_equal(Input::Native(&valid), Input::Native(&v)),
            ErrorCode::HistoryBoundaryInvalid,
            Some("b"),
        );
    }
    for position in [
        Json::Null,
        json!(true),
        json!(-1),
        json!(0.5),
        json!("1"),
        json!([]),
    ] {
        let v = native(&json!({"store_id":"a","position":position}));
        error(
            compare_history_boundaries(Input::Native(&valid), Input::Native(&v)),
            ErrorCode::HistoryBoundaryInvalid,
            Some("b"),
        );
    }
    let v = boundary("a", SAFE_INTEGER_MAX + 1);
    error(
        compare_history_boundaries(Input::Native(&valid), Input::Native(&v)),
        ErrorCode::IntegerOutOfRange,
        Some("b"),
    );
    error(
        compare_history_boundaries(
            Input::Native(&Value::null()),
            Input::Text(r#"{"x":1,"x":2}"#),
        ),
        ErrorCode::DuplicateKey,
        Some("b"),
    );
}

fn resolved(facets: Json) -> Value {
    native(&json!({"scope_state":"resolved","facets":facets}))
}
#[test]
fn scope_name_grammar_is_distinct_from_value_tokens_and_preserves_unknowns() {
    for name in [
        "a.b".to_owned(),
        "con.prn".into(),
        "agent.con".into(),
        "a-b.c_d".into(),
        format!("{}.{}", "a".repeat(128), "b".repeat(128)),
    ] {
        let scope = resolved(json!({&name:"v"}));
        assert_eq!(
            as_json(&validate_scope(Input::Native(&scope)).unwrap()),
            as_json(&scope)
        );
        assert!(!scope_visible(Input::Native(&scope), Input::Native(&native(&json!({})))).unwrap());
    }
    for name in [
        "a".to_owned(),
        ".b".into(),
        "a.".into(),
        "a.b.c".into(),
        "A.b".into(),
        "a.é".into(),
        "_a.b".into(),
        "a.-b".into(),
        format!("{}.b", "a".repeat(129)),
        format!("a.{}", "b".repeat(129)),
    ] {
        error(
            validate_scope(Input::Native(&resolved(json!({&name:"v"})))),
            ErrorCode::ScopeInvalid,
            None,
        );
    }
    for value in [
        Json::Null,
        json!(true),
        json!(1),
        json!([]),
        json!({}),
        json!("con"),
        json!("prn.txt"),
        json!("v."),
        json!("V"),
        json!(""),
        json!("v".repeat(129)),
    ] {
        error(
            validate_scope(Input::Native(&resolved(json!({"agent.task":value})))),
            ErrorCode::ScopeInvalid,
            None,
        );
    }
}

#[test]
fn scope_states_are_closed_and_unresolved_never_aliases_global() {
    for invalid in [
        Json::Null,
        json!([]),
        json!({}),
        json!({"scope_state":"resolved"}),
        json!({"scope_state":"resolved","facets":{},"extra":1}),
        json!({"scope_state":"unresolved","facets":{}}),
        json!({"scope_state":"bad"}),
        json!({"scope_state":true}),
        json!({"scope_state":"resolved","facets":[]}),
    ] {
        error(
            validate_scope(Input::Native(&native(&invalid))),
            ErrorCode::ScopeInvalid,
            None,
        );
    }
    let global = resolved(json!({}));
    let unresolved = native(&json!({"scope_state":"unresolved"}));
    let q = native(&json!({}));
    assert!(scope_visible(Input::Native(&global), Input::Native(&q)).unwrap());
    assert!(!scope_visible(Input::Native(&unresolved), Input::Native(&q)).unwrap());
    assert!(scopes_equal(Input::Native(&unresolved), Input::Native(&unresolved)).unwrap());
    assert!(!scopes_equal(Input::Native(&global), Input::Native(&unresolved)).unwrap());
    assert!(
        !scopes_equal(
            Input::Native(&global),
            Input::Native(&resolved(json!({"agent.task":"v"})))
        )
        .unwrap()
    );
}

#[test]
fn queries_validate_even_for_hidden_records_and_errors_follow_all_generic_stages() {
    for rec in [
        resolved(json!({})),
        resolved(json!({"learning.course":"v"})),
        native(&json!({"scope_state":"unresolved"})),
    ] {
        for q in [
            Json::Null,
            json!([]),
            json!({"scope_state":"unresolved"}),
            json!({"agent.task":null}),
            json!({"agent.con":"v"}),
            json!({"learning.course":"v"}),
            json!({"agent.session.x":"v"}),
        ] {
            error(
                scope_visible(Input::Native(&rec), Input::Native(&native(&q))),
                ErrorCode::ScopeQueryInvalid,
                Some("query"),
            );
        }
    }
    let bad = Value::null();
    let good = resolved(json!({}));
    error(
        scope_visible(Input::Native(&bad), Input::Native(&bad)),
        ErrorCode::ScopeInvalid,
        Some("record"),
    );
    error(
        scopes_equal(Input::Native(&bad), Input::Native(&bad)),
        ErrorCode::ScopeInvalid,
        Some("a"),
    );
    error(
        scopes_equal(Input::Native(&good), Input::Native(&bad)),
        ErrorCode::ScopeInvalid,
        Some("b"),
    );
    error(
        scope_visible(Input::Native(&bad), Input::Text(r#"{"x":1,"x":2}"#)),
        ErrorCode::DuplicateKey,
        Some("query"),
    );
    error(
        scopes_equal(
            Input::Native(&bad),
            Input::Native(&Value::number(f64::INFINITY)),
        ),
        ErrorCode::NumberNotFinite,
        Some("b"),
    );
    error(
        validate_scope(Input::Bytes(
            br#"{"scope_state":"resolved","facets":{"agent.task":"\ud800"}}"#,
        )),
        ErrorCode::LoneSurrogate,
        None,
    );
    let raw = r#"{"scope_state":"resolved","facets":{"agent.task":"v","agent.principal":"p"}}"#;
    assert!(
        scope_visible(
            Input::Text(raw),
            Input::Bytes(br#"{"agent.principal":"p","agent.task":"v","agent.session":"s"}"#)
        )
        .unwrap()
    );
    assert!(
        scopes_equal(
            Input::Text(raw),
            Input::Native(&resolved(json!({"agent.principal":"p","agent.task":"v"})))
        )
        .unwrap()
    );
}

#[test]
fn six_facet_combinations_have_no_hierarchy_or_query_wildcards() {
    assert_eq!(
        AGENT_FACETS.as_slice(),
        &[
            "agent.principal",
            "agent.agent",
            "agent.workspace",
            "agent.conversation",
            "agent.session",
            "agent.task"
        ]
    );
    let mut count = 0;
    for pattern in 0..729 {
        // Three states per record facet: absent / v / w.
        let mut n = pattern;
        let mut r = serde_json::Map::new();
        for name in AGENT_FACETS {
            let choice = n % 3;
            n /= 3;
            if choice != 0 {
                r.insert(name.into(), json!(if choice == 1 { "v" } else { "w" }));
            }
        }
        let scope = resolved(Json::Object(r.clone()));
        for mask in 0..64 {
            let mut q = serde_json::Map::new();
            for (i, name) in AGENT_FACETS.iter().enumerate() {
                if mask >> i & 1 != 0 {
                    q.insert((*name).into(), json!("v"));
                }
            }
            // Spec predicate, independent of the implementation's arena and lookup functions.
            let expected = r.iter().all(|(k, v)| q.get(k) == Some(v));
            assert_eq!(
                scope_visible(
                    Input::Native(&scope),
                    Input::Native(&native(&Json::Object(q)))
                )
                .unwrap(),
                expected
            );
            count += 1;
        }
        let reversed = Value::object(vec![
            ("scope_state".into(), Value::string("resolved")),
            (
                "facets".into(),
                Value::object(
                    r.iter()
                        .rev()
                        .map(|(k, v)| (k.clone(), native(v)))
                        .collect(),
                ),
            ),
        ]);
        assert!(scopes_equal(Input::Native(&scope), Input::Native(&reversed)).unwrap());
    }
    assert_eq!(count, 46_656);
}
