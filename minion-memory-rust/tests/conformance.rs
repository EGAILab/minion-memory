//! Canonical integration through the public library, with runner self-controls.
mod support;
use minion_memory::*;
use serde_json::{Value as Json, json};
use std::path::{Path, PathBuf};

// Moves only with a Rust implementation candidate that passes the newly admitted corpus.
const IMPLEMENTED_LAYER: u8 = 3;

fn observed_value(value: &Value) -> Result<Json, Error> {
    Ok(serde_json::from_slice(&canonical_bytes(value)?).expect("normalized public value"))
}

fn observe(step: &Json) -> Result<Json, String> {
    let step = step.as_object().ok_or("operation must be a map")?;
    if step.len() != 1 {
        return Err("exactly one operation required".into());
    }
    let (operation, args) = step.iter().next().unwrap();
    if !args.is_object() {
        return Err("operation arguments must be a map".into());
    }
    let field = |key: &str| {
        args.get(key)
            .ok_or_else(|| format!("{operation}: missing argument {key}"))
    };
    let text = |key: &str| {
        args[key]
            .as_str()
            .ok_or_else(|| format!("{operation}: missing string {key}"))
    };
    let result: Result<Json, Error> = match operation.as_str() {
        "canonicalize" => decode_text(text("value_json")?).and_then(|v| Ok(json!({
            "canonical": String::from_utf8(canonical_bytes(&v)?).unwrap(), "sha256": content_hash(&v)? }))),
        "compare_versions" => compare_versions(Input::Native(&support::native(field("a")?)), Input::Native(&support::native(field("b")?)))
            .map(|order| json!({"result": match order { std::cmp::Ordering::Less=>"less",std::cmp::Ordering::Equal=>"equal",std::cmp::Ordering::Greater=>"greater" }})),
        "validate_stable_id" => validate_stable_id(Input::Native(&support::native(field("id")?))).map(|_| json!({"result":"valid"})),
        "normalize_timestamp" => {
            let native = if args.get("value_json").is_some() { Value::null() } else { support::native(field("input")?) };
            let input = if args.get("value_json").is_some() { Input::Text(text("value_json")?) } else { Input::Native(&native) };
            normalize_timestamp(input).map(|t| json!({"canonical":t}))
        }
        "validate_operation_identity" => operation_identity(Input::Text(text("value_json")?)).map(|_| json!({"result":"valid"})),
        "compare_operation_identities" => compare_operation_identities(Input::Text(text("a_json")?), Input::Text(text("b_json")?))
            .map(|same| json!({"result":if same {"same"} else {"different"}})),
        "compare_artifact_revisions" => compare_artifact_revisions(Input::Native(&support::native(field("a")?)), Input::Native(&support::native(field("b")?)))
            .map(|r| json!({"result":match r {RevisionComparison::Identical=>"identical",RevisionComparison::Distinct=>"distinct",RevisionComparison::NotApplicable=>"not_applicable"}})),
        "hash_content" => {
            let hex = text("hex")?;
            if hex.len()%2!=0 || !hex.is_ascii() { return Err("invalid runner hex bytes".into()); }
            let bytes = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i+2],16)).collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
            Ok(json!({"sha256":raw_content_hash(&bytes)}))
        }
        "admit_history_record" => {
            let native = if args.get("record_json").is_some() { Value::null() } else { support::native(field("record")?) };
            let input = if args.get("record_json").is_some() { Input::Text(text("record_json")?) } else { Input::Native(&native) };
            admit_history_record(Input::Native(&support::native(field("reader")?)), input).and_then(|admission| match admission {
                HistoryAdmission::Decode(envelope) => Ok(json!({"outcome":"decode", "envelope":observed_value(&envelope)?})),
                HistoryAdmission::Skip => Ok(json!({"outcome":"skip"})),
                HistoryAdmission::Excluded => Ok(json!({"outcome":"excluded"})),
            })
        }
        "compare_history_boundaries" => compare_history_boundaries(Input::Native(&support::native(field("a")?)), Input::Native(&support::native(field("b")?)))
            .map(|order| json!({"result": match order {std::cmp::Ordering::Less=>"less",std::cmp::Ordering::Equal=>"equal",std::cmp::Ordering::Greater=>"greater"}})),
        "history_boundaries_equal" => history_boundaries_equal(Input::Native(&support::native(field("a")?)), Input::Native(&support::native(field("b")?)))
            .map(|equal| json!({"result":equal})),
        "validate_scope" => validate_scope(Input::Native(&support::native(field("scope")?)))
            .and_then(|scope| Ok(json!({"result":observed_value(&scope)?}))),
        "scopes_equal" => scopes_equal(Input::Native(&support::native(field("a")?)), Input::Native(&support::native(field("b")?)))
            .map(|equal| json!({"result":equal})),
        "scope_visible" => scope_visible(Input::Native(&support::native(field("record")?)), Input::Native(&support::native(field("query")?)))
            .map(|visible| json!({"result":visible})),
        "project_history" => project_history(Input::Native(&support::native(field("reader")?)), Input::Native(&support::native(field("stream")?)), Input::Native(&support::native(field("relations")?)))
            .and_then(|projection| Ok(json!({"projection":observed_value(&projection)?}))),
        _ => return Err(format!("unsupported ACTIVE operation {operation}")),
    };
    Ok(result.unwrap_or_else(|e| {
        let mut result = json!({"error":e.code.as_str()});
        if let Some(argument) = e.argument {
            result["argument"] = json!(argument);
        }
        if let Some(position) = e.position {
            result["position"] = json!(position);
        }
        result
    }))
}
fn compare_step(step: &Json, expected: &Json) -> Result<(), String> {
    let mut observed = observe(step)?;
    let mut expected = expected.clone();
    expected
        .as_object_mut()
        .ok_or("expectation must be a map")?
        .remove("case");
    if expected.get("argument").is_none() {
        observed.as_object_mut().unwrap().remove("argument");
    }
    if observed != expected {
        return Err(format!("observed {observed}, expected {expected}"));
    }
    Ok(())
}
fn paths(directory: &Path, result: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            paths(&path, result);
        } else if path.extension().is_some_and(|s| s == "yaml") {
            result.push(path);
        }
    }
}
#[derive(Debug, PartialEq)]
enum ScenarioOutcome {
    Active { steps: usize },
    ExpectedFailure { reason: String },
    Retired,
}

// The caller validates the shared envelope schema before dispatching any scenario.
// Status controls evidence eligibility only; all ACTIVE steps use the real library.
fn run_scenario(scenario: &Json) -> Result<ScenarioOutcome, String> {
    run_scenario_at_layer(scenario, IMPLEMENTED_LAYER)
}

fn run_scenario_at_layer(
    scenario: &Json,
    implemented_layer: u8,
) -> Result<ScenarioOutcome, String> {
    let status = scenario["status"]
        .as_str()
        .ok_or("scenario status must be a string")?;
    if !["ACTIVE", "DRAFT", "PLACEHOLDER", "RETIRED"].contains(&status) {
        return Err(format!("unknown scenario status {status}"));
    }
    let placeholder = support::placeholder(scenario);
    if status == "ACTIVE" && placeholder {
        return Err("ACTIVE must not have TO_BE_ values".into());
    }
    if status == "DRAFT" {
        let layer = scenario["layer"]
            .as_str()
            .ok_or("DRAFT must name its layer")?;
        return Ok(ScenarioOutcome::ExpectedFailure {
            reason: format!(
                "DRAFT scenario, layer {layer} contract under review (conformance/README.md Drafts rule)"
            ),
        });
    }
    if status == "ACTIVE" {
        let layer_text = scenario["layer"]
            .as_str()
            .ok_or("ACTIVE must name its layer")?;
        if layer_text.len() != 2 || !layer_text.bytes().all(|c| c.is_ascii_digit()) {
            return Err("ACTIVE layer must contain two ASCII digits".into());
        }
        let layer: u8 = layer_text.parse().map_err(|_| "invalid ACTIVE layer")?;
        if layer > implemented_layer {
            return Ok(ScenarioOutcome::ExpectedFailure {
                reason: format!(
                    "ACTIVE scenario, layer {layer_text} above Rust implemented layer {implemented_layer:02} (conformance/README.md Implemented layers rule)"
                ),
            });
        }
    }
    if placeholder {
        return Ok(ScenarioOutcome::ExpectedFailure {
            reason: "recursive TO_BE_ rule (conformance/README.md)".into(),
        });
    }
    if status == "RETIRED" {
        return Ok(ScenarioOutcome::Retired);
    }
    if status != "ACTIVE" {
        return Err("unreviewed non-placeholder scenario".into());
    }
    let when = scenario["when"]
        .as_array()
        .ok_or("ACTIVE when must be an array")?;
    let expected = scenario["expect"]["results"]
        .as_array()
        .ok_or("ACTIVE expects results")?;
    if when.len() != expected.len() {
        return Err("ACTIVE step/expectation lengths differ".into());
    }
    for (index, (step, expected)) in when.iter().zip(expected).enumerate() {
        compare_step(step, expected).map_err(|e| format!("step {index}: {e}"))?;
    }
    Ok(ScenarioOutcome::Active { steps: when.len() })
}

#[test]
fn canonical_corpus() {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../conformance");
    let schema: Json = serde_json::from_str(
        &std::fs::read_to_string(corpus.join("schema/scenario.schema.json")).unwrap(),
    )
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let mut files = vec![];
    for family in ["core", "learning", "agent"] {
        paths(&corpus.join(family), &mut files);
    }
    files.sort();
    let (mut active, mut steps, mut xfails) = (0, 0, 0);
    for path in files {
        let scenario = support::load_yaml(&std::fs::read_to_string(&path).unwrap())
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        validator
            .validate(&scenario)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        match run_scenario(&scenario).unwrap_or_else(|e| panic!("{}: {e}", scenario["id"])) {
            ScenarioOutcome::Active { steps: count } => {
                active += 1;
                steps += count;
            }
            ScenarioOutcome::ExpectedFailure { reason } => {
                xfails += 1;
                println!("xfail {}: {reason}", scenario["id"]);
            }
            ScenarioOutcome::Retired => println!("retired {}", scenario["id"]),
        }
    }
    println!("Rust corpus: {active} ACTIVE, {steps} steps, {xfails} explicit xfails");
    assert!(active > 0, "corpus must exercise ACTIVE scenarios");
}

#[test]
fn draft_rule_and_active_promotion() {
    let mut scenario = json!({
        "scenario_format": 1, "id": "core.runner.draft-control", "family": "core",
        "layer": "01", "status": "DRAFT", "title": "runner control",
        "requirements": ["SER-008"], "given": {},
        "when": [{"unknown_active": {}}], "expect": {"results": [{"result": "wrong"}]}
    });
    let expected = ScenarioOutcome::ExpectedFailure {
        reason:
            "DRAFT scenario, layer 01 contract under review (conformance/README.md Drafts rule)"
                .into(),
    };
    assert_eq!(run_scenario(&scenario), Ok(expected));
    // No operation support can turn a draft into implementation evidence.
    scenario["status"] = json!("ACTIVE");
    assert!(
        run_scenario(&scenario)
            .unwrap_err()
            .contains("unsupported ACTIVE operation")
    );
    scenario["status"] = json!("DRAFT");
    scenario["when"] = json!([{"compare_versions": {"a": 1, "b": 2}}]);
    assert!(matches!(
        run_scenario(&scenario),
        Ok(ScenarioOutcome::ExpectedFailure { .. })
    ));
    // Promotion must compare the real output, even though the same input was previously xfail.
    scenario["status"] = json!("ACTIVE");
    assert!(run_scenario(&scenario).unwrap_err().contains("observed"));
    scenario["expect"]["results"] = json!([{"result": "less"}]);
    assert_eq!(
        run_scenario(&scenario),
        Ok(ScenarioOutcome::Active { steps: 1 })
    );
    scenario["when"]
        .as_array_mut()
        .unwrap()
        .push(json!({"unknown_active": {}}));
    scenario["expect"]["results"]
        .as_array_mut()
        .unwrap()
        .push(json!({"result": "wrong"}));
    assert!(run_scenario(&scenario).unwrap_err().contains("step 1"));
    scenario["expect"]["results"] = json!([]);
    assert!(
        run_scenario(&scenario)
            .unwrap_err()
            .contains("lengths differ")
    );
    scenario["status"] = json!("RETIRED");
    assert_eq!(run_scenario(&scenario), Ok(ScenarioOutcome::Retired));
    scenario["status"] = json!("PLACEHOLDER");
    assert!(run_scenario(&scenario).is_err());
    scenario["when"] = json!([{"placeholder": {"nested": ["TO_BE_OPERATION"]}}]);
    assert!(matches!(
        run_scenario(&scenario),
        Ok(ScenarioOutcome::ExpectedFailure { .. })
    ));
    scenario["status"] = json!("ACTIVE");
    assert!(
        run_scenario(&scenario)
            .unwrap_err()
            .contains("ACTIVE must not have TO_BE_")
    );
    scenario["status"] = json!("DRAFT");
    scenario["layer"] = json!("03");
    match run_scenario(&scenario).unwrap() {
        ScenarioOutcome::ExpectedFailure { reason } => {
            assert!(reason.contains("layer 03") && reason.contains("Drafts rule"));
        }
        other => panic!("draft with a placeholder was not reported as DRAFT: {other:?}"),
    }
}
#[test]
fn implemented_layer_and_promotion() {
    assert_eq!(IMPLEMENTED_LAYER, 3);
    let future = IMPLEMENTED_LAYER + 1;
    let mut scenario = json!({"status":"ACTIVE", "layer":format!("{future:02}"), "when":[{"unknown_active":{}}], "expect":{"results":[{"result":"wrong"}]}});
    let expected = ScenarioOutcome::ExpectedFailure {
        reason: format!(
            "ACTIVE scenario, layer {future:02} above Rust implemented layer {IMPLEMENTED_LAYER:02} (conformance/README.md Implemented layers rule)"
        ),
    };
    assert_eq!(run_scenario(&scenario), Ok(expected));
    assert!(
        run_scenario_at_layer(&scenario, future)
            .unwrap_err()
            .contains("unsupported ACTIVE operation")
    );
    scenario["when"] = json!([{"compare_versions":{"a":1,"b":2}}]);
    assert!(matches!(
        run_scenario(&scenario),
        Ok(ScenarioOutcome::ExpectedFailure { .. })
    ));
    assert!(
        run_scenario_at_layer(&scenario, future)
            .unwrap_err()
            .contains("observed")
    );
    scenario["expect"]["results"] = json!([{"result":"less"}]);
    assert_eq!(
        run_scenario_at_layer(&scenario, future),
        Ok(ScenarioOutcome::Active { steps: 1 })
    );
    scenario["layer"] = json!("01");
    assert_eq!(
        run_scenario(&scenario),
        Ok(ScenarioOutcome::Active { steps: 1 })
    );
    scenario["layer"] = json!("00");
    assert_eq!(
        run_scenario(&scenario),
        Ok(ScenarioOutcome::Active { steps: 1 })
    );
    scenario["layer"] = json!("02");
    scenario["when"] = json!([{"unknown_active":{"x":"TO_BE_OPERATION"}}]);
    assert!(
        run_scenario(&scenario)
            .unwrap_err()
            .contains("ACTIVE must not have TO_BE_")
    );
    scenario["when"] = json!([]);
    scenario["layer"] = json!("xx");
    assert!(run_scenario(&scenario).is_err());
}

#[test]
fn runner_self_controls() {
    assert!(support::placeholder(
        &json!({"nested":[{"x":"TO_BE_OPERATION"}]})
    ));
    assert!(!support::placeholder(&json!({"TO_BE_KEY":"ordinary"})));
    assert!(observe(&json!({"unknown_active":{}})).is_err());
    assert!(observe(&json!({"compare_versions":{"a":1}})).is_err());
    let baseline = json!({"canonicalize":{"value_json":"{\"a\":1}"}});
    let expected = json!({"canonical":"{\"a\":1}","sha256":"015abd7f5cc57a2dd94b7590f04ad8084273905ee33ec5cebeae62276a97f862"});
    compare_step(&baseline, &expected).unwrap();
    let mut wrong = expected.clone();
    wrong["canonical"] = json!("{ \"a\": 1 }");
    assert!(
        compare_step(&baseline, &wrong).is_err(),
        "known-bad pretty printing killed"
    );
    wrong = expected.clone();
    wrong["sha256"] = json!("015ABD7F5CC57A2DD94B7590F04AD8084273905EE33EC5CEBEAE62276A97F862");
    assert!(
        compare_step(&baseline, &wrong).is_err(),
        "uppercase digest killed"
    );
}
#[test]
fn portable_yaml_controls() {
    for bad in [
        "x: &unused 1",
        "x: &a [1]\ny: *a",
        "x: !!str foo",
        "x: 1\nx: 2",
        "x: yes",
        "x: 010",
        "x: 2026-10-09",
        "x: 1e3",
        "x: .inf",
        "1: value",
        "x: [1\n",
        "x: 1\n---\ny: 2",
        "",
    ] {
        assert!(support::load_yaml(bad).is_err(), "accepted {bad:?}");
    }
    assert_eq!(
        support::load_yaml("x: 'yes'\ny: 1.5\nz: false").unwrap(),
        json!({"x":"yes","y":1.5,"z":false})
    );
}
