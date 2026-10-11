//! Contract-derived public-seam controls for HIST-006/007 and PROJ-001/002.
mod support;
use minion_memory::*;
use serde_json::{Value as Json, json};
use std::path::Path;

fn reader() -> Json {
    json!({"profile":"learning","recognized":[{"record_type":"attempt","schema_version":1}]})
}
fn record(id: &str) -> Json {
    json!({"record_id":id,"profile":"learning","record_type":"attempt","schema_version":1,
           "compatibility":"required","recorded_at":"2026-10-11T08:00:00Z"})
}
fn stream(records: Vec<Json>) -> Json {
    json!({"store_id":"s","entries":records.into_iter().enumerate().map(|(i,r)|json!({"position":i+1,"record":r})).collect::<Vec<_>>()})
}
fn relation(source: &str, kind: &str, target: &str) -> Json {
    json!({"from_record_id":source,"relation":kind,"to_record_id":target})
}
fn input<'a>(kind: usize, native: &'a Value, text: &'a str) -> Input<'a> {
    match kind {
        0 => Input::Native(native),
        1 => Input::Text(text),
        _ => Input::Bytes(text.as_bytes()),
    }
}
fn call(args: &[Json; 3], seams: usize) -> Result<Value, Error> {
    let values = args.each_ref().map(support::native);
    let texts = args.each_ref().map(Json::to_string);
    project_history(
        input(seams % 3, &values[0], &texts[0]),
        input(seams / 3 % 3, &values[1], &texts[1]),
        input(seams / 9, &values[2], &texts[2]),
    )
}
fn observed(value: &Value) -> Json {
    serde_json::from_slice(&canonical_bytes(value).unwrap()).unwrap()
}
fn failure(args: &[Json; 3], code: ErrorCode, argument: Option<&str>, position: Option<i128>) {
    for seams in 0..27 {
        let e = call(args, seams).unwrap_err();
        assert_eq!(
            (e.code, e.argument.as_deref(), e.position),
            (code, argument, position),
            "seams {seams}, {args:?}"
        );
    }
}

#[test]
fn canonical_projection_all_native_text_byte_combinations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../conformance/core");
    let mut comparisons = 0;
    for file in [
        "projection-canonical-path.yaml",
        "projection-lineage-current-heads.yaml",
    ] {
        let scenario =
            support::load_yaml(&std::fs::read_to_string(root.join(file)).unwrap()).unwrap();
        assert_eq!(scenario["status"], "ACTIVE");
        assert!(!support::placeholder(&scenario));
        for (step, want) in scenario["when"]
            .as_array()
            .unwrap()
            .iter()
            .zip(scenario["expect"]["results"].as_array().unwrap())
        {
            let params = &step["project_history"];
            let args = [
                params["reader"].clone(),
                params["stream"].clone(),
                params["relations"].clone(),
            ];
            for seams in 0..27 {
                match call(&args, seams) {
                    Ok(value) => assert_eq!(
                        observed(&value),
                        want["projection"],
                        "{file} {}, seam {seams}",
                        want["case"]
                    ),
                    Err(e) => {
                        assert_eq!(
                            e.code.as_str(),
                            want["error"].as_str().unwrap(),
                            "{file} {}, seam {seams}",
                            want["case"]
                        );
                        if let Some(arg) = want["argument"].as_str() {
                            assert_eq!(e.argument.as_deref(), Some(arg));
                        }
                        assert_eq!(e.position, want["position"].as_i64().map(i128::from));
                    }
                }
                comparisons += 1;
            }
        }
    }
    assert_eq!(comparisons, 54 * 27);
}

#[test]
fn early_lineage_shapes_and_late_graphs_straddle_entry_errors() {
    let early = vec![
        Json::Null,
        json!([null]),
        json!([{}]),
        json!([relation("INVALID", "supersedes", "a")]),
        json!([relation("a", "unknown", "a")]),
        json!([relation("a", "supersedes", "INVALID")]),
        json!([{"from_record_id":"a","relation":"supersedes","to_artifact":{"stable_id":"ku","artifact_version":1}}]),
        json!([{"from_record_id":"a","relation":"derived_from","to_artifact":{"stable_id":"ku"}}]),
        json!([{"from_record_id":"a","relation":"derived_from","to_artifact":{"content_hash":"A".repeat(64)}}]),
        json!([{"from_record_id":"a","relation":"supersedes","to_record_id":"b","extra":1}]),
        json!([{"from_record_id":"a","relation":"derived_from","to_record_id":"b","to_artifact":{"content_hash":"a".repeat(64)}}]),
    ];
    let late = vec![
        json!([relation("a", "supersedes", "a")]),
        json!([relation("missing", "supersedes", "a")]),
        json!([relation("a", "supersedes", "missing")]),
        json!([relation("a", "reinterprets", "missing")]),
    ];
    for (field, value, code, arg) in [
        (
            "record_type",
            "mystery",
            ErrorCode::HistoryUnknownRequired,
            None,
        ),
        ("record_id", "INVALID", ErrorCode::IdInvalid, Some("stream")),
        (
            "recorded_at",
            "invalid",
            ErrorCode::TimestampInvalid,
            Some("stream"),
        ),
    ] {
        let mut rec = record("a");
        rec[field] = json!(value);
        let s = json!({"store_id":"s","entries":[{"position":7,"record":rec}]});
        for relations in &early {
            failure(
                &[reader(), s.clone(), relations.clone()],
                ErrorCode::LineageInvalid,
                Some("relations"),
                None,
            );
        }
        for relations in &late {
            failure(
                &[reader(), s.clone(), relations.clone()],
                code,
                arg,
                Some(7),
            );
        }
    }
}

#[test]
fn routing_detail_is_complete_and_layer_two_errors_remain_positionless() {
    for profile in ["learning".to_owned(), "agent".to_owned(), "p".repeat(128)] {
        for (kind, edition) in [("mystery", 4), ("attempt", 2)] {
            let r = json!({"profile":profile,"recognized":[{"record_type":"attempt","schema_version":1}]});
            let mut rec = record("u");
            rec["profile"] = json!(profile);
            rec["record_type"] = json!(kind);
            rec["schema_version"] = json!(edition);
            let s = json!({"store_id":"s","entries":[{"position":7,"record":rec}]});
            for seams in 0..27 {
                let e = call(&[r.clone(), s.clone(), json!([])], seams).unwrap_err();
                assert_eq!(
                    (e.code, e.argument.as_deref(), e.position),
                    (ErrorCode::HistoryUnknownRequired, None, Some(7))
                );
                assert_eq!(
                    e.detail,
                    format!("({profile}, {kind}, {edition}) is required but not recognized")
                );
            }
            let error = admit_history_record(
                Input::Native(&support::native(&r)),
                Input::Native(&support::native(&rec)),
            )
            .unwrap_err();
            assert_eq!(
                (error.code, error.argument, error.position, error.detail),
                (ErrorCode::HistoryUnknownRequired, None, None, String::new())
            );
        }
    }
}

#[test]
fn whole_argument_staging_and_shape_order_are_preserved() {
    let reader_bad = Value::object(vec![("profile".into(), Value::number(f64::NAN))]);
    let mut stream_bad = record("a");
    stream_bad["recorded_at"] = json!("bad");
    let s = support::native(&stream(vec![stream_bad]));
    let e = project_history(
        Input::Native(&reader_bad),
        Input::Native(&s),
        Input::Text("[{\"a\":1,\"a\":2}]"),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.argument.as_deref(), e.position),
        (ErrorCode::DuplicateKey, Some("relations"), None)
    );
    let e =
        project_history(Input::Text("{}"), Input::Text("{}"), Input::Text("[1e400]")).unwrap_err();
    assert_eq!(
        (e.code, e.argument.as_deref(), e.position),
        (ErrorCode::NumberNotFinite, Some("relations"), None)
    );
    failure(
        &[json!({}), json!({}), json!([null])],
        ErrorCode::HistoryReaderInvalid,
        Some("reader"),
        None,
    );
    failure(
        &[reader(), json!({}), json!([null])],
        ErrorCode::HistoryStreamInvalid,
        Some("stream"),
        None,
    );
    let e =
        project_history(Input::Text("{}"), Input::Bytes(b"\xff"), Input::Text("[]")).unwrap_err();
    assert_eq!(
        (e.code, e.argument.as_deref(), e.position),
        (ErrorCode::InvalidJson, Some("stream"), None)
    );
    assert!(e.detail.contains("byte"));
}

#[test]
fn canonical_repeats_use_complete_normalized_content() {
    let mut a = record("a");
    a["payload"] = json!({"\u{10000}":1,"\u{e000}":-0.0,"flag":false});
    let mut b = a.clone();
    b["recorded_at"] = json!("2026-10-11t09:00:00+01:00");
    b["payload"]["\u{10000}"] = json!(1.0);
    let args = [reader(), stream(vec![a.clone(), b]), json!([])];
    for seams in 0..27 {
        let out = observed(&call(&args, seams).unwrap());
        assert_eq!(out["duplicates"], json!([{"position":2,"record_id":"a"}]));
        assert_eq!(out["records"].as_array().unwrap().len(), 1);
    }
    for field in ["payload", "provenance"] {
        let mut b = a.clone();
        b[field] = json!(0);
        failure(
            &[reader(), stream(vec![a.clone(), b]), json!([])],
            ErrorCode::HistoryDuplicateRecordId,
            Some("stream"),
            Some(2),
        );
    }
    let mut b = a.clone();
    b["payload"]["flag"] = json!(0);
    failure(
        &[reader(), stream(vec![a, b]), json!([])],
        ErrorCode::HistoryDuplicateRecordId,
        Some("stream"),
        Some(2),
    );
}

#[test]
fn typed_external_targets_sort_and_never_become_record_endpoints() {
    let mut relations = vec![
        relation("b", "derived_from", "zz"),
        relation("b", "derived_from", "a"),
    ];
    for edition in [10, 2] {
        relations.push(json!({"from_record_id":"b","relation":"derived_from","to_artifact":{"stable_id":"ku","artifact_version":edition}}));
    }
    for hash in ["b".repeat(64), "a".repeat(64)] {
        relations.push(json!({"from_record_id":"b","relation":"derived_from","to_artifact":{"content_hash":hash}}));
    }
    let mut duplicate = relations[2].clone();
    duplicate["to_artifact"]["artifact_version"] = json!(10.0);
    relations.push(duplicate);
    let expected = json!([
        relations[1],
        relations[0],
        relations[3],
        relations[2],
        relations[5],
        relations[4]
    ]);
    for _ in 0..relations.len() {
        let out = observed(
            &call(
                &[
                    reader(),
                    stream(vec![record("a"), record("b")]),
                    json!(relations),
                ],
                0,
            )
            .unwrap(),
        );
        assert_eq!(out["relations"], expected);
        assert_eq!(
            out["unresolved_derivations"],
            json!(expected.as_array().unwrap()[1..])
        );
        assert_eq!(out["current"], json!(["a", "b"]));
        relations.rotate_left(1);
    }
    for artifact in [
        json!({"stable_id":"ku"}),
        json!({"stable_id":"ku","artifact_version":0}),
        json!({"content_hash":"A".repeat(64)}),
        json!({}),
        json!({"stable_id":"ku","artifact_version":1,"extra":0}),
    ] {
        failure(
            &[
                reader(),
                stream(vec![record("a")]),
                json!([{"from_record_id":"a","relation":"derived_from","to_artifact":artifact}]),
            ],
            ErrorCode::LineageInvalid,
            Some("relations"),
            None,
        );
    }
}

#[test]
fn presentation_follows_positions_not_ids() {
    let records = vec![record("z"), record("y"), record("a")];
    let mut relations = vec![
        relation("y", "derived_from", "a"),
        relation("z", "derived_from", "a"),
        relation("z", "derived_from", "y"),
    ];
    let expected = json!([
        relation("z", "derived_from", "y"),
        relation("z", "derived_from", "a"),
        relation("y", "derived_from", "a")
    ]);
    for _ in 0..relations.len() {
        let out =
            observed(&call(&[reader(), stream(records.clone()), json!(relations)], 0).unwrap());
        assert_eq!(out["relations"], expected);
        assert_eq!(out["current"], json!(["z", "y", "a"]));
        relations.rotate_left(1);
    }
    for kind in ["supersedes", "corrects", "reinterprets", "derived_from"] {
        failure(
            &[
                reader(),
                stream(vec![record("a")]),
                json!([relation("a", kind, "a")]),
            ],
            ErrorCode::LineageInvalid,
            Some("relations"),
            None,
        );
    }
}

#[test]
fn every_three_record_graph_matches_independent_transitive_closure() {
    let pairs = [(0, 1), (0, 2), (1, 0), (1, 2), (2, 0), (2, 1)];
    let ids = ["a", "b", "c"];
    for mut bits in 0..5usize.pow(6) {
        let mut resolution = [[false; 3]; 3];
        let mut derivation = [[false; 3]; 3];
        let mut replaced = [false; 3];
        let mut interpretations = [[false; 3]; 3];
        let mut relations = vec![];
        for (source, target) in pairs {
            let kind = bits % 5;
            bits /= 5;
            if kind == 0 {
                continue;
            }
            let name = ["", "supersedes", "corrects", "reinterprets", "derived_from"][kind];
            relations.push(relation(ids[source], name, ids[target]));
            if kind == 4 {
                derivation[source][target] = true;
            } else {
                resolution[source][target] = true;
            }
            if kind == 1 || kind == 2 {
                replaced[target] = true;
            }
            if kind == 3 {
                interpretations[target][source] = true;
            }
        }
        for graph in [&mut resolution, &mut derivation] {
            for k in 0..3 {
                for i in 0..3 {
                    for j in 0..3 {
                        graph[i][j] |= graph[i][k] && graph[k][j];
                    }
                }
            }
        }
        let cyclic = (0..3).any(|i| resolution[i][i] || derivation[i][i]);
        let result = call(
            &[
                reader(),
                stream(ids.iter().map(|id| record(id)).collect()),
                json!(relations),
            ],
            0,
        );
        if cyclic {
            let e = result.unwrap_err();
            assert_eq!(
                (e.code, e.argument.as_deref(), e.position),
                (ErrorCode::LineageInvalid, Some("relations"), None)
            );
        } else {
            let result = observed(&result.unwrap());
            let current: Vec<_> = (0..3).filter(|i| !replaced[*i]).map(|i| ids[i]).collect();
            assert_eq!(result["current"], json!(current));
            let wanted:Vec<_>=(0..3).filter(|i|interpretations[*i].iter().any(|v|*v)).map(|i|json!({"observation":ids[i],"current":(0..3).filter(|j|interpretations[i][*j] && !replaced[*j]).map(|j|ids[j]).collect::<Vec<_>>()})).collect();
            assert_eq!(result["interpretations"], json!(wanted));
        }
    }
}

#[test]
fn deep_payload_and_long_graph_are_iterative_and_input_is_owned() {
    let depth = 20_000;
    let payload = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
    let s = format!(
        r#"{{"store_id":"s","entries":[{{"position":1,"record":{{"record_id":"a","profile":"learning","record_type":"attempt","schema_version":1,"compatibility":"required","recorded_at":"2026-10-11T08:00:00Z","payload":{payload}}}}}]}}"#
    );
    let value = project_history(
        Input::Native(&support::native(&reader())),
        Input::Text(&s),
        Input::Text("[]"),
    )
    .unwrap();
    let bytes = canonical_bytes(&value).unwrap();
    assert!(String::from_utf8(bytes).unwrap().contains(&payload));
    let ids: Vec<_> = (0..3000).map(|i| format!("r-{i}")).collect();
    let relations: Vec<_> = (1..ids.len())
        .map(|i| relation(&ids[i], "supersedes", &ids[i - 1]))
        .collect();
    let s = support::native(&stream(ids.iter().map(|id| record(id)).collect()));
    let before = canonical_bytes(&s).unwrap();
    let out = project_history(
        Input::Native(&support::native(&reader())),
        Input::Native(&s),
        Input::Native(&support::native(&json!(relations))),
    )
    .unwrap();
    assert_eq!(canonical_bytes(&s).unwrap(), before);
    let out = observed(&out);
    assert_eq!(out["records"].as_array().unwrap().len(), 3000);
    assert_eq!(out["current"], json!(["r-2999"]));
}
