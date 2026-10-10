//! Canonical integration through the public library, with runner self-controls.
mod support;
use minion_memory::*;
use serde_json::{Value as Json, json};
use std::path::{Path, PathBuf};

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
        _ => return Err(format!("unsupported ACTIVE operation {operation}")),
    };
    Ok(result.unwrap_or_else(|e| {
        let mut result = json!({"error":e.code.as_str()});
        if let Some(argument) = e.argument {
            result["argument"] = json!(argument);
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
        if support::placeholder(&scenario) {
            assert_ne!(
                scenario["status"], "ACTIVE",
                "ACTIVE must not have TO_BE_ values"
            );
            xfails += 1;
            println!(
                "xfail {}: recursive TO_BE_ rule (conformance/README.md)",
                scenario["id"]
            );
            continue;
        }
        if scenario["status"] == "RETIRED" {
            println!("retired {}", scenario["id"]);
            continue;
        }
        assert_eq!(
            scenario["status"], "ACTIVE",
            "unreviewed non-placeholder scenario"
        );
        active += 1;
        let when = scenario["when"].as_array().unwrap();
        let expected = scenario["expect"]["results"].as_array().unwrap();
        assert_eq!(when.len(), expected.len());
        for (index, (step, expected)) in when.iter().zip(expected).enumerate() {
            compare_step(step, expected)
                .unwrap_or_else(|e| panic!("{} step {index}: {e}", scenario["id"]));
            steps += 1;
        }
    }
    println!("Rust corpus: {active} ACTIVE, {steps} steps, {xfails} explicit xfails");
    assert_eq!((active, steps, xfails), (8, 146, 5));
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
