//! Layer 04 canonical history repository, PostgreSQL reference backend.
//!
//! Authority: the accepted persistence contract, G-077..G-098, and shared v1 pins.
//! Semantic rejections retain their Mneme code; database errors retain the driver error.

pub mod catalog;

use crate::history::{boundary_rule, reader_rule, record_rule};
use crate::identity::{id_rule, operation_rule, staged};
use crate::time::timestamp_rule;
use crate::values::Node;
use crate::{Error, ErrorCode, Input, SAFE_INTEGER_MAX, Value, canonical_bytes, decode_json};
use postgres::{Client, IsolationLevel, NoTls};
use std::collections::HashSet;
use std::fmt;
use std::sync::Mutex;

/// Shared single-bigint schema-open lock (SHA-256 of mneme.schema.open.v1).
pub const SCHEMA_OPEN_LOCK_KEY: i64 = -4695452043516644990;
/// Shared PostgreSQL schema version.
pub const SCHEMA_VERSION: i32 = 1;

/// Language binding distinguishes contract rejection, engine failure and defective clock.
#[derive(Debug)]
pub enum StoreError {
    /// A stable contract outcome; store errors never carry an entry position.
    Validation(Error),
    /// A genuine engine/availability error, never reclassified as schema incompatibility.
    Database(postgres::Error),
    /// The injected clock produced a value outside HIST-004.
    Clock(Error),
    /// Caller lifecycle failure after a panic poisoned the handle.
    Poisoned,
}
impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(e) => write!(f, "{e}"),
            Self::Database(e) => write!(f, "{e}"),
            Self::Clock(e) => write!(f, "clock yielded an invalid instant: {e}"),
            Self::Poisoned => write!(f, "history store handle is poisoned"),
        }
    }
}
impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Validation(e) | Self::Clock(e) => Some(e),
            Self::Database(e) => Some(e),
            Self::Poisoned => None,
        }
    }
}
impl From<Error> for StoreError {
    fn from(e: Error) -> Self {
        Self::Validation(e)
    }
}
impl From<postgres::Error> for StoreError {
    fn from(e: postgres::Error) -> Self {
        Self::Database(e)
    }
}

fn failure(code: ErrorCode, argument: Option<&str>) -> StoreError {
    Error::new(code)
        .named(argument.unwrap_or(""), argument.is_some())
        .into()
}
fn incompatible(detail: &str) -> StoreError {
    let mut e = Error::new(ErrorCode::PersistenceSchemaIncompatible);
    e.detail = detail.to_owned();
    e.into()
}
fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
fn record(fields: Vec<(&str, Value)>) -> Value {
    Value::object(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}
fn boundary(sid: &str, position: i64) -> Value {
    record(vec![
        ("store_id", Value::string(sid)),
        ("position", Value::integer(i128::from(position))),
    ])
}
fn receipt(
    sid: &str,
    namespace: &str,
    operation: &str,
    first: i64,
    last: i64,
    time: &str,
    envelopes: Vec<Value>,
) -> Value {
    record(vec![
        ("store_id", Value::string(sid)),
        (
            "operation",
            record(vec![
                ("source_namespace", Value::string(namespace)),
                ("source_operation_id", Value::string(operation)),
            ]),
        ),
        ("first_position", Value::integer(i128::from(first))),
        ("last_position", Value::integer(i128::from(last))),
        ("recorded_at", Value::string(time)),
        ("records", Value::array(envelopes)),
    ])
}
fn child(value: &Value, key: &str) -> Value {
    let Node::Object(members) = value.node() else {
        unreachable!("typed object")
    };
    value.detached(
        members
            .iter()
            .find(|(k, _)| k == key)
            .expect("typed member")
            .1,
    )
}
fn stamped(draft: &Value, time: &str) -> Value {
    let Node::Object(members) = draft.node() else {
        unreachable!("typed draft")
    };
    let mut fields: Vec<_> = members
        .iter()
        .map(|(k, i)| (k.clone(), draft.detached(*i)))
        .collect();
    fields.push(("recorded_at".to_owned(), Value::string(time)));
    Value::object(fields)
}

type Clock = Box<dyn FnMut(&str) -> Value + Send>;
struct State {
    client: Client,
    namespace: String,
    clock: Clock,
}

/// One private connection, serialized across each operation's full transaction lifecycle.
pub struct PostgresHistoryStore {
    state: Mutex<State>,
}

impl PostgresHistoryStore {
    /// Open or initialize v1. Uses a new private session and only the caller's ordinary role.
    /// `clock(store_id)` returns a HIST-004 value; all store-generated fields are excluded
    /// from the canonical mutation used for retry identity.
    pub fn open(
        conninfo: &str,
        clock: impl FnMut(&str) -> Value + Send + 'static,
    ) -> Result<Self, StoreError> {
        let mut client = Client::connect(conninfo, NoTls)?;
        client.batch_execute("SET client_encoding = 'UTF8'; SET synchronous_commit = on")?;
        let namespace;
        {
            let mut tx = client
                .build_transaction()
                .isolation_level(IsolationLevel::ReadCommitted)
                .read_only(false)
                .start()?;
            tx.query(
                "SELECT pg_catalog.pg_advisory_xact_lock($1::pg_catalog.int8)",
                &[&SCHEMA_OPEN_LOCK_KEY],
            )?;
            let env = tx.query_one("SELECT pg_catalog.current_setting('server_version_num'), pg_catalog.pg_encoding_to_char(d.encoding), d.datcollate::pg_catalog.text, d.datctype::pg_catalog.text FROM pg_catalog.pg_database d WHERE d.datname OPERATOR(pg_catalog.=) pg_catalog.current_database()",&[])?;
            let version: String = env.get(0);
            let encoding: String = env.get(1);
            let collate: String = env.get(2);
            let ctype: String = env.get(3);
            if !catalog::environment_matches(
                version.parse().unwrap_or(0),
                &encoding,
                &collate,
                &ctype,
            ) {
                return Err(incompatible(
                    "database environment differs from PostgreSQL v1",
                ));
            }
            let ns=tx.query_opt("SELECT n.oid, n.nspname::pg_catalog.text FROM pg_catalog.pg_namespace n WHERE n.nspname OPERATOR(pg_catalog.=) pg_catalog.current_schema()",&[])?;
            let Some(ns) = ns else {
                // Let PostgreSQL provide its own no-current-schema error, without classification.
                tx.batch_execute("CREATE TABLE mneme_schema (singleton pg_catalog.bool)")?;
                unreachable!("no namespace cannot initialize")
            };
            let oid: u32 = ns.get(0);
            namespace = ns.get::<_, String>(1);
            let mut found = catalog::fetch(&mut tx, oid)?;
            if found.empty_namespace() {
                tx.batch_execute("SET LOCAL default_tablespace = ''; SET LOCAL default_table_access_method = heap")?;
                for ddl in ddl(&namespace) {
                    tx.batch_execute(&ddl)?;
                }
                tx.execute(
                    &format!(
                        "INSERT INTO {}.mneme_schema(version) VALUES ($1)",
                        ident(&namespace)
                    ),
                    &[&SCHEMA_VERSION],
                )?;
                found = catalog::fetch(&mut tx, oid)?;
            }
            if let Some(part) = found.mismatch()? {
                return Err(incompatible(&format!("v1 catalog differs in {part}")));
            }
            let rows = tx.query(
                &format!("SELECT version FROM {}.mneme_schema", ident(&namespace)),
                &[],
            )?;
            if rows.len() != 1 || rows[0].get::<_, i32>(0) != SCHEMA_VERSION {
                return Err(incompatible("expected one schema version 1 row"));
            }
            tx.commit()?;
        }
        Ok(Self {
            state: Mutex::new(State {
                client,
                namespace,
                clock: Box::new(clock),
            }),
        })
    }

    /// Create one empty logical store, durably; an existing ID is store_exists.
    pub fn create_store(&self, input: Input<'_>) -> Result<(), StoreError> {
        let values = staged(&[("store_id", input)]).map_err(|e| e.named("store_id", true))?;
        let sid = id_rule(&values[0]).map_err(|e| e.named("store_id", true))?;
        let mut state = self.state.lock().map_err(|_| StoreError::Poisoned)?;
        let table = format!("{}.mneme_store", ident(&state.namespace));
        let mut tx = state
            .client
            .build_transaction()
            .isolation_level(IsolationLevel::ReadCommitted)
            .read_only(false)
            .start()?;
        let created=tx.query_opt(&format!("INSERT INTO {table}(store_id) VALUES ($1) ON CONFLICT DO NOTHING RETURNING store_id"),&[&sid])?.is_some();
        tx.commit()?;
        if !created {
            return Err(failure(ErrorCode::PersistenceStoreExists, Some("store_id")));
        }
        Ok(())
    }

    /// Return the committed store-local tail, 0 for an empty store.
    pub fn store_tail(&self, input: Input<'_>) -> Result<Value, StoreError> {
        let values = staged(&[("store_id", input)]).map_err(|e| e.named("store_id", true))?;
        let sid = id_rule(&values[0]).map_err(|e| e.named("store_id", true))?;
        let mut state = self.state.lock().map_err(|_| StoreError::Poisoned)?;
        let table = format!("{}.mneme_store", ident(&state.namespace));
        let mut tx = state
            .client
            .build_transaction()
            .isolation_level(IsolationLevel::ReadCommitted)
            .read_only(true)
            .start()?;
        let row = tx
            .query_opt(
                &format!("SELECT tail FROM {table} WHERE store_id OPERATOR(pg_catalog.=) $1"),
                &[&sid],
            )?
            .ok_or_else(|| failure(ErrorCode::PersistenceStoreUnknown, Some("store_id")))?;
        let value = boundary(&sid, row.get(0));
        tx.commit()?;
        Ok(value)
    }

    /// Append an atomic batch, or return an identical operation's original receipt.
    pub fn commit_history(
        &self,
        sid: Input<'_>,
        writer: Input<'_>,
        operation: Input<'_>,
        records: Input<'_>,
    ) -> Result<Value, StoreError> {
        let values = staged(&[
            ("store_id", sid),
            ("writer", writer),
            ("operation", operation),
            ("records", records),
        ])?;
        let sid = id_rule(&values[0]).map_err(|e| e.named("store_id", true))?;
        let reader = reader_rule(&values[1]).map_err(|e| e.named("writer", true))?;
        let identity = operation_rule(&values[2]).map_err(|e| e.named("operation", true))?;
        let Node::Array(children) = values[3].node() else {
            return Err(failure(ErrorCode::PersistenceBatchInvalid, Some("records")));
        };
        if children.is_empty() {
            return Err(failure(ErrorCode::PersistenceBatchInvalid, Some("records")));
        }
        let mut drafts = Vec::with_capacity(children.len());
        for index in children {
            let admission =
                record_rule(&reader, values[3].detached(*index), true).map_err(|e| {
                    if e.code == ErrorCode::HistoryUnknownRequired {
                        Error::new(ErrorCode::PersistenceRecordUnrecognized).named("records", true)
                    } else {
                        e.named("records", true)
                    }
                })?;
            let crate::HistoryAdmission::Decode(draft) = admission else {
                return Err(failure(
                    ErrorCode::PersistenceRecordUnrecognized,
                    Some("records"),
                ));
            };
            drafts.push(draft);
        }
        let ids: Vec<String> = drafts
            .iter()
            .map(|v| {
                child(v, "record_id")
                    .as_str()
                    .expect("validated ID")
                    .to_owned()
            })
            .collect();
        let mut seen = HashSet::new();
        if ids.iter().any(|id| !seen.insert(id)) {
            return Err(failure(
                ErrorCode::HistoryDuplicateRecordId,
                Some("records"),
            ));
        }
        let mutation = canonical_bytes(&Value::array(drafts.clone()))?;
        let key = identity.source_operation_id.as_bytes();
        let mut state = self.state.lock().map_err(|_| StoreError::Poisoned)?;
        let State {
            client,
            namespace,
            clock,
        } = &mut *state;
        let ns = ident(namespace);
        let mut tx = client
            .build_transaction()
            .isolation_level(IsolationLevel::ReadCommitted)
            .read_only(false)
            .start()?;
        let row=tx.query_opt(&format!("SELECT tail FROM {ns}.mneme_store WHERE store_id OPERATOR(pg_catalog.=) $1 FOR UPDATE"),&[&sid])?.ok_or_else(||failure(ErrorCode::PersistenceStoreUnknown,Some("store_id")))?;
        let tail: i64 = row.get(0);
        let prior=tx.query_opt(&format!("SELECT mutation,first_position,last_position,recorded_at FROM {ns}.mneme_operation WHERE store_id OPERATOR(pg_catalog.=) $1 AND source_namespace OPERATOR(pg_catalog.=) $2 AND source_operation_id OPERATOR(pg_catalog.=) $3"),&[&sid,&identity.source_namespace,&key])?;
        if let Some(prior) = prior {
            if prior.get::<_, Vec<u8>>(0) != mutation {
                return Err(failure(
                    ErrorCode::PersistenceOperationConflict,
                    Some("operation"),
                ));
            }
            let first: i64 = prior.get(1);
            let last: i64 = prior.get(2);
            let time: String = prior.get(3);
            let rows=tx.query(&format!("SELECT envelope FROM {ns}.mneme_record WHERE store_id OPERATOR(pg_catalog.=) $1 AND position OPERATOR(pg_catalog.>=) $2 AND position OPERATOR(pg_catalog.<=) $3 ORDER BY position"),&[&sid,&first,&last])?;
            let envelopes = rows
                .iter()
                .map(|r| decode_json(&r.get::<_, Vec<u8>>(0)))
                .collect::<Result<Vec<_>, _>>()?;
            tx.commit()?;
            return Ok(receipt(
                &sid,
                &identity.source_namespace,
                &identity.source_operation_id,
                first,
                last,
                &time,
                envelopes,
            ));
        }
        let duplicates=tx.query_opt(&format!("SELECT record_id FROM {ns}.mneme_record WHERE store_id OPERATOR(pg_catalog.=) $1 AND record_id OPERATOR(pg_catalog.=) ANY($2) ORDER BY position LIMIT 1"),&[&sid,&ids])?;
        if duplicates.is_some() {
            return Err(failure(
                ErrorCode::HistoryDuplicateRecordId,
                Some("records"),
            ));
        }
        let count = i64::try_from(drafts.len()).unwrap_or(i64::MAX);
        if i128::from(count) > SAFE_INTEGER_MAX - i128::from(tail) {
            return Err(failure(ErrorCode::PersistenceStoreFull, None));
        }
        let sample = clock(&sid);
        let time = staged(&[("clock", Input::Native(&sample))])
            .and_then(|v| timestamp_rule(&v[0]))
            .map_err(StoreError::Clock)?;
        let first = tail + 1;
        let last = tail + count;
        let envelopes: Vec<_> = drafts.iter().map(|d| stamped(d, &time)).collect();
        for (i, envelope) in envelopes.iter().enumerate() {
            let position = first + i64::try_from(i).expect("capacity checked");
            let rid = child(envelope, "record_id")
                .as_str()
                .expect("validated ID")
                .to_owned();
            let profile = child(envelope, "profile")
                .as_str()
                .expect("routing")
                .to_owned();
            let record_type = child(envelope, "record_type")
                .as_str()
                .expect("routing")
                .to_owned();
            let edition = i64::try_from(
                child(envelope, "schema_version")
                    .as_integer()
                    .expect("edition"),
            )
            .expect("safe edition");
            let bytes = canonical_bytes(envelope)?;
            tx.execute(&format!("INSERT INTO {ns}.mneme_record(store_id,position,record_id,profile,record_type,schema_version,envelope) VALUES ($1,$2,$3,$4,$5,$6,$7)"),&[&sid,&position,&rid,&profile,&record_type,&edition,&bytes])?;
        }
        tx.execute(&format!("INSERT INTO {ns}.mneme_operation(store_id,source_namespace,source_operation_id,mutation,first_position,last_position,recorded_at) VALUES ($1,$2,$3,$4,$5,$6,$7)"),&[&sid,&identity.source_namespace,&key,&mutation,&first,&last,&time])?;
        tx.execute(
            &format!(
                "UPDATE {ns}.mneme_store SET tail=$1 WHERE store_id OPERATOR(pg_catalog.=) $2"
            ),
            &[&last, &sid],
        )?;
        tx.commit()?;
        Ok(receipt(
            &sid,
            &identity.source_namespace,
            &identity.source_operation_id,
            first,
            last,
            &time,
            envelopes,
        ))
    }

    /// One record-counted page from a consistent snapshot; batches may span pages.
    pub fn read_history(
        &self,
        sid: Input<'_>,
        since: Input<'_>,
        limit: Input<'_>,
    ) -> Result<Value, StoreError> {
        let values = staged(&[("store_id", sid), ("since", since), ("limit", limit)])?;
        let sid = id_rule(&values[0]).map_err(|e| e.named("store_id", true))?;
        let since = boundary_rule(&values[1]).map_err(|e| e.named("since", true))?;
        let count = values[2]
            .as_integer()
            .filter(|v| *v >= 1)
            .ok_or_else(|| failure(ErrorCode::PersistenceLimitInvalid, Some("limit")))?;
        let count = i64::try_from(count).expect("generic safe integer");
        let position = i64::try_from(since.position).expect("generic safe integer");
        let mut state = self.state.lock().map_err(|_| StoreError::Poisoned)?;
        let ns = ident(&state.namespace);
        let mut tx = state
            .client
            .build_transaction()
            .isolation_level(IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()?;
        let tail: i64 = tx
            .query_opt(
                &format!(
                    "SELECT tail FROM {ns}.mneme_store WHERE store_id OPERATOR(pg_catalog.=) $1"
                ),
                &[&sid],
            )?
            .ok_or_else(|| failure(ErrorCode::PersistenceStoreUnknown, Some("store_id")))?
            .get(0);
        if since.store_id != sid {
            return Err(failure(
                ErrorCode::HistoryBoundaryIncomparable,
                Some("since"),
            ));
        }
        if position > tail {
            return Err(failure(ErrorCode::HistoryBoundaryInvalid, Some("since")));
        }
        let rows=tx.query(&format!("SELECT position,envelope FROM {ns}.mneme_record WHERE store_id OPERATOR(pg_catalog.=) $1 AND position OPERATOR(pg_catalog.>) $2 ORDER BY position LIMIT $3"),&[&sid,&position,&count])?;
        let mut through = position;
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            through = row.get(0);
            let envelope = decode_json(&row.get::<_, Vec<u8>>(1))?;
            entries.push(record(vec![
                ("position", Value::integer(i128::from(through))),
                ("record", envelope),
            ]));
        }
        tx.commit()?;
        Ok(record(vec![
            ("entries", Value::array(entries)),
            ("through", boundary(&sid, through)),
            ("tail", boundary(&sid, tail)),
        ]))
    }
}

/// Normative DDL with built-in types/operators and quoted resolved namespace.
#[doc(hidden)]
pub fn ddl(namespace: &str) -> Vec<String> {
    let ns = ident(namespace);
    vec![
        format!(
            "CREATE TABLE {ns}.mneme_schema(singleton pg_catalog.bool PRIMARY KEY DEFAULT true CHECK(singleton),version pg_catalog.int4 NOT NULL)"
        ),
        format!(
            "CREATE TABLE {ns}.mneme_store(store_id pg_catalog.text PRIMARY KEY,tail pg_catalog.int8 NOT NULL DEFAULT 0 CHECK(tail OPERATOR(pg_catalog.>=) 0 AND tail OPERATOR(pg_catalog.<=) 9007199254740991))"
        ),
        format!(
            "CREATE TABLE {ns}.mneme_record(store_id pg_catalog.text NOT NULL REFERENCES {ns}.mneme_store,position pg_catalog.int8 NOT NULL CHECK(position OPERATOR(pg_catalog.>=) 1 AND position OPERATOR(pg_catalog.<=) 9007199254740991),record_id pg_catalog.text NOT NULL,profile pg_catalog.text NOT NULL,record_type pg_catalog.text NOT NULL,schema_version pg_catalog.int8 NOT NULL,envelope pg_catalog.bytea NOT NULL,PRIMARY KEY(store_id,position),UNIQUE(store_id,record_id))"
        ),
        format!(
            "CREATE TABLE {ns}.mneme_operation(store_id pg_catalog.text NOT NULL REFERENCES {ns}.mneme_store,source_namespace pg_catalog.text NOT NULL,source_operation_id pg_catalog.bytea NOT NULL,mutation pg_catalog.bytea NOT NULL,first_position pg_catalog.int8 NOT NULL,last_position pg_catalog.int8 NOT NULL,recorded_at pg_catalog.text NOT NULL,PRIMARY KEY(store_id,source_namespace,source_operation_id))"
        ),
    ]
}
