//! Contract-derived Layer 04 controls: stages, transactions, crash, concurrency and catalog.
#[path = "support/pg.rs"]
mod pg;
#[allow(dead_code)] // This integration target uses only the shared native-value constructor.
mod support;
use minion_memory::persistence::{
    PostgresHistoryStore as Store, SCHEMA_OPEN_LOCK_KEY, StoreError, catalog,
};
use minion_memory::*;
use postgres::{Client, NoTls};
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;
use std::sync::{
    Arc, Barrier,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

const T: &str = "2026-10-11T08:00:00.000000Z";
const NORMATIVE: &str = r#"
CREATE TABLE mneme_schema(singleton boolean PRIMARY KEY DEFAULT true CHECK(singleton),version integer NOT NULL);
CREATE TABLE mneme_store(store_id text PRIMARY KEY,tail bigint NOT NULL DEFAULT 0 CHECK(tail BETWEEN 0 AND 9007199254740991));
CREATE TABLE mneme_record(store_id text NOT NULL REFERENCES mneme_store,position bigint NOT NULL CHECK(position BETWEEN 1 AND 9007199254740991),record_id text NOT NULL,profile text NOT NULL,record_type text NOT NULL,schema_version bigint NOT NULL,envelope bytea NOT NULL,PRIMARY KEY(store_id,position),UNIQUE(store_id,record_id));
CREATE TABLE mneme_operation(store_id text NOT NULL REFERENCES mneme_store,source_namespace text NOT NULL,source_operation_id bytea NOT NULL,mutation bytea NOT NULL,first_position bigint NOT NULL,last_position bigint NOT NULL,recorded_at text NOT NULL,PRIMARY KEY(store_id,source_namespace,source_operation_id));
INSERT INTO mneme_schema(version) VALUES(1);
"#;
fn native(v: &Json) -> Value {
    support::native(v)
}
fn value(v: &Value) -> Json {
    serde_json::from_slice(&canonical_bytes(v).unwrap()).unwrap()
}
fn writer() -> Json {
    json!({"profile":"learning","recognized":[{"record_type":"attempt","schema_version":1}]})
}
fn operation(id: &str) -> Json {
    json!({"source_namespace":"app","source_operation_id":id})
}
fn draft(id: &str) -> Json {
    json!({"record_id":id,"profile":"learning","record_type":"attempt","schema_version":1,"compatibility":"required"})
}
fn append(s: &Store, id: &str, op: &str, records: &Json) -> Result<Value, StoreError> {
    s.commit_history(
        Input::Native(&Value::string(id)),
        Input::Native(&native(&writer())),
        Input::Native(&native(&operation(op))),
        Input::Native(&native(records)),
    )
}
fn error(e: StoreError, code: ErrorCode, arg: Option<&str>) {
    let StoreError::Validation(e) = e else {
        panic!("contract error expected: {e}")
    };
    assert_eq!(
        (e.code, e.argument.as_deref(), e.position),
        (code, arg, None)
    );
}
fn namespace(c: &mut Client) -> u32 {
    c.query_one(
        "SELECT pg_catalog.current_schema()::pg_catalog.regnamespace::pg_catalog.oid",
        &[],
    )
    .unwrap()
    .get(0)
}
fn protocol(s: &Store, large: bool) {
    s.create_store(Input::Text("\"s\"")).unwrap();
    let mut rows = json!([draft("r1"), draft("r2")]);
    if large {
        rows[0]["payload"] = json!({"text":"x".repeat(24000),"nul":"a\u{0}b"});
    }
    let a = append(s, "s", "one", &rows).unwrap();
    assert_eq!(value(&a)["first_position"], 1);
    assert_eq!(value(&a)["last_position"], 2);
    assert_eq!(value(&append(s, "s", "one", &rows).unwrap()), value(&a));
    let page = s
        .read_history(
            Input::Text("\"s\""),
            Input::Text(r#"{"store_id":"s","position":0}"#),
            Input::Text("10"),
        )
        .unwrap();
    let page = value(&page);
    assert_eq!(page["entries"][0]["position"], 1);
    assert_eq!(page["entries"][1]["position"], 2);
    assert_eq!(page["through"], json!({"store_id":"s","position":2}));
    assert_eq!(page["tail"], page["through"]);
    if large {
        assert_eq!(page["entries"][0]["record"]["payload"], rows[0]["payload"]);
    }
}

#[test]
fn normative_pins_initializer_and_embedding() {
    let b = pg::Backend::new();
    let mut c = b.client();
    c.batch_execute(NORMATIVE).unwrap();
    let oid = namespace(&mut c);
    let fetched = catalog::fetch(&mut c, oid).unwrap();
    for part in catalog::PARTS {
        let actual = fetched.rows();
        let expected = catalog::pins().rows();
        assert_eq!(actual[part], expected[part], "normative [PIN] {part}");
    }
    let version: i32 = c
        .query_one("SELECT version FROM mneme_schema", &[])
        .unwrap()
        .get(0);
    assert_eq!(version, 1);
    protocol(
        &Store::open(&b.conninfo, |_| Value::string(T)).unwrap(),
        true,
    );
    let b2 = pg::Backend::new();
    drop(Store::open(&b2.conninfo, |_| Value::string(T)).unwrap());
    let mut c = b2.client();
    let oid = namespace(&mut c);
    assert_eq!(&catalog::fetch(&mut c, oid).unwrap(), catalog::pins());
    let file = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../reference-backend/postgres14-v1-catalog.json"),
    )
    .unwrap();
    assert_eq!(file, catalog::SHARED_PINS);
}

#[test]
fn stages_across_every_argument_before_database_or_clock() {
    let b = pg::Backend::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let s = Store::open(&b.conninfo, move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        Value::string(T)
    })
    .unwrap();
    let malformed = [r#"{"x":1,"x":2}"#, r#"{"x":"\uD800"}"#, r#"["#];
    for text in malformed {
        let e = s
            .commit_history(
                Input::Text("\"BAD\""),
                Input::Text("null"),
                Input::Text("null"),
                Input::Text(text),
            )
            .unwrap_err();
        let StoreError::Validation(e) = e else {
            panic!("validation")
        };
        assert_eq!(e.argument.as_deref(), Some("records"));
        assert!(matches!(
            e.code,
            ErrorCode::DuplicateKey | ErrorCode::LoneSurrogate | ErrorCode::InvalidJson
        ));
        assert_eq!(e.position, None);
    }
    error(
        s.create_store(Input::Text("1e9999")).unwrap_err(),
        ErrorCode::NumberNotFinite,
        Some("store_id"),
    );
    error(
        s.store_tail(Input::Bytes(&[0xff])).unwrap_err(),
        ErrorCode::InvalidJson,
        Some("store_id"),
    );
    error(
        s.commit_history(
            Input::Text("\"BAD\""),
            Input::Text("null"),
            Input::Text("null"),
            Input::Text("[]"),
        )
        .unwrap_err(),
        ErrorCode::IdInvalid,
        Some("store_id"),
    );
    error(
        s.commit_history(
            Input::Text("\"s\""),
            Input::Text("null"),
            Input::Text("null"),
            Input::Text("[]"),
        )
        .unwrap_err(),
        ErrorCode::HistoryReaderInvalid,
        Some("writer"),
    );
    error(
        s.read_history(
            Input::Text("\"absent\""),
            Input::Text(r#"{"store_id":"other","position":0}"#),
            Input::Text("0"),
        )
        .unwrap_err(),
        ErrorCode::PersistenceLimitInvalid,
        Some("limit"),
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn draft_gates_and_array_order_before_duplicates_or_store() {
    let b = pg::Backend::new();
    let s = Store::open(&b.conninfo, |_| Value::string(T)).unwrap();
    for rows in [json!([]), json!({}), Json::Null] {
        error(
            append(&s, "absent", "x", &rows).unwrap_err(),
            ErrorCode::PersistenceBatchInvalid,
            Some("records"),
        );
    }
    let mut bad = draft("BAD");
    bad["recorded_at"] = json!(T);
    error(
        append(&s, "absent", "x", &json!([bad])).unwrap_err(),
        ErrorCode::HistoryEnvelopeInvalid,
        Some("records"),
    );
    let mut foreign = draft("BAD");
    foreign["profile"] = json!("agent");
    foreign["recorded_at"] = json!(T);
    error(
        append(&s, "absent", "x", &json!([foreign])).unwrap_err(),
        ErrorCode::PersistenceRecordUnrecognized,
        Some("records"),
    );
    let mut earlier = draft("r");
    earlier["occurred_at"] = json!("bad");
    earlier["record_id"] = json!("BAD");
    error(
        append(&s, "absent", "x", &json!([earlier])).unwrap_err(),
        ErrorCode::TimestampInvalid,
        Some("records"),
    );
    error(
        append(&s, "absent", "x", &json!([draft("r"), draft("r")])).unwrap_err(),
        ErrorCode::HistoryDuplicateRecordId,
        Some("records"),
    );
    let mut unknown = draft("r");
    unknown["record_type"] = json!("unknown");
    for compatibility in ["required", "ignorable"] {
        unknown["compatibility"] = json!(compatibility);
        error(
            append(&s, "absent", "x", &json!([unknown])).unwrap_err(),
            ErrorCode::PersistenceRecordUnrecognized,
            Some("records"),
        );
    }
}

#[test]
fn open_version_rows_exactly_one_and_supported() {
    for corrupt in [
        "UPDATE mneme_schema SET version=2",
        "DELETE FROM mneme_schema",
    ] {
        let b = pg::Backend::new();
        let mut c = b.client();
        c.batch_execute(NORMATIVE).unwrap();
        c.batch_execute(corrupt).unwrap();
        error(
            Store::open(&b.conninfo, |_| Value::string(T))
                .err()
                .expect("invalid version rows refused"),
            ErrorCode::PersistenceSchemaIncompatible,
            None,
        );
    }
}

#[test]
fn capacity_retry_conflict_duplicates_and_clock_order() {
    let b = pg::Backend::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let s = Store::open(&b.conninfo, move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
        Value::string(T)
    })
    .unwrap();
    s.create_store(Input::Text("\"s\"")).unwrap();
    let first = append(&s, "s", "a", &json!([draft("r1")])).unwrap();
    let mut c = b.client();
    c.execute(
        "UPDATE mneme_store SET tail=$1",
        &[&(SAFE_INTEGER_MAX as i64)],
    )
    .unwrap();
    assert_eq!(
        value(&append(&s, "s", "a", &json!([draft("r1")])).unwrap()),
        value(&first)
    );
    error(
        append(&s, "s", "a", &json!([draft("r2")])).unwrap_err(),
        ErrorCode::PersistenceOperationConflict,
        Some("operation"),
    );
    error(
        append(&s, "s", "b", &json!([draft("r1")])).unwrap_err(),
        ErrorCode::HistoryDuplicateRecordId,
        Some("records"),
    );
    error(
        append(&s, "s", "b", &json!([draft("r2")])).unwrap_err(),
        ErrorCode::PersistenceStoreFull,
        None,
    );
    c.execute(
        "UPDATE mneme_store SET tail=$1",
        &[&((SAFE_INTEGER_MAX - 1) as i64)],
    )
    .unwrap();
    error(
        append(&s, "s", "b", &json!([draft("r2"), draft("r3")])).unwrap_err(),
        ErrorCode::PersistenceStoreFull,
        None,
    );
    let last = append(&s, "s", "b", &json!([draft("r2")])).unwrap();
    assert_eq!(
        value(&last)["last_position"],
        json!(SAFE_INTEGER_MAX as i64)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn invalid_clock_and_killed_transaction_persist_nothing() {
    let b = pg::Backend::new();
    let s = Store::open(&b.conninfo, |_| Value::integer(1)).unwrap();
    s.create_store(Input::Text("\"s\"")).unwrap();
    assert!(matches!(
        append(&s, "s", "a", &json!([draft("r")])),
        Err(StoreError::Clock(_))
    ));
    assert_eq!(
        value(&s.store_tail(Input::Text("\"s\"")).unwrap())["position"],
        0
    );
    drop(s);
    let kill_info = b.conninfo.clone();
    let s=Store::open(&format!("{} application_name=mneme_rust_killed",b.conninfo),move |_| {
        let mut c=Client::connect(&kill_info,NoTls).unwrap();
        let pid:i32=c.query_one("SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name='mneme_rust_killed' AND pid OPERATOR(pg_catalog.<>) pg_catalog.pg_backend_pid()",&[]).unwrap().get(0);
        let killed:bool=c.query_one("SELECT pg_catalog.pg_terminate_backend($1)",&[&pid]).unwrap().get(0);assert!(killed);Value::string(T)
    }).unwrap();
    assert!(matches!(
        append(&s, "s", "a", &json!([draft("r")])),
        Err(StoreError::Database(_))
    ));
    drop(s);
    let s = Store::open(&b.conninfo, |_| Value::string(T)).unwrap();
    let r = append(&s, "s", "a", &json!([draft("r")])).unwrap();
    assert_eq!(value(&r)["first_position"], 1);
}

#[test]
fn parallel_writers_and_identical_or_conflicting_retries() {
    let b = pg::Backend::new();
    let s = Store::open(&b.conninfo, |_| Value::string(T)).unwrap();
    s.create_store(Input::Text("\"s\"")).unwrap();
    drop(s);
    let gate = Arc::new(Barrier::new(6));
    let mut workers = vec![];
    for worker in 0..6 {
        let info = b.conninfo.clone();
        let gate = Arc::clone(&gate);
        workers.push(std::thread::spawn(move || {
            let s = Store::open(&info, |_| Value::string(T)).unwrap();
            gate.wait();
            (0..10)
                .map(|i| {
                    value(
                        &append(
                            &s,
                            "s",
                            &format!("{worker}-{i}"),
                            &json!([
                                draft(&format!("r{worker}-{i}a")),
                                draft(&format!("r{worker}-{i}b"))
                            ]),
                        )
                        .unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        }));
    }
    let mut ranges = workers
        .into_iter()
        .flat_map(|w| w.join().unwrap())
        .map(|v| {
            (
                v["first_position"].as_i64().unwrap(),
                v["last_position"].as_i64().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    ranges.sort();
    assert_eq!(
        ranges.iter().flat_map(|(a, b)| *a..=*b).collect::<Vec<_>>(),
        (1..=120).collect::<Vec<_>>()
    );
    let s = Arc::new(Store::open(&b.conninfo, |_| Value::string(T)).unwrap());
    let gate = Arc::new(Barrier::new(8));
    let mut workers = vec![];
    for _ in 0..8 {
        let s = Arc::clone(&s);
        let gate = Arc::clone(&gate);
        workers.push(std::thread::spawn(move || {
            gate.wait();
            value(&append(&s, "s", "shared", &json!([draft("same")])).unwrap())
        }));
    }
    let all = workers
        .into_iter()
        .map(|w| w.join().unwrap())
        .collect::<Vec<_>>();
    assert!(all.iter().all(|v| v == &all[0]));
    assert_eq!(all[0]["first_position"], 121);
    let gate = Arc::new(Barrier::new(5));
    let mut workers = vec![];
    for i in 0..5 {
        let info = b.conninfo.clone();
        let gate = Arc::clone(&gate);
        workers.push(std::thread::spawn(move || {
            let s = Store::open(&info, |_| Value::string(T)).unwrap();
            gate.wait();
            append(
                &s,
                "s",
                "conflict",
                &json!([draft(&format!("conflict{i}"))]),
            )
        }));
    }
    let all = workers
        .into_iter()
        .map(|w| w.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(all.iter().filter(|r| r.is_ok()).count(), 1);
    for e in all.into_iter().filter_map(Result::err) {
        error(
            e,
            ErrorCode::PersistenceOperationConflict,
            Some("operation"),
        );
    }
}

#[test]
fn read_snapshot_during_commits_and_record_counted_pages() {
    let b = pg::Backend::new();
    let s = Store::open(&b.conninfo, |_| Value::string(T)).unwrap();
    s.create_store(Input::Text("\"s\"")).unwrap();
    let info = b.conninfo.clone();
    let done = Arc::new(AtomicUsize::new(0));
    let flag = Arc::clone(&done);
    let writer = std::thread::spawn(move || {
        let s = Store::open(&info, |_| Value::string(T)).unwrap();
        for i in 0..35 {
            append(
                &s,
                "s",
                &format!("o{i}"),
                &json!([draft(&format!("r{i}a")), draft(&format!("r{i}b"))]),
            )
            .unwrap();
        }
        flag.store(1, Ordering::SeqCst);
    });
    let since = native(&json!({"store_id":"s","position":0}));
    let limit = Value::integer(200);
    while done.load(Ordering::SeqCst) == 0 {
        let p = value(
            &s.read_history(
                Input::Text("\"s\""),
                Input::Native(&since),
                Input::Native(&limit),
            )
            .unwrap(),
        );
        let tail = p["tail"]["position"].as_u64().unwrap();
        assert_eq!(p["entries"].as_array().unwrap().len() as u64, tail);
        assert_eq!(tail % 2, 0);
        assert_eq!(p["through"], p["tail"]);
    }
    writer.join().unwrap();
    let p = value(
        &s.read_history(
            Input::Text("\"s\""),
            Input::Native(&since),
            Input::Text("1"),
        )
        .unwrap(),
    );
    assert_eq!(p["through"]["position"], 1);
    assert_eq!(p["tail"]["position"], 70);
}

#[test]
fn waiting_first_open_uses_read_committed_not_the_session_default() {
    let b = pg::Backend::new();
    let mut c = b.client();
    let mut tx = c.transaction().unwrap();
    tx.query(
        "SELECT pg_catalog.pg_advisory_xact_lock($1)",
        &[&SCHEMA_OPEN_LOCK_KEY],
    )
    .unwrap();
    let info = b.with_options(
        "-c default_transaction_isolation=repeatable\\ read -c application_name=mneme_waiter",
    );
    let opener = std::thread::spawn(move || Store::open(&info, |_| Value::string(T)));
    let mut observer = b.client();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let waiting:bool=observer.query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE application_name='mneme_waiter' AND wait_event='advisory')",&[]).unwrap().get(0);
        if waiting {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    tx.batch_execute(NORMATIVE).unwrap();
    tx.commit().unwrap();
    let s = opener.join().unwrap().unwrap();
    protocol(&s, false);
}

#[test]
fn classified_public_catalog_witnesses() {
    let vectors: Json =
        serde_json::from_str(include_str!("vectors/catalog-witnesses.json")).unwrap();
    let mut observed = 0;
    for case in vectors["cases"].as_array().unwrap() {
        let b = pg::Backend::new();
        let mut c = b.client();
        c.batch_execute(NORMATIVE).unwrap();
        if case["old"] == true {
            c.execute("INSERT INTO mneme_store(store_id) VALUES('old')", &[])
                .unwrap();
        }
        let mut extras = vec![];
        for sql in case["setup"].as_array().unwrap() {
            let mut statement = sql.as_str().unwrap().replace("ce04", &b.namespace);
            for original in ["outside_ns", "outside_ref", "elsewhere"] {
                let replacement = format!("{}_{}", b.namespace, original);
                statement = statement.replace(original, &replacement);
                if statement.starts_with(&format!("CREATE SCHEMA {replacement}")) {
                    extras.push(replacement);
                }
            }
            // Publication/extension namespaces are per-case; never touch another fixture's object.
            statement = statement.replace("outside_pub", &format!("{}_pub", b.namespace));
            let result = c.batch_execute(&statement);
            // These original stimuli deliberately leave a useful residual catalog after an
            // engine refuses constraint attachment; it is that real residual that is refused.
            if let Err(e) = result {
                assert!(
                    ["I08", "I09", "I10"].contains(&case["id"].as_str().unwrap()),
                    "{} setup failed: {e}",
                    case["id"]
                );
            }
        }
        let oid = namespace(&mut c);
        let before = catalog::fetch(&mut c, oid).unwrap();
        let mut info = b.conninfo.clone();
        if let Some(options) = case["options"].as_str().filter(|s| !s.is_empty()) {
            info = b.with_options(options);
        }
        let opened = Store::open(&info, |_| Value::string(T));
        match case["expected"].as_str().unwrap() {
            "reject" => error(
                opened
                    .err()
                    .unwrap_or_else(|| panic!("{} accepted", case["id"])),
                ErrorCode::PersistenceSchemaIncompatible,
                None,
            ),
            "accept" => protocol(
                &opened.unwrap_or_else(|e| panic!("{} refused: {e}", case["id"])),
                case["large"] == true,
            ),
            "old" => {
                let s = opened.unwrap();
                assert_eq!(
                    value(&s.store_tail(Input::Text("\"old\"")).unwrap())["position"],
                    1
                );
                let r = append(&s, "old", "a", &json!([draft("r1"), draft("r2")])).unwrap();
                assert_eq!(
                    (
                        value(&r)["first_position"].clone(),
                        value(&r)["last_position"].clone()
                    ),
                    (json!(2), json!(3))
                );
                assert_eq!(
                    value(&append(&s, "old", "a", &json!([draft("r1"), draft("r2")])).unwrap()),
                    value(&r)
                );
                protocol(&s, false);
            }
            _ => panic!("disposition"),
        }
        assert_eq!(
            catalog::fetch(&mut c, oid).unwrap(),
            before,
            "{} changed catalog",
            case["id"]
        );
        for namespace in extras {
            c.batch_execute(&format!("DROP SCHEMA {namespace} CASCADE"))
                .unwrap();
        }
        if case["id"] == "E025" {
            c.batch_execute(&format!("DROP PUBLICATION {}_pub", b.namespace))
                .unwrap();
        }
        observed += 1;
        println!("[P] {} {}", case["id"], case["expected"]);
    }
    assert_eq!(observed, 174);
}

// [F] The proxy projects catalog columns, then executes the production fetch statements.
// It never substitutes a normalized result or a predicate answer.
struct Projected<'a> {
    client: &'a mut Client,
    table: &'static str,
    field: &'static str,
    expression: &'static str,
    condition: &'static str,
}
impl catalog::CatalogConnection for Projected<'_> {
    fn catalog_rows(
        &mut self,
        query: &str,
        namespace: u32,
    ) -> Result<Vec<String>, postgres::Error> {
        let columns=self.client.query("SELECT attname::pg_catalog.text FROM pg_catalog.pg_attribute WHERE attrelid OPERATOR(pg_catalog.=) $1::pg_catalog.text::pg_catalog.regclass AND attnum OPERATOR(pg_catalog.>) 0 AND NOT attisdropped ORDER BY attnum",&[&format!("pg_catalog.{}",self.table)])?;
        let fields = columns
            .iter()
            .map(|r| {
                let name: String = r.get(0);
                if name == self.field {
                    format!(
                        "CASE WHEN {} THEN {} ELSE {} END AS {}",
                        self.condition, self.expression, name, name
                    )
                } else {
                    name
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        let projected = format!("(SELECT {fields} FROM pg_catalog.{})", self.table);
        // A trailing space selects relation references only, never regclass literals.
        let sql = query.replace(
            &format!("pg_catalog.{} ", self.table),
            &format!("{projected} "),
        );
        Ok(self
            .client
            .query(&sql, &[&namespace])?
            .iter()
            .map(|r| r.get(0))
            .collect())
    }
}

#[test]
fn fetch_binding_projects_real_catalog_fields() {
    let b = pg::Backend::new();
    let mut c = b.client();
    c.batch_execute(NORMATIVE).unwrap();
    let oid = namespace(&mut c);
    let cases = [
        (
            "pg_attribute",
            "attcacheoff",
            "7::pg_catalog.int4",
            "attrelid OPERATOR(pg_catalog.=) 'mneme_store'::pg_catalog.regclass AND attnum OPERATOR(pg_catalog.=) 1",
            "columns",
        ),
        (
            "pg_attribute",
            "attlen",
            "99::pg_catalog.int2",
            "attrelid OPERATOR(pg_catalog.=) 'mneme_store'::pg_catalog.regclass AND attnum OPERATOR(pg_catalog.=) 1",
            "columns",
        ),
        (
            "pg_attribute",
            "attbyval",
            "NOT attbyval",
            "attrelid OPERATOR(pg_catalog.=) 'mneme_store'::pg_catalog.regclass AND attnum OPERATOR(pg_catalog.=) 1",
            "columns",
        ),
        (
            "pg_attribute",
            "attalign",
            "'d'::pg_catalog.\"char\"",
            "attrelid OPERATOR(pg_catalog.=) 'mneme_store'::pg_catalog.regclass AND attnum OPERATOR(pg_catalog.=) 1",
            "columns",
        ),
        (
            "pg_attribute",
            "atthasmissing",
            "true",
            "attrelid OPERATOR(pg_catalog.=) 'mneme_store'::pg_catalog.regclass AND attnum OPERATOR(pg_catalog.=) 1",
            "columns",
        ),
        (
            "pg_attribute",
            "attfdwoptions",
            "ARRAY['x=y']::pg_catalog.text[]",
            "attrelid OPERATOR(pg_catalog.=) 'mneme_store'::pg_catalog.regclass AND attnum OPERATOR(pg_catalog.=) 1",
            "columns",
        ),
        (
            "pg_class",
            "relnatts",
            "99::pg_catalog.int2",
            "relname OPERATOR(pg_catalog.=) 'mneme_store'",
            "relations",
        ),
        (
            "pg_class",
            "relchecks",
            "99::pg_catalog.int2",
            "relname OPERATOR(pg_catalog.=) 'mneme_store'",
            "relations",
        ),
        (
            "pg_class",
            "relrewrite",
            "1::pg_catalog.oid",
            "relname OPERATOR(pg_catalog.=) 'mneme_store'",
            "relations",
        ),
        (
            "pg_class",
            "relispopulated",
            "false",
            "relname OPERATOR(pg_catalog.=) 'mneme_store'",
            "relations",
        ),
        (
            "pg_trigger",
            "tgisinternal",
            "false",
            "tgisinternal",
            "triggers",
        ),
        (
            "pg_index",
            "indisready",
            "false",
            "indrelid OPERATOR(pg_catalog.=) 'mneme_store'::pg_catalog.regclass",
            "indexes",
        ),
        (
            "pg_class",
            "reloptions",
            "ARRAY['autovacuum_enabled=false']::pg_catalog.text[]",
            "relkind OPERATOR(pg_catalog.=) 't' AND oid OPERATOR(pg_catalog.=) (SELECT reltoastrelid FROM pg_catalog.pg_class WHERE oid OPERATOR(pg_catalog.=) 'mneme_store'::pg_catalog.regclass)",
            "relations", // the root row also observes this TOAST reloptions property
        ),
        (
            "pg_index",
            "indoption",
            "'1 0'::pg_catalog.int2vector",
            "indrelid OPERATOR(pg_catalog.=) (SELECT reltoastrelid FROM pg_catalog.pg_class WHERE oid OPERATOR(pg_catalog.=) 'mneme_store'::pg_catalog.regclass)",
            "members",
        ),
        (
            "pg_type",
            "typinput",
            "'pg_catalog.int4in'::pg_catalog.regproc",
            "typrelid OPERATOR(pg_catalog.=) 'mneme_store'::pg_catalog.regclass",
            "members",
        ),
    ];
    for (table, field, expression, condition, part) in cases {
        let mut proxy = Projected {
            client: &mut c,
            table,
            field,
            expression,
            condition,
        };
        let observed = catalog::fetch(&mut proxy, oid).unwrap();
        assert_eq!(
            observed.mismatch().unwrap().as_deref(),
            Some(part),
            "[F] {table}.{field}"
        );
    }
    assert_eq!(
        &catalog::fetch(&mut c, oid).unwrap(),
        catalog::pins(),
        "projection left physical catalog untouched"
    );
}

#[test]
fn predicate_checks_every_pin_cell_and_preserves_multiplicity() {
    let expected = catalog::pins().rows();
    let mut count = 0;
    for (part, rows) in &expected {
        for (i, text) in rows.iter().enumerate() {
            let row: Json = serde_json::from_str(text).unwrap();
            for col in 0..row.as_array().unwrap().len() {
                let mut changed = row.clone();
                changed[col] = json!({"deliberately_outside_v1":true});
                let mut input = expected.clone();
                input.get_mut(part).unwrap()[i] = changed.to_string();
                assert_eq!(
                    catalog::Catalog::from_rows(input)
                        .unwrap()
                        .mismatch()
                        .unwrap()
                        .as_deref(),
                    Some(part.as_str())
                );
                count += 1;
            }
        }
        let mut input = expected.clone();
        input
            .get_mut(part)
            .unwrap()
            .push(json!(["unexpected-row"]).to_string());
        assert_eq!(
            catalog::Catalog::from_rows(input)
                .unwrap()
                .mismatch()
                .unwrap()
                .as_deref(),
            Some(part.as_str())
        );
        if !rows.is_empty() {
            let mut input = expected.clone();
            input.get_mut(part).unwrap().pop();
            assert!(
                catalog::Catalog::from_rows(input)
                    .unwrap()
                    .mismatch()
                    .unwrap()
                    .is_some()
            );
        }
    }
    assert!(count > 800);
    println!("[Pr] {count} scalar/structured cells plus row multiplicity");
    let mut input = expected.clone();
    input.insert("extra_part".into(), vec![]);
    assert_eq!(
        catalog::Catalog::from_rows(input)
            .unwrap()
            .mismatch()
            .unwrap()
            .as_deref(),
        Some("inventory")
    );
    // Arbitrary strings would not prove individual whole-row properties. Re-render each
    // changed document with PostgreSQL, first checking the no-op rendering against the pin.
    let b = pg::Backend::new();
    let mut c = b.client();
    let mut fields = 0;
    for part in ["members", "index_columns"] {
        for (i, text) in expected[part].iter().enumerate() {
            let row: Json = serde_json::from_str(text).unwrap();
            let slot = if part == "members" { 2 } else { 4 };
            let document = row[slot].as_str().unwrap();
            let same: String = c
                .query_one(
                    "SELECT $1::pg_catalog.text::pg_catalog.jsonb::pg_catalog.text",
                    &[&document],
                )
                .unwrap()
                .get(0);
            assert_eq!(same, document, "fixture renderer must have no drift");
            let keys: Json = serde_json::from_str(document).unwrap();
            for key in keys.as_object().unwrap().keys() {
                let mutated:String=c.query_one("SELECT pg_catalog.jsonb_set($1::pg_catalog.text::pg_catalog.jsonb,ARRAY[$2]::pg_catalog.text[],'\"not-v1\"'::pg_catalog.jsonb)::pg_catalog.text",&[&document,key]).unwrap().get(0);
                let mut changed = row.clone();
                changed[slot] = json!(mutated);
                let mut input = expected.clone();
                input.get_mut(part).unwrap()[i] = changed.to_string();
                assert_eq!(
                    catalog::Catalog::from_rows(input)
                        .unwrap()
                        .mismatch()
                        .unwrap()
                        .as_deref(),
                    Some(part)
                );
                fields += 1;
            }
        }
    }
    assert!(fields > 700);
    println!("[Pr] {fields} individually mutated whole-row properties");
}

#[test]
fn inventory_matches_the_live_postgres14_catalog() {
    let b = pg::Backend::new();
    let mut c = b.client();
    let pin: Json = serde_json::from_str(catalog::SHARED_PINS).unwrap();
    // The accepted shared row documents enumerate every non-exempt catalog field.
    let mut observed = BTreeMap::<&str, std::collections::BTreeSet<String>>::new();
    for text in &catalog::pins().rows()["members"] {
        let row: Json = serde_json::from_str(text).unwrap();
        let document: Json = serde_json::from_str(row[2].as_str().unwrap()).unwrap();
        let catalogs: Vec<&str> = match row[1].as_str().unwrap() {
            "toast" => vec!["pg_class"],
            "toast index" => vec!["pg_class", "pg_index"],
            "toast column" | "toast index column" => vec!["pg_attribute"],
            "row" | "array" => vec!["pg_type"],
            _ => panic!("member kind"),
        };
        for table in catalogs {
            let live:Vec<String>=c.query("SELECT attname::pg_catalog.text FROM pg_catalog.pg_attribute WHERE attrelid OPERATOR(pg_catalog.=) $1::pg_catalog.text::pg_catalog.regclass AND attnum OPERATOR(pg_catalog.>) 0 AND NOT attisdropped",&[&format!("pg_catalog.{table}")]).unwrap().iter().map(|r|r.get(0)).collect();
            let excludes = catalog::exclusions(table);
            let listed = document.as_object().unwrap();
            for key in &live {
                assert_eq!(
                    listed.contains_key(key),
                    !excludes.contains(&key.as_str()),
                    "[INV] {table}.{key}"
                );
            }
            assert!(
                excludes.iter().all(|key| live.iter().any(|v| v == key)),
                "exclusions name actual PG14 columns"
            );
            observed.entry(table).or_default().extend(live);
        }
    }
    assert_eq!(observed.len(), 4);
    assert!(pin["catalog"]["members"].is_array());
    for (table, fields) in observed {
        println!("[INV] {table} {} fields inventoried", fields.len());
    }
}

#[test]
fn genuine_sql_under_hostile_search_path_and_temporary_catalog_names() {
    let b = pg::Backend::new();
    let mut c = b.client();
    c.batch_execute(NORMATIVE).unwrap();
    let oid = namespace(&mut c);
    c.batch_execute(r#"
      CREATE FUNCTION spoof_eq(pg_catalog.text,pg_catalog.text) RETURNS pg_catalog.bool LANGUAGE plpgsql AS $$BEGIN RAISE EXCEPTION 'user text equality called'; END$$;
      CREATE OPERATOR = (FUNCTION=spoof_eq,LEFTARG=pg_catalog.text,RIGHTARG=pg_catalog.text);
    "#).unwrap();
    // Poison same-named ordinary catalog objects without altering the Mneme island.
    c.batch_execute("CREATE TABLE pg_class(bogus integer); CREATE DOMAIN text AS pg_catalog.int4; CREATE DOMAIN bigint AS pg_catalog.text; CREATE FUNCTION current_schema() RETURNS pg_catalog.name LANGUAGE sql AS $$SELECT 'bogus'::pg_catalog.name$$;").unwrap();
    let info = b.with_options(&format!("-c search_path={},pg_catalog", b.namespace));
    let s = Store::open(&info, |_| Value::string(T)).unwrap();
    protocol(&s, false);
    // [E] Poisoned existing sessions are fetch controls, not public-open executions.
    c.batch_execute("CREATE TEMP TABLE pg_class(bogus integer); CREATE TEMP TABLE pg_attribute(bogus integer); CREATE TEMP TABLE pg_type(bogus integer); CREATE TEMP TABLE pg_depend(bogus integer); SET search_path=pg_temp;").unwrap();
    assert_eq!(&catalog::fetch(&mut c, oid).unwrap(), catalog::pins());
    c.batch_execute("RESET search_path").unwrap();
    // Every genuine declaration uses catalog-qualified types and this concrete namespace.
    for statement in minion_memory::persistence::ddl(&b.namespace) {
        assert!(statement.contains("pg_catalog."));
        assert!(statement.contains(&format!("\"{}\"", b.namespace)));
    }
}

#[test]
fn open_lock_is_the_shared_granted_bigint_lock_and_init_postcondition_is_pinned() {
    let b = pg::Backend::new();
    let mut c = b.client();
    let mut tx = c.transaction().unwrap();
    tx.query(
        "SELECT pg_catalog.pg_advisory_xact_lock($1::pg_catalog.int8)",
        &[&SCHEMA_OPEN_LOCK_KEY],
    )
    .unwrap();
    let mut observer = b.client();
    let granted:bool=observer.query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE locktype='advisory' AND granted AND objsubid=1 AND classid=3201722174::pg_catalog.oid AND objid=1984885122::pg_catalog.oid)",&[]).unwrap().get(0);
    assert!(
        granted,
        "[T] literal shared single-bigint key is actually granted"
    );
    tx.rollback().unwrap();
    let info = b.with_options("-c default_tablespace=mneme_ts");
    let s = Store::open(&info, |_| Value::string(T)).unwrap();
    let oid = namespace(&mut c);
    assert_eq!(&catalog::fetch(&mut c, oid).unwrap(), catalog::pins());
    protocol(&s, false);
}

struct PythonBridge {
    child: std::process::Child,
    input: std::process::ChildStdin,
    output: std::io::BufReader<std::process::ChildStdout>,
}
impl PythonBridge {
    fn start(info: &str, clock: &str) -> Self {
        use std::process::{Command, Stdio};
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let python = std::env::var_os("MNEME_INTEROP_PYTHON")
            .expect("managed project Python required for [IT], never silently skip");
        let mut python_paths = vec![root.join("../minion-memory-python/src")];
        if let Some(existing) = std::env::var_os("PYTHONPATH") {
            python_paths.extend(std::env::split_paths(&existing));
        }
        let mut child = Command::new(python)
            .arg("-X")
            .arg("utf8")
            .arg(root.join("tests/interop_python.py"))
            .env("PYTHONPATH", std::env::join_paths(python_paths).unwrap())
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("MNEME_INTEROP_DSN", info)
            .env("MNEME_INTEROP_CLOCK", clock)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = std::io::BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            input,
            output,
        }
    }
    fn read(&mut self) -> Json {
        use std::io::BufRead;
        let mut line = String::new();
        assert!(
            self.output.read_line(&mut line).unwrap() > 0,
            "Python bridge terminated before response"
        );
        serde_json::from_str(&line).unwrap()
    }
    fn ready(&mut self) {
        assert_eq!(self.read(), json!({"ready":true}));
    }
    fn call(&mut self, request: Json) -> Json {
        use std::io::Write;
        writeln!(self.input, "{request}").unwrap();
        self.input.flush().unwrap();
        let response = self.read();
        assert!(
            response.get("error").is_none(),
            "interop contract rejection: {response}"
        );
        response["value"].clone()
    }
}
impl Drop for PythonBridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn both_languages_read_retry_nul_identity_and_normalized_mutation() {
    for python_first in [false, true] {
        let b = pg::Backend::new();
        let mut py;
        let rust;
        if python_first {
            py = PythonBridge::start(&b.conninfo, T);
            py.ready();
            rust = Store::open(&b.conninfo, |_| Value::string(T)).unwrap();
        } else {
            rust = Store::open(&b.conninfo, |_| Value::string(T)).unwrap();
            py = PythonBridge::start(&b.conninfo, T);
            py.ready();
        }
        for python_writer in [true, false] {
            let sid = if python_writer { "python" } else { "rust" };
            if python_writer {
                assert_eq!(
                    py.call(json!({"operation_name":"create_store","store_id":sid})),
                    Json::Null
                );
            } else {
                rust.create_store(Input::Native(&Value::string(sid)))
                    .unwrap();
            }
            let mut r = draft("r1");
            r["occurred_at"] = json!("2026-10-11T10:00:00+02:00");
            r["payload"] =
                json!({"n":2.0,"large":"a".repeat(24000),"unicode":"💡","nul":"a\u{0}b"});
            let rows = json!([r]);
            let op = operation("a\u{0}b");
            let receipt = if python_writer {
                py.call(json!({"operation_name":"commit_history","store_id":sid,"writer":writer(),"operation":op,"records":rows}))
            } else {
                value(
                    &rust
                        .commit_history(
                            Input::Native(&Value::string(sid)),
                            Input::Native(&native(&writer())),
                            Input::Native(&native(&op)),
                            Input::Native(&native(&rows)),
                        )
                        .unwrap(),
                )
            };
            assert_eq!(receipt["first_position"], 1);
            assert_eq!(receipt["last_position"], 1);
            assert_eq!(receipt["recorded_at"], T);
            assert_eq!(receipt["records"][0]["occurred_at"], T);
            assert_eq!(receipt["records"][0]["payload"]["n"], 2);
            let mut normalized = rows.clone();
            normalized[0]["occurred_at"] = json!(T);
            normalized[0]["payload"]["n"] = json!(2);
            let mut check = b.client();
            let stored: Vec<u8> = check
                .query_one(
                    "SELECT mutation FROM mneme_operation WHERE store_id=$1",
                    &[&sid],
                )
                .unwrap()
                .get(0);
            let expected = canonical_bytes(&native(&normalized)).unwrap();
            if stored != expected {
                let at = stored
                    .iter()
                    .zip(&expected)
                    .position(|(a, b)| a != b)
                    .unwrap_or(stored.len().min(expected.len()));
                panic!(
                    "[IT] writer Python={python_writer} mutation byte {at}, lengths {} / {}, actual {:?}, expected {:?}",
                    stored.len(),
                    expected.len(),
                    &stored[at..stored.len().min(at + 40)],
                    &expected[at..expected.len().min(at + 40)]
                );
            }
            println!(
                "[IT] initializer Python={python_first}, writer Python={python_writer} mutation exact"
            );
            let retried = if python_writer {
                value(
                    &rust
                        .commit_history(
                            Input::Text(&serde_json::to_string(sid).unwrap()),
                            Input::Text(&writer().to_string()),
                            Input::Text(&op.to_string()),
                            Input::Text(&normalized.to_string()),
                        )
                        .unwrap(),
                )
            } else {
                py.call(json!({"operation_name":"commit_history","store_id_json":serde_json::to_string(sid).unwrap(),"writer_json":writer().to_string(),"operation_json":op.to_string(),"records_json":normalized.to_string()}))
            };
            assert_eq!(
                retried, receipt,
                "[IT] normalized retry returns original stamped receipt"
            );
            let since = json!({"store_id":sid,"position":0});
            let page_rust = value(
                &rust
                    .read_history(
                        Input::Native(&Value::string(sid)),
                        Input::Native(&native(&since)),
                        Input::Text("10"),
                    )
                    .unwrap(),
            );
            let page_python = py.call(
                json!({"operation_name":"read_history","store_id":sid,"since":since,"limit":10}),
            );
            assert_eq!(page_rust, page_python);
            assert_eq!(page_rust["entries"][0]["record"], receipt["records"][0]);
            // Persisted bytes are compared to the shared canonical contract, not a host serializer.
            let mut c = b.client();
            let bytes: Vec<u8> = c
                .query_one(
                    "SELECT envelope FROM mneme_record WHERE store_id=$1",
                    &[&sid],
                )
                .unwrap()
                .get(0);
            assert_eq!(
                bytes,
                canonical_bytes(&native(&receipt["records"][0])).unwrap()
            );
            let key: Vec<u8> = c
                .query_one(
                    "SELECT source_operation_id FROM mneme_operation WHERE store_id=$1",
                    &[&sid],
                )
                .unwrap()
                .get(0);
            assert_eq!(key, b"a\0b");
        }
    }
}

#[test]
fn mixed_language_waiting_first_open_under_repeatable_read_session_defaults() {
    let b = pg::Backend::new();
    let mut c = b.client();
    let mut tx = c.transaction().unwrap();
    tx.query(
        "SELECT pg_catalog.pg_advisory_xact_lock($1::pg_catalog.int8)",
        &[&SCHEMA_OPEN_LOCK_KEY],
    )
    .unwrap();
    let rust_info = b.with_options(
        "-c default_transaction_isolation=repeatable\\ read -c application_name=mneme_mixed_rust",
    );
    let py_info = b.with_options(
        "-c default_transaction_isolation=repeatable\\ read -c application_name=mneme_mixed_python",
    );
    let worker = std::thread::spawn(move || Store::open(&rust_info, |_| Value::string(T)));
    let mut py = PythonBridge::start(&py_info, T);
    let mut observer = b.client();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let count:i64=observer.query_one("SELECT pg_catalog.count(*) FROM pg_catalog.pg_stat_activity WHERE application_name IN ('mneme_mixed_rust','mneme_mixed_python') AND wait_event='advisory'",&[]).unwrap().get(0);
        if count == 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "both language openers must really wait"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    tx.commit().unwrap();
    let rust = worker.join().unwrap().unwrap();
    py.ready();
    rust.create_store(Input::Text("\"mixed\"")).unwrap();
    assert_eq!(
        py.call(json!({"operation_name":"store_tail","store_id":"mixed"})),
        json!({"store_id":"mixed","position":0})
    );
    let oid = namespace(&mut observer);
    assert_eq!(
        &catalog::fetch(&mut observer, oid).unwrap(),
        catalog::pins()
    );
    // A real mixed-language conflict uses byte-identical operation identity and drafts.
    let gate = Arc::new(Barrier::new(2));
    let start = Arc::clone(&gate);
    let writer_thread = std::thread::spawn(move || {
        start.wait();
        value(&append(&rust, "mixed", "race", &json!([draft("r")])).unwrap())
    });
    gate.wait();
    let receipt=py.call(json!({"operation_name":"commit_history","store_id":"mixed","writer":writer(),"operation":operation("race"),"records":[draft("r")]}));
    assert_eq!(writer_thread.join().unwrap(), receipt);
    assert_eq!(receipt["first_position"], 1);
    assert_eq!(receipt["last_position"], 1);
    let count: i64 = observer
        .query_one("SELECT pg_catalog.count(*) FROM mneme_record", &[])
        .unwrap()
        .get(0);
    assert_eq!(count, 1);
}

#[test]
fn fetch_binding_nonconstructible_attachment_rows() {
    let b = pg::Backend::new();
    let mut c = b.client();
    c.batch_execute(NORMATIVE).unwrap();
    let oid = namespace(&mut c);
    c.batch_execute("CREATE TABLE donor_parent(x integer); CREATE TABLE donor_child() INHERITS(donor_parent); CREATE POLICY donor_policy ON donor_parent USING (true); CREATE RULE donor_rule AS ON INSERT TO donor_parent DO INSTEAD NOTHING;").unwrap();
    for (table, field, expression, condition, part) in [
        (
            "pg_inherits",
            "inhparent",
            "'mneme_store'::pg_catalog.regclass::pg_catalog.oid",
            "inhparent OPERATOR(pg_catalog.=) 'donor_parent'::pg_catalog.regclass",
            "inheritance",
        ),
        (
            "pg_policy",
            "polrelid",
            "'mneme_store'::pg_catalog.regclass::pg_catalog.oid",
            "polrelid OPERATOR(pg_catalog.=) 'donor_parent'::pg_catalog.regclass",
            "policies",
        ),
        (
            "pg_rewrite",
            "ev_class",
            "'mneme_store'::pg_catalog.regclass::pg_catalog.oid",
            "ev_class OPERATOR(pg_catalog.=) 'donor_parent'::pg_catalog.regclass",
            "rules",
        ),
        (
            "pg_index",
            "indrelid",
            "'donor_parent'::pg_catalog.regclass::pg_catalog.oid",
            "indrelid OPERATOR(pg_catalog.=) 'mneme_store'::pg_catalog.regclass",
            "indexes",
        ),
    ] {
        let mut proxy = Projected {
            client: &mut c,
            table,
            field,
            expression,
            condition,
        };
        let fetched = catalog::fetch(&mut proxy, oid).unwrap();
        assert_eq!(
            fetched.mismatch().unwrap().as_deref(),
            Some(part),
            "[F] attachment {part}"
        );
    }
    assert_eq!(&catalog::fetch(&mut c, oid).unwrap(), catalog::pins());
}

#[test]
fn security_label_fetch_binding_checks_subobjects_and_internal_members() {
    struct Label<'a> {
        client: &'a mut Client,
        object: u32,
        class: u32,
        subobject: i32,
    }
    impl catalog::CatalogConnection for Label<'_> {
        fn catalog_rows(
            &mut self,
            query: &str,
            namespace: u32,
        ) -> Result<Vec<String>, postgres::Error> {
            let source = format!(
                "(SELECT * FROM pg_catalog.pg_seclabel UNION ALL SELECT {}::pg_catalog.oid,{}::pg_catalog.oid,{}::pg_catalog.int4,'fixture'::pg_catalog.text,'label'::pg_catalog.text)",
                self.object, self.class, self.subobject
            );
            let sql = query.replace("pg_catalog.pg_seclabel l", &format!("{source} l"));
            Ok(self
                .client
                .query(&sql, &[&namespace])?
                .iter()
                .map(|r| r.get(0))
                .collect())
        }
    }
    let b = pg::Backend::new();
    let mut c = b.client();
    c.batch_execute(NORMATIVE).unwrap();
    let oid = namespace(&mut c);
    for (object, class, subobject) in [
        (
            "'mneme_store'::pg_catalog.regclass::pg_catalog.oid",
            "pg_class",
            0,
        ),
        (
            "'mneme_store'::pg_catalog.regclass::pg_catalog.oid",
            "pg_class",
            1,
        ),
        (
            "(SELECT reltoastrelid FROM pg_catalog.pg_class WHERE oid='mneme_store'::pg_catalog.regclass)",
            "pg_class",
            0,
        ),
        (
            "(SELECT reltype FROM pg_catalog.pg_class WHERE oid='mneme_store'::pg_catalog.regclass)",
            "pg_type",
            0,
        ),
    ] {
        let object: u32 = c
            .query_one(&format!("SELECT {object}"), &[])
            .unwrap()
            .get(0);
        let class: u32 = c
            .query_one(
                "SELECT $1::pg_catalog.text::pg_catalog.regclass::pg_catalog.oid",
                &[&format!("pg_catalog.{class}")],
            )
            .unwrap()
            .get(0);
        let mut proxy = Label {
            client: &mut c,
            object,
            class,
            subobject,
        };
        assert_eq!(
            catalog::fetch(&mut proxy, oid)
                .unwrap()
                .mismatch()
                .unwrap()
                .as_deref(),
            Some("boundary")
        );
    }
    assert_eq!(&catalog::fetch(&mut c, oid).unwrap(), catalog::pins());
}

#[test]
fn initialization_refuses_partial_nonindex_relations_and_preserves_engine_errors() {
    for object in [
        "CREATE VIEW mneme_surprise AS SELECT 1 AS x",
        "CREATE SEQUENCE mneme_surprise",
        "CREATE TABLE mneme_surprise(x integer)",
        "CREATE MATERIALIZED VIEW mneme_surprise AS SELECT 1 AS x",
    ] {
        let b = pg::Backend::new();
        let mut c = b.client();
        c.batch_execute(object).unwrap();
        error(
            Store::open(&b.conninfo, |_| Value::string(T))
                .err()
                .expect("partial schema refused"),
            ErrorCode::PersistenceSchemaIncompatible,
            None,
        );
        let created: bool = c
            .query_one(
                "SELECT pg_catalog.to_regclass('mneme_schema') IS NOT NULL",
                &[],
            )
            .unwrap()
            .get(0);
        assert!(!created, "no initializer write on a partial schema");
    }
    // A harmless non-Mneme relation and a Mneme-named index do not meet the init guard.
    let b = pg::Backend::new();
    let mut c = b.client();
    c.batch_execute("CREATE TABLE donor(x integer); CREATE INDEX mneme_other_index ON donor(x);")
        .unwrap();
    protocol(
        &Store::open(&b.conninfo, |_| Value::string(T)).unwrap(),
        false,
    );
    let b = pg::Backend::new();
    let result = Store::open(&b.with_options("-c search_path=missing_namespace"), |_| {
        Value::string(T)
    });
    let StoreError::Database(e) = result.err().expect("no current schema fails") else {
        panic!("genuine DDL error required")
    };
    assert_eq!(e.code().map(|c| c.code()), Some("3F000"));
}

#[test]
fn environment_and_open_lock_derivation_controls() {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(b"mneme.schema.open.v1");
    assert_eq!(
        &hash[..8],
        &[0xbe, 0xd6, 0x67, 0x3e, 0x76, 0x4e, 0xf1, 0x82]
    );
    assert_eq!(
        i64::from_be_bytes(hash[..8].try_into().unwrap()),
        SCHEMA_OPEN_LOCK_KEY
    );
    for collate in ["C", "POSIX"] {
        for ctype in ["C", "POSIX"] {
            assert!(catalog::environment_matches(140012, "UTF8", collate, ctype));
        }
    }
    for (version, encoding, collate, ctype) in [
        (130012, "UTF8", "C", "C"),
        (150001, "UTF8", "C", "C"),
        (140012, "SQL_ASCII", "C", "C"),
        (140012, "UTF8", "en_US.UTF-8", "C"),
        (140012, "UTF8", "C", "en_US.UTF-8"),
    ] {
        assert!(!catalog::environment_matches(
            version, encoding, collate, ctype
        ));
    }
}

#[test]
fn one_handle_serializes_mixed_transaction_lifecycles_while_clock_is_gated() {
    use std::sync::{Condvar, Mutex, mpsc};
    let b = pg::Backend::new();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let clock_gate = Arc::clone(&release);
    let (entered_tx, entered_rx) = mpsc::channel();
    let store = Arc::new(
        Store::open(&b.conninfo, move |_| {
            entered_tx.send(()).unwrap();
            let (lock, condition) = &*clock_gate;
            let mut ready = lock.lock().unwrap();
            while !*ready {
                ready = condition.wait(ready).unwrap();
            }
            Value::string(T)
        })
        .unwrap(),
    );
    store.create_store(Input::Text("\"s\"")).unwrap();
    let committing = Arc::clone(&store);
    let commit =
        std::thread::spawn(move || append(&committing, "s", "gated", &json!([draft("r")])));
    entered_rx.recv_timeout(Duration::from_secs(30)).unwrap();
    let (tail_tx, tail_rx) = mpsc::channel();
    let reading = Arc::clone(&store);
    let tail = std::thread::spawn(move || {
        let result = reading.store_tail(Input::Text("\"s\""));
        tail_tx.send(result.map(|v| value(&v))).unwrap();
    });
    assert!(
        matches!(
            tail_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "second operation must not use a connection inside the first transaction"
    );
    let (lock, condition) = &*release;
    *lock.lock().unwrap() = true;
    condition.notify_all();
    let receipt = commit.join().unwrap().unwrap();
    assert_eq!(value(&receipt)["last_position"], 1);
    assert_eq!(
        tail_rx
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .unwrap(),
        json!({"store_id":"s","position":1})
    );
    tail.join().unwrap();
}

#[test]
fn non_utf8_database_is_refused_and_private_handle_pins_utf8() {
    let b = pg::Backend::new();
    let mut c = b.client();
    let database = std::env::var("MNEME_RUST_PG_DATABASE").unwrap();
    let bad = format!("{database}_sqlascii");
    struct Cleanup {
        client: Client,
        name: String,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = self.client.batch_execute(&format!(
                "DROP DATABASE IF EXISTS \"{}\" WITH (FORCE)",
                self.name
            ));
        }
    }
    let mut cleanup = Cleanup {
        client: b.client(),
        name: bad.clone(),
    };
    cleanup
        .client
        .batch_execute(&format!("DROP DATABASE IF EXISTS \"{bad}\" WITH (FORCE)"))
        .unwrap();
    cleanup.client.batch_execute(&format!("CREATE DATABASE \"{bad}\" TEMPLATE template0 ENCODING 'SQL_ASCII' LC_COLLATE 'C' LC_CTYPE 'C' TABLESPACE mneme_ts")).unwrap();
    let info = b
        .conninfo
        .replace(&format!("dbname='{database}'"), &format!("dbname='{bad}'"));
    assert_ne!(info, b.conninfo);
    error(
        Store::open(&info, |_| Value::string(T))
            .err()
            .expect("SQL_ASCII rejected"),
        ErrorCode::PersistenceSchemaIncompatible,
        None,
    );
    let s = Store::open(&b.with_options("-c client_encoding=LATIN1"), |_| {
        Value::string(T)
    })
    .unwrap();
    s.create_store(Input::Text("\"s\"")).unwrap();
    let mut r = draft("r");
    r["payload"] = json!({"text":"🦀é"});
    append(&s, "s", "unicode", &json!([r])).unwrap();
    let bytes: Vec<u8> = c
        .query_one("SELECT envelope FROM mneme_record", &[])
        .unwrap()
        .get(0);
    assert!(std::str::from_utf8(&bytes).unwrap().contains("🦀é"));
}
