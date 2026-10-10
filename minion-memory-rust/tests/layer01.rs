//! Independent contract-derived controls for every Layer 01 public seam.
use minion_memory::*;
use std::cmp::Ordering;

fn code<T: std::fmt::Debug>(value: Result<T, Error>, expected: ErrorCode) {
    let error = value.unwrap_err();
    assert_eq!(error.code, expected, "{error}");
    assert!(error.detail.len() < 80);
}
fn canonical(text: &str) -> String {
    String::from_utf8(canonical_bytes(&decode_text(text).unwrap()).unwrap()).unwrap()
}
#[test]
fn structural_source_order_and_deferred_numbers() {
    for (text, expected) in [
        (r#"{"a":1e400,"b":1,"b":2}"#, ErrorCode::DuplicateKey),
        (r#"[9007199254740992,"\ud800"]"#, ErrorCode::LoneSurrogate),
        (r#"{"a":1,"\u0061":2}"#, ErrorCode::DuplicateKey),
        (r#"{"a":1,"a": "\ud800"}"#, ErrorCode::DuplicateKey),
        (r#"{"a":"\ud800","a":2}"#, ErrorCode::LoneSurrogate),
        (r#"["\ud800", NaN]"#, ErrorCode::LoneSurrogate),
        (r#"[NaN,"\ud800"]"#, ErrorCode::InvalidJson),
        (r#"[1e400,]"#, ErrorCode::InvalidJson),
        (r#"{"a":1,"a"}"#, ErrorCode::DuplicateKey),
        (r#"{"a":1,"a\ud800":2}"#, ErrorCode::LoneSurrogate),
        (r#"["\ud800\uZZZZ"]"#, ErrorCode::LoneSurrogate),
        (r#"["\uZZZZ\ud800"]"#, ErrorCode::InvalidJson),
    ] {
        code(decode_text(text), expected);
    }
    for bytes in [
        b"[1e400,\xff]".as_slice(),
        b"\xed\xa0\x80",
        b"{\"a\xff\":1,\"a\":2}",
    ] {
        code(decode_json(bytes), ErrorCode::InvalidJson);
    }
    code(decode_json(b"[\"\\ud800\",\xff]"), ErrorCode::LoneSurrogate);
    code(
        decode_json(b"{\"a\":1,\"a\":2,\xff}"),
        ErrorCode::DuplicateKey,
    );
    assert_eq!(
        canonical(r#"["\ud83d\ude00",{},[],"é"]"#),
        "[\"😀\",{},[],\"é\"]"
    );
}
#[test]
fn strict_json_syntax() {
    for bad in [
        "",
        " ",
        "NaN",
        "Infinity",
        "-Infinity",
        "undefined",
        "01",
        "-01",
        "1.",
        "1e",
        "1e+",
        "+1",
        "[1,]",
        "[,1]",
        "{\"a\":1,}",
        "{a:1}",
        "true false",
        "[",
        "{",
        "\"",
        "\"\\x41\"",
        "\"\u{1}\"",
        "// comment\n1",
        "[1 2]",
        "{\"a\" 1}",
        "tru",
        "\u{feff}1",
    ] {
        code(decode_text(bad), ErrorCode::InvalidJson);
    }
    assert_eq!(
        canonical(" \t\n\r [ 1 , { \"a\" : true } ] \r"),
        "[1,{\"a\":true}]"
    );
    for unit in [r#""\ud800""#, r#""\udfff""#, r#""\udbff\u0041""#] {
        code(decode_text(unit), ErrorCode::LoneSurrogate);
    }
}
#[test]
fn numeric_normalization_exact_native_and_rounding() {
    for (text, expected) in [
        ("-0", "0"),
        ("-0.0", "0"),
        ("-1e-400", "0"),
        ("1e-400", "0"),
        ("5e-324", "5e-324"),
        ("1.0000000000000001", "1"),
        ("9007199254740990.5", "9007199254740990"),
        ("1.234567890123456789", "1.2345678901234567"),
        ("0.000001", "0.000001"),
        ("0.0000009999999999999999", "0.000001"),
        ("1.5e-7", "1.5e-7"),
    ] {
        assert_eq!(canonical(text), expected, "{text}");
    }
    for text in [
        "9007199254740991.5",
        "9007199254740992",
        "9007199254740992.0",
        "9.007199254740992e15",
        "1e21",
        "-1e21",
        "1.7976931348623157e308",
    ] {
        code(decode_text(text), ErrorCode::IntegerOutOfRange);
    }
    for text in ["1e400", "-1e400", "1.7976931348623159e308"] {
        code(decode_text(text), ErrorCode::NumberNotFinite);
    }
    for n in [
        i128::MIN,
        i128::MAX,
        SAFE_INTEGER_MAX + 1,
        -SAFE_INTEGER_MAX - 1,
    ] {
        code(
            validate_value(&Value::integer(n)),
            ErrorCode::IntegerOutOfRange,
        );
    }
    for n in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        code(
            validate_value(&Value::number(n)),
            ErrorCode::NumberNotFinite,
        );
    }
    assert_eq!(
        validate_value(&Value::number(-0.0)).unwrap().as_integer(),
        Some(0)
    );
    assert_eq!(canonical(&format!("0e{}", "9".repeat(10_000))), "0");
    assert_eq!(canonical(&format!("1e-{}", "9".repeat(10_000))), "0");
    code(
        decode_text(&format!("1e{}", "9".repeat(10_000))),
        ErrorCode::NumberNotFinite,
    );
}
#[test]
fn stages_across_arguments_and_canonical_traversal() {
    let e = compare_versions(Input::Text("1e400"), Input::Text(r#"{"x":1,"x":2}"#)).unwrap_err();
    assert_eq!(
        (e.code, e.argument.as_deref()),
        (ErrorCode::DuplicateKey, Some("b"))
    );
    let e = compare_versions(Input::Text("0"), Input::Text("1e400")).unwrap_err();
    assert_eq!(
        (e.code, e.argument.as_deref()),
        (ErrorCode::NumberNotFinite, Some("b"))
    );
    let e = compare_operation_identities(Input::Text("null"), Input::Text("1e400")).unwrap_err();
    assert_eq!(
        (e.code, e.argument.as_deref()),
        (ErrorCode::NumberNotFinite, Some("b"))
    );
    let e = compare_artifact_revisions(Input::Text("null"), Input::Text(r#"{"x":1,"x":2}"#))
        .unwrap_err();
    assert_eq!(
        (e.code, e.argument.as_deref()),
        (ErrorCode::DuplicateKey, Some("b"))
    );
    for text in [
        r#"{"\uff61":1e400,"\ud83d\ude00":9007199254740992}"#,
        r#"{"\ud83d\ude00":9007199254740992,"\uff61":1e400}"#,
    ] {
        code(decode_text(text), ErrorCode::IntegerOutOfRange);
    }
    for text in [
        r#"[9007199254740992,1e400]"#,
        r#"{"a":[9007199254740992],"b":1e400}"#,
    ] {
        code(decode_text(text), ErrorCode::IntegerOutOfRange);
    }
    let duplicate = Value::object(vec![
        ("same".into(), Value::null()),
        ("same".into(), Value::null()),
    ]);
    let native = Value::object(vec![
        ("a".into(), Value::number(f64::NAN)),
        ("z".into(), duplicate),
    ]);
    code(validate_value(&native), ErrorCode::DuplicateKey);
    let native = Value::object(vec![
        ("｡".into(), Value::number(f64::NAN)),
        ("😀".into(), Value::integer(SAFE_INTEGER_MAX + 1)),
    ]);
    code(validate_value(&native), ErrorCode::IntegerOutOfRange);
    assert!(validate_arguments(&[]).unwrap().is_empty());
}
#[test]
fn utf16_order_escaping_and_hashes() {
    assert_eq!(
        canonical(r#"{"\ufffd":0,"\ud83d\ude00":1,"\ue000":2,"\r":3,"1":4,"\u0080":5,"é":6}"#),
        "{\"\\r\":3,\"1\":4,\"\u{80}\":5,\"é\":6,\"😀\":1,\"\u{e000}\":2,\"�\":0}"
    );
    let all = (0..32)
        .map(|c| char::from_u32(c).unwrap())
        .collect::<String>()
        + "\"\\/é😀";
    let expected = r#""\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\b\t\n\u000b\f\r\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f\"\\/é😀""#;
    assert_eq!(
        String::from_utf8(canonical_bytes(&Value::string(&all)).unwrap()).unwrap(),
        expected
    );
    assert_eq!(
        raw_content_hash(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_ne!(raw_content_hash(b"line\r\n"), raw_content_hash(b"line\n"));
    assert_ne!(
        content_hash(&Value::string("é")).unwrap(),
        content_hash(&Value::string("e\u{301}")).unwrap()
    );
    let v = decode_text("{\"v\":1e0}").unwrap();
    assert_eq!(
        content_hash(&v).unwrap(),
        "afbf9d0f3560b0fd7795e81c42a0a79ee6b6fc67e064f77826aee642cad28d91"
    );
}
#[test]
fn stable_id_and_version_neighborhoods() {
    for s in [
        "a", "0", "nulled", "com0", "com10", "lpt0", "lpt10", "a..b", "a-_",
    ] {
        assert_eq!(
            validate_stable_id(Input::Native(&Value::string(s))).unwrap(),
            s
        );
    }
    for s in [
        "", "A", "é", ".x", "-x", "_x", "a.", "con", "prn", "aux", "nul", "con.txt", "lpt1.a",
        "com9_", "a/b", "a\\b", "a b",
    ] {
        // com9_ is legal: only the exact basename is a reserved device.
        if s == "com9_" {
            assert!(validate_stable_id(Input::Native(&Value::string(s))).is_ok());
        } else {
            code(
                validate_stable_id(Input::Native(&Value::string(s))),
                ErrorCode::IdInvalid,
            );
        }
    }
    assert!(validate_stable_id(Input::Native(&Value::string(&"x".repeat(128)))).is_ok());
    code(
        validate_stable_id(Input::Native(&Value::string(&"x".repeat(129)))),
        ErrorCode::IdInvalid,
    );
    for value in [
        Value::boolean(true),
        Value::null(),
        Value::string("1"),
        Value::number(0.5),
        Value::integer(0),
        Value::integer(-1),
        Value::array(vec![]),
    ] {
        code(
            validate_version(Input::Native(&value)),
            ErrorCode::VersionInvalid,
        );
    }
    assert_eq!(
        compare_versions(Input::Text("2.0"), Input::Text("2e0")).unwrap(),
        Ordering::Equal
    );
    assert_eq!(
        validate_version(Input::Native(&Value::integer(SAFE_INTEGER_MAX))).unwrap(),
        SAFE_INTEGER_MAX
    );
}
#[test]
fn timestamp_grammar_before_calendar_arithmetic() {
    for (s, expected) in [
        ("0001-01-01t00:00:00.1z", "0001-01-01T00:00:00.100000Z"),
        ("9999-12-31T23:59:59.999999Z", "9999-12-31T23:59:59.999999Z"),
        ("2000-03-01T00:00:00+00:01", "2000-02-29T23:59:00.000000Z"),
        ("1900-03-01T00:00:00+00:01", "1900-02-28T23:59:00.000000Z"),
        ("2026-10-11T23:59:00+23:59", "2026-10-11T00:00:00.000000Z"),
    ] {
        assert_eq!(
            normalize_timestamp(Input::Native(&Value::string(s))).unwrap(),
            expected
        );
    }
    for s in [
        "0000-01-01T00:00:00Z",
        "0001-01-01T00:00:00+00:01",
        "9999-12-31T23:59:59-00:01",
        "2000-01-01T00:00:60Z",
        "2000-01-01T24:00:00Z",
        "1900-02-29T00:00:00Z",
        "2000-01-01T00:00:00.0000000Z",
        "2000-01-01T00:00:00.Z",
        "2000-01-01 00:00:00Z",
        "2000-01-01T00:00:00+24:00",
        "2000-01-01T00:00:00+00:60",
        "2000-01-01T00:00:00Z\n",
        "+2000-01-01T00:00:00Z",
        "2000-1-1T00:00:00Z",
    ] {
        code(
            normalize_timestamp(Input::Native(&Value::string(s))),
            ErrorCode::TimestampInvalid,
        );
    }
    code(
        normalize_timestamp(Input::Text("1e400")),
        ErrorCode::NumberNotFinite,
    );
    code(
        normalize_timestamp(Input::Text("true")),
        ErrorCode::TimestampInvalid,
    );
}
fn operation(id: &str) -> Value {
    Value::object(vec![
        ("source_namespace".into(), Value::string("ns")),
        ("source_operation_id".into(), Value::string(id)),
    ])
}
#[test]
fn opaque_operation_pair_and_closed_shape() {
    assert_eq!(
        operation_identity(Input::Native(&operation("\u{0}")))
            .unwrap()
            .source_operation_id,
        "\u{0}"
    );
    for good in [" ".to_owned(), "é".repeat(128), "😀".repeat(64)] {
        assert!(operation_identity(Input::Native(&operation(&good))).is_ok());
    }
    for bad in ["".to_owned(), "é".repeat(129), "😀".repeat(65)] {
        code(
            operation_identity(Input::Native(&operation(&bad))),
            ErrorCode::OperationIdentityInvalid,
        );
    }
    assert!(
        !compare_operation_identities(
            Input::Native(&operation("é")),
            Input::Native(&operation("e\u{301}"))
        )
        .unwrap()
    );
    code(
        operation_identity(Input::Text(
            r#"{"source_namespace":"ns","source_operation_id":"x","extra":0}"#,
        )),
        ErrorCode::OperationIdentityInvalid,
    );
    code(
        operation_identity(Input::Text(
            r#"{"source_namespace":"ns","source_operation_id":"x","extra":1e400}"#,
        )),
        ErrorCode::NumberNotFinite,
    );
}
#[test]
fn immutable_revisions_validate_all_before_comparing() {
    let a = r#"{"stable_id":"a","projection":true}"#;
    let b = r#"{"stable_id":"a","projection":1,"artifact_version":1}"#;
    assert_eq!(
        compare_artifact_revisions(Input::Text(a), Input::Text(b)).unwrap(),
        RevisionComparison::NotApplicable
    );
    code(
        compare_artifact_revisions(
            Input::Text(a),
            Input::Text(r#"{"stable_id":"BAD","projection":null}"#),
        ),
        ErrorCode::IdInvalid,
    );
    code(
        compare_artifact_revisions(
            Input::Text(r#"{"stable_id":"BAD","artifact_version":0,"projection":null}"#),
            Input::Text(b),
        ),
        ErrorCode::VersionInvalid,
    );
    code(
        compare_artifact_revisions(
            Input::Text(r#"{"stable_id":"BAD","artifact_version":0,"projection":null,"extra":0}"#),
            Input::Text(b),
        ),
        ErrorCode::RevisionInvalid,
    );
    code(
        compare_artifact_revisions(
            Input::Text(r#"{"stable_id":"a","projection":true,"artifact_version":1}"#),
            Input::Text(b),
        ),
        ErrorCode::RevisionConflict,
    );
    assert_eq!(
        compare_artifact_revisions(
            Input::Text(b),
            Input::Text(r#"{"projection":1.0,"artifact_version":1e0,"stable_id":"a"}"#)
        )
        .unwrap(),
        RevisionComparison::Identical
    );
    assert_eq!(
        compare_artifact_revisions(
            Input::Text(b),
            Input::Text(r#"{"stable_id":"b","projection":null,"artifact_version":1}"#)
        )
        .unwrap(),
        RevisionComparison::Distinct
    );
}
#[test]
fn arbitrary_depth_and_bounded_diagnostics() {
    let text = "[".repeat(100_000) + "0" + &"]".repeat(100_000);
    let value = decode_text(&text).unwrap();
    assert_eq!(canonical_bytes(&value.clone()).unwrap(), text.as_bytes());
    code(
        validate_version(Input::Native(&value)),
        ErrorCode::VersionInvalid,
    );
    assert!(format!("{value:?}").len() > 100_000); // flat debug, not recursive graph traversal
    drop(value);
}
#[test]
fn realistic_known_bad_controls_have_passing_baselines() {
    let text = r#"{"\uff61":1,"\ud83d\ude00":2}"#;
    let correct = "{\"😀\":2,\"｡\":1}";
    assert_eq!(canonical(text), correct);
    let bad =
        serde_json::to_string(&serde_json::from_str::<serde_json::Value>(text).unwrap()).unwrap();
    assert_ne!(
        bad, correct,
        "code-point serializer killed by UTF-16 witness"
    );
    code(decode_text(r#"{"a":1,"a":2}"#), ErrorCode::DuplicateKey);
    assert!(
        serde_json::from_str::<serde_json::Value>(r#"{"a":1,"a":2}"#).is_ok(),
        "last-key-wins parser is actually wrong"
    );
    code(
        decode_text("9007199254740992"),
        ErrorCode::IntegerOutOfRange,
    );
    assert!(
        serde_json::from_str::<serde_json::Value>("9007199254740992").is_ok(),
        "unvalidated parser admits the forbidden number"
    );
    code(
        decode_text(r#"{"a":1e400,"b":1,"b":2}"#),
        ErrorCode::DuplicateKey,
    );
    code(decode_text("[1e400,]"), ErrorCode::InvalidJson);
    assert!(
        serde_json::from_str::<Vec<f64>>("[1e400,]")
            .unwrap_err()
            .to_string()
            .contains("number out of range"),
        "eager numeric decoder fails before reaching a later structural violation"
    );
    code(
        normalize_timestamp(Input::Native(&Value::string("2000-01-01 00:00:00Z"))),
        ErrorCode::TimestampInvalid,
    );
    assert!(
        chrono::NaiveDateTime::parse_from_str("2000-01-01 00:00:00", "%Y-%m-%d %H:%M:%S").is_ok(),
        "permissive host timestamp parser discriminated"
    );
}
#[test]
fn independent_ecmascript_binary64_vectors() {
    // Independent Node 22.15.1 JSON.stringify + Mneme's admission rule, fixed seed.
    let vectors: serde_json::Value =
        serde_json::from_str(include_str!("vectors/binary64.json")).unwrap();
    for vector in vectors.as_array().unwrap() {
        let bits = u64::from_str_radix(vector["bits"].as_str().unwrap(), 16).unwrap();
        let expected = vector["expected"].as_str().unwrap();
        let observed = canonical_bytes(&Value::number(f64::from_bits(bits)))
            .map(|v| String::from_utf8(v).unwrap())
            .unwrap_or_else(|e| e.code.as_str().into());
        assert_eq!(observed, expected, "bits {bits:016x}");
        if let Some(text) = vector["text"].as_str() {
            let observed = decode_text(text)
                .and_then(|v| canonical_bytes(&v))
                .map(|v| String::from_utf8(v).unwrap())
                .unwrap_or_else(|e| e.code.as_str().into());
            assert_eq!(observed, expected, "decimal {text}, bits {bits:016x}");
        }
    }
    let values = [Value::integer(1), Value::null()];
    assert_eq!(
        canonical_bytes(&Value::array_from_slice(&values)).unwrap(),
        b"[1,null]"
    );
}
#[test]
fn typed_known_bad_controls_prove_their_baseline() {
    assert_eq!(
        compare_versions(Input::Text("2"), Input::Text("10")).unwrap(),
        Ordering::Less
    );
    assert_ne!("2".cmp("10"), Ordering::Less, "lexical editions killed");
    code(
        validate_stable_id(Input::Native(&Value::string("KU-1"))),
        ErrorCode::IdInvalid,
    );
    assert!(
        validate_stable_id(Input::Native(&Value::string(&"KU-1".to_lowercase()))).is_ok(),
        "case-folding adapter would falsely accept"
    );
    let long = "é".repeat(129);
    code(
        operation_identity(Input::Native(&operation(&long))),
        ErrorCode::OperationIdentityInvalid,
    );
    assert!(
        long.chars().count() <= 256,
        "character-count implementation falsely admits >256 bytes"
    );
    let a = r#"{"stable_id":"a","projection":1,"artifact_version":1}"#;
    let b = r#"{"stable_id":"a","projection":1.0,"artifact_version":1}"#;
    assert_eq!(
        compare_artifact_revisions(Input::Text(a), Input::Text(b)).unwrap(),
        RevisionComparison::Identical
    );
    assert_ne!(
        a.as_bytes(),
        b.as_bytes(),
        "raw-text equality wrongly reports a conflict"
    );
    code(
        compare_artifact_revisions(
            Input::Text(a),
            Input::Text(r#"{"stable_id":"a","projection":2,"artifact_version":1}"#),
        ),
        ErrorCode::RevisionConflict,
    );
    let changed = r#"{"stable_id":"a","projection":2,"artifact_version":1}"#;
    let expected: Result<RevisionComparison, ErrorCode> = Err(ErrorCode::RevisionConflict);
    let bad_last_write_wins =
        compare_artifact_revisions(Input::Text(changed), Input::Text(changed)).map_err(|e| e.code);
    assert_ne!(
        bad_last_write_wins, expected,
        "overwriting the first revision before comparison hides conflict"
    );
    assert_ne!(
        raw_content_hash(b"line\r\n"),
        raw_content_hash(b"line\n"),
        "line-ending-normalizing hash killed"
    );
}
