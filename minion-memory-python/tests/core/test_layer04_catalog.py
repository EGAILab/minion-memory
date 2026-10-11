"""CE-04-01 witnesses: the v1 catalog trust boundary of the PostgreSQL reference backend.

Oracles come from the agreed checkpoint (`assurance/convergence/CE-04-01-classification.md` rev. 2,
Codex APPROVED `bfb6cad3`) and `spec/persistence.md` ("Open and initialize", "v1 catalog contract",
"Trust scope"; Owner G-089..G-092). Row IDs are the characterization's (`0f7ac1bf`) and the
challenge additions (CH..). Four kinds of evidence are kept apart, as the checkpoint requires:

* **public** - a fresh schema, the row's setup, then the public `open` and protocol N;
* **direct engine control** - CH12, the store's actual SQL texts in a poisoned session;
* **transaction control** - initialization pins and postcondition, and the X09 lock observation;
* **predicate** - the pure comparison over a synthesized catalog, for states this host or an
  ordinary role cannot construct. Never reported as a public execution.
"""

from __future__ import annotations

import dataclasses
import json
import threading
import time
import uuid
from collections.abc import Callable, Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import Any

import pgdb
import psycopg
import pytest
from psycopg import errors, sql

from minion_memory.core import MnemeValidationError
from minion_memory.persistence import SCHEMA_OPEN_LOCK_KEY, PostgresHistoryStore, postgres
from minion_memory.persistence._v1_catalog import V1_CATALOG
from minion_memory.persistence.catalog import (
    ENVIRONMENT_SQL,
    NAMESPACE_SQL,
    QUERIES,
    Catalog,
    environment_mismatch,
    fetch,
    mismatch,
)

T = "2026-10-11T08:00:00.000000Z"
WRITER = {"profile": "learning", "recognized": [{"record_type": "attempt", "schema_version": 1}]}
INCOMPATIBLE = "persistence.schema_incompatible"
SHARED_PINS = Path(__file__).resolve().parents[3] / "reference-backend" / "postgres14-v1-catalog.json"

# The normative DDL of spec/persistence.md ("Reference backend"), verbatim, unqualified.
NORMATIVE_DDL = """
CREATE TABLE mneme_schema    (singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
                              version integer NOT NULL);
CREATE TABLE mneme_store     (store_id text PRIMARY KEY,
                              tail bigint NOT NULL DEFAULT 0
                                   CHECK (tail BETWEEN 0 AND 9007199254740991));
CREATE TABLE mneme_record    (store_id text NOT NULL REFERENCES mneme_store,
                              position bigint NOT NULL
                                   CHECK (position BETWEEN 1 AND 9007199254740991),
                              record_id text NOT NULL,
                              profile text NOT NULL, record_type text NOT NULL,
                              schema_version bigint NOT NULL,
                              envelope bytea NOT NULL,
                              PRIMARY KEY (store_id, position),
                              UNIQUE (store_id, record_id));
CREATE TABLE mneme_operation (store_id text NOT NULL REFERENCES mneme_store,
                              source_namespace text NOT NULL,
                              source_operation_id bytea NOT NULL,
                              mutation bytea NOT NULL,
                              first_position bigint NOT NULL, last_position bigint NOT NULL,
                              recorded_at text NOT NULL,
                              PRIMARY KEY (store_id, source_namespace, source_operation_id));
"""


def draft(rid: str, **extra: object) -> dict[str, object]:
    return {
        "record_id": rid,
        "profile": "learning",
        "record_type": "attempt",
        "schema_version": 1,
        "compatibility": "required",
        **extra,
    }


def op(i: str) -> dict[str, str]:
    return {"source_namespace": "app", "source_operation_id": i}


def schema_of(params: dict[str, str]) -> str:
    return params["options"].split("search_path=")[1].split()[0]


def with_options(params: dict[str, str], options: str) -> dict[str, str]:
    return params | {"options": f"{params['options']} {options}".strip()}


def snapshot(params: dict[str, str]) -> Catalog:
    """The catalog of the params' current schema, read by a plain connection."""
    with psycopg.connect(autocommit=True, **params) as c:  # type: ignore[arg-type]
        row = c.execute(NAMESPACE_SQL).fetchone()
        assert row is not None
        return fetch(c, row[0])


def execute(params: dict[str, str], statements: list[str]) -> None:
    with psycopg.connect(autocommit=True, **params) as c:  # type: ignore[arg-type]
        for statement in statements:
            c.execute(statement.replace("{ns}", schema_of(params)))


def protocol_n(params: dict[str, str], payload: object = None) -> None:
    """The characterization's literal protocol N: create, commit, identical retry, read, tail."""
    extra = {} if payload is None else {"payload": payload}
    drafts = [draft("r1", **extra), draft("r2")]
    with PostgresHistoryStore.open(params, lambda _: T) as s:
        s.create_store("s")
        receipt = s.commit_history("s", WRITER, op("one"), drafts)
        assert (receipt["first_position"], receipt["last_position"]) == (1, 2)
        assert s.commit_history("s", WRITER, op("one"), drafts) == receipt
        page = s.read_history("s", {"store_id": "s", "position": 0}, 10)
        assert [e["position"] for e in page["entries"]] == [1, 2]
        assert [e["record"] for e in page["entries"]] == receipt["records"]
        assert page["through"] == page["tail"] == s.store_tail("s") == {"store_id": "s", "position": 2}
        assert receipt["records"][0]["recorded_at"] == T


def refused_without_change(params: dict[str, str]) -> None:
    before = snapshot(params)
    with pytest.raises(MnemeValidationError) as info:
        PostgresHistoryStore.open(params, lambda _: T)
    assert info.value.code == INCOMPATIBLE
    assert snapshot(params) == before


@contextmanager
def initialized() -> Iterator[dict[str, str]]:
    with pgdb.fresh_backend() as params:
        PostgresHistoryStore.open(params, lambda _: T).close()
        yield params


@contextmanager
def other_schema(params: dict[str, str]) -> Iterator[str]:
    name = f"o_{uuid.uuid4().hex[:12]}"
    execute(params, [f"CREATE SCHEMA {name}", f"CREATE TABLE {name}.mneme_record (poison text)"])
    try:
        yield name
    finally:
        execute(params, [f"DROP SCHEMA {name} CASCADE"])


# ------------------------------------------------------------------------------- pins (G-089, G-091)


def _tuples(value: Any) -> Any:
    return tuple(_tuples(v) for v in value) if isinstance(value, list) else value


def test_the_embedded_pins_equal_the_shared_pin_file() -> None:
    shared = json.loads(SHARED_PINS.read_text(encoding="utf-8"))
    assert shared["postgresql_major"] == 14
    assert Catalog(**{k: _tuples(v) for k, v in shared["catalog"].items()}) == V1_CATALOG


def test_the_normative_ddl_produces_the_pinned_catalog() -> None:
    with pgdb.fresh_backend() as params:
        execute(params, [*NORMATIVE_DDL.split(";")[:4], "INSERT INTO mneme_schema (version) VALUES (1)"])
        assert snapshot(params) == V1_CATALOG


def test_the_initializer_produces_the_pinned_catalog_with_three_checks_and_two_defaults() -> None:
    with initialized() as params:
        found = snapshot(params)
        assert found == V1_CATALOG
        assert [row[0] for row in found.constraints if row[2] == "c"] == [
            "mneme_record",
            "mneme_schema",
            "mneme_store",
        ]
        assert [row[:2] for row in found.defaults] == [("mneme_schema", "singleton"), ("mneme_store", "tail")]
        assert len(found.triggers) == 8 and all(row[1] and row[2] == "O" for row in found.triggers)


# ------------------------------------------------------------------------------- public: refused

# Setup SQL runs in the fresh v1 schema (search_path = the schema). Row IDs: characterization rows,
# then the challenge additions and the per-table trigger and rule witnesses (§6).
_TRIGGER_FN = "CREATE FUNCTION {ns}.probe_fn() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RETURN NULL; END$$"
_NEW_FN = "CREATE FUNCTION {ns}.probe_fn() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RETURN NEW; END$$"
_LOOSE = "CREATE COLLATION probe_loose (provider=icu,locale='und-u-ka-shifted',deterministic=false)"
REFUSED: dict[str, list[str]] = {
    # persistence
    "R01": ["ALTER TABLE mneme_record SET UNLOGGED"],
    "R02": ["ALTER TABLE mneme_operation SET UNLOGGED"],
    "R04": ["ALTER TABLE mneme_schema SET UNLOGGED"],
    "R31": [
        "ALTER TABLE mneme_record SET UNLOGGED",
        "ALTER TABLE mneme_operation SET UNLOGGED",
        "ALTER TABLE mneme_store SET UNLOGGED",
    ],
    # inheritance
    "R05": [
        "CREATE TABLE probe_child () INHERITS (mneme_record)",
        "INSERT INTO probe_child VALUES ('s', 99, 'shadow', 'learning', 'attempt', 1, '\\x7b7d')",
    ],
    "R06": [
        "CREATE TABLE probe_parent (LIKE mneme_record INCLUDING CONSTRAINTS)",
        "ALTER TABLE mneme_record INHERIT probe_parent",
    ],
    # row security and policies
    "R07": [
        "ALTER TABLE mneme_record ENABLE ROW LEVEL SECURITY",
        "CREATE POLICY probe_policy ON mneme_record USING (false) WITH CHECK (false)",
    ],
    "R08": [
        "ALTER TABLE mneme_record ENABLE ROW LEVEL SECURITY",
        "ALTER TABLE mneme_record FORCE ROW LEVEL SECURITY",
        "CREATE POLICY probe_policy ON mneme_record USING (false) WITH CHECK (false)",
    ],
    "R09": [
        "ALTER TABLE mneme_record ENABLE ROW LEVEL SECURITY",
        "ALTER TABLE mneme_record FORCE ROW LEVEL SECURITY",
        "CREATE POLICY probe_policy ON mneme_record USING (true) WITH CHECK (true)",
    ],
    "R40": [
        "ALTER TABLE mneme_schema ENABLE ROW LEVEL SECURITY",
        "ALTER TABLE mneme_schema FORCE ROW LEVEL SECURITY",
        "CREATE POLICY probe_policy ON mneme_schema USING (false) WITH CHECK (true)",
    ],
    "R41": [
        "ALTER TABLE mneme_store ENABLE ROW LEVEL SECURITY",
        "ALTER TABLE mneme_store FORCE ROW LEVEL SECURITY",
        "CREATE POLICY probe_policy ON mneme_store USING (false) WITH CHECK (true)",
    ],
    "R42": [
        "ALTER TABLE mneme_operation ENABLE ROW LEVEL SECURITY",
        "ALTER TABLE mneme_operation FORCE ROW LEVEL SECURITY",
        "CREATE POLICY probe_policy ON mneme_operation USING (false) WITH CHECK (true)",
    ],
    "R43": [
        "ALTER TABLE mneme_record ENABLE ROW LEVEL SECURITY",
        "ALTER TABLE mneme_record FORCE ROW LEVEL SECURITY",
        "CREATE POLICY probe_policy ON mneme_record USING (false) WITH CHECK (true)",
    ],
    "CH05": ["CREATE POLICY probe_policy ON mneme_record USING (false)"],
    # row security alone, without any policy (relation properties only)
    "RLS-enabled": ["ALTER TABLE mneme_record ENABLE ROW LEVEL SECURITY"],
    "RLS-forced": ["ALTER TABLE mneme_operation FORCE ROW LEVEL SECURITY"],
    "RLS-enabled-forced": [
        "ALTER TABLE mneme_store ENABLE ROW LEVEL SECURITY",
        "ALTER TABLE mneme_store FORCE ROW LEVEL SECURITY",
    ],
    # rules, one on each table
    "R10": ["CREATE RULE probe_rule AS ON INSERT TO mneme_record DO INSTEAD NOTHING"],
    "R11": ["CREATE RULE probe_rule AS ON UPDATE TO mneme_store DO INSTEAD NOTHING"],
    "R12": ["CREATE RULE probe_rule AS ON INSERT TO mneme_record DO ALSO NOTHING"],
    "RULE-operation": ["CREATE RULE probe_rule AS ON INSERT TO mneme_operation DO ALSO NOTHING"],
    "RULE-schema": ["CREATE RULE probe_rule AS ON UPDATE TO mneme_schema DO ALSO NOTHING"],
    # user triggers in every timing and enable state, one on each table
    "R13": [
        _TRIGGER_FN,
        "CREATE TRIGGER probe_trigger BEFORE INSERT ON mneme_record FOR EACH ROW EXECUTE FUNCTION probe_fn()",
    ],
    "R14": [
        _NEW_FN,
        "CREATE TRIGGER probe_trigger AFTER INSERT ON mneme_record FOR EACH ROW EXECUTE FUNCTION probe_fn()",
    ],
    "R15": [
        _TRIGGER_FN,
        "CREATE TRIGGER probe_trigger AFTER INSERT ON mneme_record FOR EACH STATEMENT"
        " EXECUTE FUNCTION probe_fn()",
    ],
    "R16": [
        _TRIGGER_FN,
        "CREATE TRIGGER probe_trigger BEFORE INSERT ON mneme_record FOR EACH STATEMENT"
        " EXECUTE FUNCTION probe_fn()",
    ],
    "R17": [
        _NEW_FN,
        "CREATE TRIGGER probe_trigger BEFORE INSERT ON mneme_record FOR EACH ROW EXECUTE FUNCTION probe_fn()",
    ],
    "R18": [
        _TRIGGER_FN,
        "CREATE TRIGGER probe_trigger BEFORE INSERT ON mneme_record FOR EACH ROW EXECUTE FUNCTION probe_fn()",
        "ALTER TABLE mneme_record DISABLE TRIGGER probe_trigger",
    ],
    "R33": [
        _TRIGGER_FN,
        "CREATE TRIGGER probe_trigger BEFORE INSERT ON mneme_record FOR EACH ROW EXECUTE FUNCTION probe_fn()",
        "ALTER TABLE mneme_record ENABLE REPLICA TRIGGER probe_trigger",
    ],
    "CH06": [
        _TRIGGER_FN,
        "CREATE TRIGGER probe_trigger BEFORE INSERT ON mneme_operation FOR EACH ROW"
        " EXECUTE FUNCTION probe_fn()",
    ],
    "TRIGGER-store": [
        _NEW_FN,
        "CREATE TRIGGER probe_trigger BEFORE UPDATE ON mneme_store FOR EACH ROW EXECUTE FUNCTION probe_fn()",
    ],
    "TRIGGER-schema": [
        _NEW_FN,
        "CREATE TRIGGER probe_trigger BEFORE UPDATE ON mneme_schema FOR EACH ROW EXECUTE FUNCTION probe_fn()",
    ],
    # relation options, kind and prefix
    "R19": ["ALTER TABLE mneme_record SET (fillfactor=50)"],
    "R20": ["ALTER TABLE mneme_record SET (autovacuum_enabled=false)"],
    "TOAST-options": ["ALTER TABLE mneme_record SET (toast.autovacuum_enabled=false)"],
    "R27": [
        "ALTER TABLE mneme_record RENAME TO probe_saved",
        "CREATE VIEW mneme_record AS SELECT * FROM probe_saved",
    ],
    "R28": [
        "ALTER TABLE mneme_record RENAME TO probe_saved",
        "CREATE MATERIALIZED VIEW mneme_record AS SELECT * FROM probe_saved",
    ],
    "R35": [
        "ALTER TABLE mneme_record RENAME TO probe_saved",
        "CREATE TABLE mneme_record (LIKE probe_saved INCLUDING CONSTRAINTS) PARTITION BY RANGE (position)",
    ],
    "CH01": [
        "DROP TABLE mneme_record",
        "CREATE TABLE mneme_record (store_id text NOT NULL REFERENCES mneme_store,"
        " position bigint NOT NULL CHECK (position BETWEEN 1 AND 9007199254740991),"
        " record_id text NOT NULL, profile text NOT NULL, record_type text NOT NULL,"
        " schema_version bigint NOT NULL, envelope bytea NOT NULL,"
        " PRIMARY KEY (store_id, position), UNIQUE (store_id, record_id)) PARTITION BY LIST (store_id)",
        "CREATE TABLE part_default PARTITION OF mneme_record DEFAULT",
    ],
    "R37": ["CREATE TABLE mneme_extra (x integer)"],
    "CH09a": ["CREATE VIEW mneme_extra AS SELECT 1 AS x"],
    "CH09b": ["CREATE SEQUENCE mneme_extra"],
    "CH02": ["CREATE TABLE outside_ref (sid text REFERENCES mneme_store ON UPDATE CASCADE)"],
    "CH04": [
        "CREATE TYPE probe_row AS (store_id text, position bigint, record_id text, profile text,"
        " record_type text, schema_version bigint, envelope bytea)",
        "ALTER TABLE mneme_record OF probe_row",
    ],
    # columns
    "C01": [_LOOSE, "ALTER TABLE mneme_store ALTER COLUMN store_id TYPE text COLLATE probe_loose"],
    "C02": [_LOOSE, "ALTER TABLE mneme_record ALTER COLUMN record_id TYPE text COLLATE probe_loose"],
    "C03": [
        _LOOSE,
        "ALTER TABLE mneme_operation ALTER COLUMN source_namespace TYPE text COLLATE probe_loose",
    ],
    "C04": [_LOOSE, "ALTER TABLE mneme_record ALTER COLUMN profile TYPE text COLLATE probe_loose"],
    "C05": ['ALTER TABLE mneme_store ALTER COLUMN store_id TYPE text COLLATE "C"'],
    "C06": ['ALTER TABLE mneme_store ALTER COLUMN store_id TYPE text COLLATE "POSIX"'],
    "C23": [
        "CREATE COLLATION probe_det (provider=icu,locale='und-u-ka-shifted',deterministic=true)",
        "ALTER TABLE mneme_store ALTER COLUMN store_id TYPE text COLLATE probe_det",
    ],
    "C07": ["ALTER TABLE mneme_record ALTER COLUMN schema_version ADD GENERATED ALWAYS AS IDENTITY"],
    "C08": ["ALTER TABLE mneme_record ALTER COLUMN schema_version ADD GENERATED BY DEFAULT AS IDENTITY"],
    "C09": ["ALTER TABLE mneme_schema ALTER COLUMN version ADD GENERATED ALWAYS AS IDENTITY"],
    "C10": [
        "ALTER TABLE mneme_store DROP COLUMN tail",
        "ALTER TABLE mneme_store ADD COLUMN tail bigint GENERATED ALWAYS AS (0::bigint) STORED NOT NULL",
        "ALTER TABLE mneme_store ADD CONSTRAINT mneme_store_tail_check"
        " CHECK (tail BETWEEN 0 AND 9007199254740991)",
    ],
    "C24": [
        "ALTER TABLE mneme_store DROP COLUMN tail",
        "ALTER TABLE mneme_store ADD COLUMN tail bigint GENERATED ALWAYS AS (0) STORED NOT NULL"
        " CHECK(tail BETWEEN 0 AND 9007199254740991)",
    ],
    "C11": ["ALTER TABLE mneme_record ALTER COLUMN schema_version SET DEFAULT 2"],
    "C12": ["ALTER TABLE mneme_store ALTER COLUMN tail DROP DEFAULT"],
    # a different value for an existing default (the defaults part only)
    "DEFAULT-tail": ["ALTER TABLE mneme_store ALTER COLUMN tail SET DEFAULT 1"],
    "DEFAULT-singleton": ["ALTER TABLE mneme_schema ALTER COLUMN singleton SET DEFAULT false"],
    "C13": ["ALTER TABLE mneme_record ALTER COLUMN envelope SET STORAGE PLAIN"],
    "C14": ["ALTER TABLE mneme_record ALTER COLUMN envelope SET STORAGE EXTERNAL"],
    "C15": ["ALTER TABLE mneme_record ALTER COLUMN envelope SET STORAGE MAIN"],
    "C17": ["ALTER TABLE mneme_record ALTER COLUMN envelope SET COMPRESSION pglz"],
    "C18": ["ALTER TABLE mneme_record ALTER COLUMN envelope SET COMPRESSION lz4"],
    "C19": [
        "CREATE DOMAIN probe_domain AS bigint CHECK (VALUE = 1)",
        "ALTER TABLE mneme_record ALTER COLUMN schema_version TYPE probe_domain",
    ],
    "C25": ["ALTER TABLE mneme_store RENAME COLUMN tail TO probe_tail"],
    "C26": ["ALTER TABLE mneme_schema RENAME COLUMN version TO probe_version"],
    "C27": ["ALTER TABLE mneme_store ALTER COLUMN tail TYPE integer"],
    "C28": ["ALTER TABLE mneme_record ALTER COLUMN envelope DROP NOT NULL"],
    # constraints
    "K01": [
        "ALTER TABLE mneme_record DROP CONSTRAINT mneme_record_store_id_record_id_key",
        "ALTER TABLE mneme_record ADD CONSTRAINT mneme_record_store_id_record_id_key"
        " UNIQUE (store_id, record_id)"
        " DEFERRABLE",
    ],
    "K02": [
        "ALTER TABLE mneme_record DROP CONSTRAINT mneme_record_store_id_record_id_key",
        "ALTER TABLE mneme_record ADD CONSTRAINT mneme_record_store_id_record_id_key"
        " UNIQUE (store_id, record_id)"
        " DEFERRABLE INITIALLY DEFERRED",
    ],
    "K03": [
        "ALTER TABLE mneme_record DROP CONSTRAINT mneme_record_position_check",
        "ALTER TABLE mneme_record ADD CONSTRAINT mneme_record_position_check"
        " CHECK (position BETWEEN 1 AND 9007199254740991) NOT VALID",
    ],
    "K04": [
        "ALTER TABLE mneme_record DROP CONSTRAINT mneme_record_store_id_fkey",
        "ALTER TABLE mneme_record ADD CONSTRAINT mneme_record_store_id_fkey FOREIGN KEY (store_id)"
        " REFERENCES mneme_store NOT VALID",
    ],
    "K05": [
        "ALTER TABLE mneme_record DROP CONSTRAINT mneme_record_store_id_fkey",
        "ALTER TABLE mneme_record ADD CONSTRAINT mneme_record_store_id_fkey FOREIGN KEY (store_id)"
        " REFERENCES mneme_store ON DELETE CASCADE",
    ],
    "K06": [
        "ALTER TABLE mneme_record DROP CONSTRAINT mneme_record_store_id_fkey",
        "ALTER TABLE mneme_record ADD CONSTRAINT mneme_record_store_id_fkey FOREIGN KEY (store_id)"
        " REFERENCES mneme_store ON UPDATE CASCADE",
    ],
    "K07": [
        "ALTER TABLE mneme_record DROP CONSTRAINT mneme_record_store_id_fkey",
        "ALTER TABLE mneme_record ADD CONSTRAINT mneme_record_store_id_fkey FOREIGN KEY (store_id)"
        " REFERENCES mneme_store ON DELETE RESTRICT",
    ],
    "K08": [
        "ALTER TABLE mneme_record DROP CONSTRAINT mneme_record_store_id_fkey",
        "ALTER TABLE mneme_record ADD CONSTRAINT mneme_record_store_id_fkey FOREIGN KEY (store_id)"
        " REFERENCES mneme_store DEFERRABLE INITIALLY DEFERRED",
    ],
    "K10": ["ALTER TABLE mneme_record ADD CONSTRAINT probe_check CHECK (true)"],
    "K11": ["ALTER TABLE mneme_record ADD CONSTRAINT probe_check CHECK (profile <> 'learning')"],
    # indexes
    "I01": ["CREATE INDEX probe_idx ON mneme_record (profile)"],
    "I02": ["CREATE UNIQUE INDEX probe_idx ON mneme_record (profile)"],
    "I03": ["CREATE INDEX probe_idx ON mneme_record (profile) WHERE schema_version = 1"],
    "I04": ["CREATE INDEX probe_idx ON mneme_record ((lower(profile)))"],
    "I05": ["CREATE INDEX probe_idx ON mneme_record USING hash (profile)"],
    "I06": ["CREATE UNIQUE INDEX probe_idx ON mneme_record (store_id, record_id)"],
    "I07": ["CREATE INDEX probe_idx ON mneme_record (record_id text_pattern_ops)"],
    "I08": ["CREATE UNIQUE INDEX probe_idx ON mneme_record (record_id) WHERE schema_version = 1"],
    "I09": ["CREATE UNIQUE INDEX probe_idx ON mneme_record ((lower(record_id)))"],
    "I10": ["CREATE UNIQUE INDEX probe_idx ON mneme_record (record_id text_pattern_ops)"],
    "I11": ["ALTER INDEX mneme_record_pkey SET (fillfactor=50)"],
    "I12": ["ALTER TABLE mneme_record CLUSTER ON mneme_record_pkey"],
    "I13": ["ALTER TABLE mneme_record REPLICA IDENTITY FULL"],
    "I19": ["CREATE INDEX probe_idx ON mneme_record ((record_id::integer))"],
    "I20": ["CREATE INDEX probe_idx ON mneme_record (record_id) WHERE record_id::integer > 0"],
    # the version row
    "H01": ["UPDATE mneme_schema SET version = 2"],
    "H02": ["DELETE FROM mneme_schema"],
}


@pytest.mark.parametrize("row", sorted(REFUSED))
def test_public_open_refuses_an_incompatible_catalog_and_changes_nothing(row: str) -> None:
    with initialized() as params:
        execute(params, REFUSED[row])
        refused_without_change(params)


def test_public_i14_an_invalid_not_ready_extra_index_is_refused() -> None:
    with initialized() as params:
        with PostgresHistoryStore.open(params, lambda _: T) as s:
            s.create_store("seed")
            s.commit_history("seed", WRITER, op("seed"), [draft("a"), draft("b")])
        with pytest.raises(errors.UniqueViolation):
            execute(params, ["CREATE UNIQUE INDEX CONCURRENTLY probe_idx ON mneme_record (profile)"])
        refused_without_change(params)


def test_public_i18_an_invalid_but_ready_extra_unique_index_is_refused() -> None:
    gate = 73700401
    with initialized() as params:
        ns = schema_of(params)
        execute(
            params,
            [
                "INSERT INTO mneme_store VALUES ('seed', 2), ('other', 0)",
                "INSERT INTO mneme_record VALUES ('seed', 1, 'r1', 'learning', 'attempt', 1, '\\x7b7d'),"
                " ('seed', 2, 'r2', 'learning', 'attempt', 1, '\\x7b7d')",
                f"CREATE FUNCTION {ns}.probe_pause(text) RETURNS text LANGUAGE plpgsql IMMUTABLE AS"
                f" $$BEGIN IF $1 = 'r2' THEN PERFORM pg_advisory_xact_lock({gate}); END IF; RETURN $1; END$$",
            ],
        )
        failure: list[BaseException] = []

        def build() -> None:
            try:
                execute(
                    params,
                    [
                        "SET max_parallel_maintenance_workers = 0",
                        "CREATE UNIQUE INDEX CONCURRENTLY probe_idx"
                        " ON mneme_record ((probe_pause(record_id)))",
                    ],
                )
            except BaseException as error:
                failure.append(error)

        with psycopg.connect(autocommit=True, **params) as holder:  # type: ignore[arg-type]
            holder.execute("SELECT pg_advisory_lock(%s)", (gate,))
            builder = threading.Thread(target=build)
            builder.start()
            deadline = time.monotonic() + 120
            waiting = (
                "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND objid = %s AND NOT granted"
            )
            while holder.execute(waiting, (gate,)).fetchone() != (1,):
                assert time.monotonic() < deadline, "the index build never reached the gate"
                time.sleep(0.2)
            # While the build waits in its first scan (index not yet ready), a valid per-store reuse.
            execute(
                params,
                ["INSERT INTO mneme_record VALUES ('other', 1, 'r1', 'learning', 'attempt', 1, '\\x7b7d')"],
            )
            holder.execute("SELECT pg_advisory_unlock(%s)", (gate,))
        builder.join(120)
        assert len(failure) == 1 and isinstance(failure[0], errors.UniqueViolation)
        with psycopg.connect(**params) as c:  # type: ignore[arg-type]
            assert c.execute(
                "SELECT indisvalid, indisready FROM pg_index WHERE indexrelid = 'probe_idx'::regclass"
            ).fetchone() == (False, True)
        refused_without_change(params)


def test_public_x08_a_shadow_renderer_cannot_hide_a_domain() -> None:
    with initialized() as params:
        execute(
            params,
            [
                "CREATE DOMAIN probe_bytes AS bytea CHECK (octet_length(VALUE) <= 10)",
                "ALTER TABLE mneme_record ALTER COLUMN envelope TYPE probe_bytes",
                "CREATE FUNCTION {ns}.format_type(oid, integer) RETURNS text LANGUAGE sql AS $$"
                " SELECT CASE WHEN $1 = '{ns}.probe_bytes'::regtype THEN 'bytea'"
                " ELSE pg_catalog.format_type($1, $2) END $$",
            ],
        )
        refused_without_change(with_options(params, f"-c search_path={schema_of(params)},pg_catalog"))


def test_public_x11_an_aggregate_overload_cannot_hide_version_2() -> None:
    with initialized() as params:
        execute(
            params,
            [
                "UPDATE mneme_schema SET version = 2",
                "CREATE FUNCTION {ns}.array_agg(integer) RETURNS integer[] LANGUAGE sql IMMUTABLE"
                " AS $$SELECT ARRAY[1]$$",
            ],
        )
        refused_without_change(params)


def test_public_x02_the_current_schema_is_the_namespace() -> None:
    with initialized() as params, other_schema(params) as other:
        refused_without_change(params | {"options": f"-c search_path={other},{schema_of(params)}"})


def test_public_ch09c_a_prefixed_view_blocks_initialization_and_nothing_is_created() -> None:
    with pgdb.fresh_backend() as params:
        execute(params, ["CREATE VIEW mneme_extra AS SELECT 1 AS x"])
        refused_without_change(params)
        assert [row[0] for row in snapshot(params).relations] == ["mneme_extra"]


@pytest.mark.parametrize(
    ("name", "statement"),
    [
        ("mneme_store", "CREATE VIEW mneme_store AS SELECT 'x'::text AS store_id"),
        ("mneme_record", "CREATE SEQUENCE mneme_record"),
    ],
)
def test_public_ch09c_a_non_table_with_a_v1_name_blocks_initialization(name: str, statement: str) -> None:
    with pgdb.fresh_backend() as params:
        execute(params, [statement])
        refused_without_change(params)
        assert [row[0] for row in snapshot(params).relations] == [name]


# ------------------------------------------------------------------------------- public: accepted

ACCEPTED: dict[str, tuple[list[str], str]] = {
    "B01": ([], ""),
    "R21": (["ALTER TABLE mneme_record SET TABLESPACE mneme_ts"], ""),
    "R22": (["ALTER TABLE mneme_record OWNER TO CURRENT_USER"], ""),
    "R36": (["CREATE TABLE probe_extra (x integer)"], ""),
    "R38": (["CREATE TABLE probe_child () INHERITS (mneme_record)", "DROP TABLE probe_child"], ""),
    "R39": (
        [
            "CREATE RULE probe_rule AS ON INSERT TO mneme_record DO ALSO NOTHING",
            "DROP RULE probe_rule ON mneme_record",
        ],
        "",
    ),
    "C16": (["ALTER TABLE mneme_record ALTER COLUMN envelope SET STORAGE EXTENDED"], ""),
    "C20": (
        [
            "ALTER TABLE mneme_record ADD COLUMN probe_dead text",
            "ALTER TABLE mneme_record DROP COLUMN probe_dead",
        ],
        "",
    ),
    "C21": (["ALTER TABLE mneme_record ALTER COLUMN envelope SET STATISTICS 0"], ""),
    "C22": (
        [
            "ALTER TABLE mneme_record DROP COLUMN schema_version",
            "ALTER TABLE mneme_record ADD COLUMN schema_version bigint NOT NULL",
        ],
        "",
    ),
    "K09": (
        ["ALTER TABLE mneme_record RENAME CONSTRAINT mneme_record_store_id_record_id_key TO probe_unique"],
        "",
    ),
    "CH08": (["CREATE STATISTICS probe_stats ON profile, record_type FROM mneme_record"], ""),
    "S01": ([], "-c default_transaction_isolation=repeatable\\ read"),
    "S02": ([], "-c default_transaction_isolation=serializable"),
    "S03": ([], "-c synchronous_commit=off"),
    "S04": ([], "-c default_transaction_read_only=on"),
    "S05": ([], "-c row_security=off"),
    "S07": ([], "-c quote_all_identifiers=on"),
    "S08": ([], "-c standard_conforming_strings=off"),
    "S09": ([], "-c timezone=Asia/Tokyo"),
    "S11": ([], "-c client_encoding=SQL_ASCII"),
    "X06": (
        [
            "CREATE FUNCTION {ns}.format_type(oid, integer) RETURNS text LANGUAGE plpgsql AS"
            " $$BEGIN RAISE EXCEPTION 'catalog shadow called'; END$$"
        ],
        "-c search_path={ns},pg_catalog",
    ),
    "X07": (
        [
            "CREATE FUNCTION {ns}.format_type(oid, integer) RETURNS text LANGUAGE plpgsql AS"
            " $$BEGIN RAISE EXCEPTION 'catalog shadow called'; END$$"
        ],
        "",
    ),
    "CH07": (
        [
            "CREATE FUNCTION {ns}.loud_eq(text, text) RETURNS boolean LANGUAGE plpgsql AS"
            " $$BEGIN RAISE EXCEPTION 'shadow = called'; END$$",
            "CREATE OPERATOR {ns}.= (LEFTARG = text, RIGHTARG = text, FUNCTION = {ns}.loud_eq)",
        ],
        "-c search_path={ns},pg_catalog",
    ),
    # The catalog queries' own operators: a raising user `=` for oid, int2 and name.
    "CH07b": (
        [
            "CREATE FUNCTION {ns}.loud_oid(oid, oid) RETURNS boolean LANGUAGE plpgsql AS"
            " $$BEGIN RAISE EXCEPTION 'shadow = called'; END$$",
            "CREATE OPERATOR {ns}.= (LEFTARG = oid, RIGHTARG = oid, FUNCTION = {ns}.loud_oid)",
            "CREATE FUNCTION {ns}.loud_int2(int2, int2) RETURNS boolean LANGUAGE plpgsql AS"
            " $$BEGIN RAISE EXCEPTION 'shadow = called'; END$$",
            "CREATE OPERATOR {ns}.= (LEFTARG = int2, RIGHTARG = int2, FUNCTION = {ns}.loud_int2)",
            "CREATE FUNCTION {ns}.loud_name(name, name) RETURNS boolean LANGUAGE plpgsql AS"
            " $$BEGIN RAISE EXCEPTION 'shadow = called'; END$$",
            "CREATE OPERATOR {ns}.= (LEFTARG = name, RIGHTARG = name, FUNCTION = {ns}.loud_name)",
        ],
        "-c search_path={ns},pg_catalog",
    ),
}


@pytest.mark.parametrize("row", sorted(ACCEPTED))
def test_public_open_accepts_a_v1_catalog_and_protocol_n_holds(row: str) -> None:
    setup, options = ACCEPTED[row]
    with initialized() as params:
        execute(params, setup)
        protocol_n(with_options(params, options.replace("{ns}", schema_of(params))))


@pytest.mark.parametrize("row", ["B02", "C16"])
def test_public_large_payloads_round_trip(row: str) -> None:
    with initialized() as params:
        execute(params, ACCEPTED["C16"][0] if row == "C16" else [])
        protocol_n(params, payload={"s": "x" * 20000})


def test_public_b03_s10_an_untouched_schema_opens_under_quote_all_identifiers() -> None:
    with initialized() as params:
        protocol_n(with_options(params, "-c quote_all_identifiers=on"))


def test_public_x01_x03_x05_other_schemas_in_the_path_are_irrelevant() -> None:
    with initialized() as params, other_schema(params) as other:
        ns = schema_of(params)
        for path in (f"{ns},{other}", f"no_such_schema,{ns}", f"{ns},pg_catalog"):
            protocol_n(params | {"options": f"-c search_path={path}"})
            execute(
                params, ["DELETE FROM mneme_record", "DELETE FROM mneme_operation", "DELETE FROM mneme_store"]
            )


# ------------------------------------------------------------------------------- engine errors propagate


@pytest.mark.parametrize(
    ("row", "setup", "sqlstate"),
    [
        ("R23", "REVOKE INSERT ON mneme_record FROM CURRENT_USER", "42501"),
        ("R24", "REVOKE SELECT ON mneme_record FROM CURRENT_USER", "42501"),
    ],
)
def test_public_a_permission_error_propagates_atomically(row: str, setup: str, sqlstate: str) -> None:
    with initialized() as params:
        execute(params, [setup])
        with PostgresHistoryStore.open(params, lambda _: T) as s:
            s.create_store("s")
            with pytest.raises(psycopg.Error) as info:
                s.commit_history("s", WRITER, op("one"), [draft("r1")])
            assert info.value.sqlstate == sqlstate
            assert s.store_tail("s") == {"store_id": "s", "position": 0}


def test_public_r25_open_propagates_a_permission_error() -> None:
    with initialized() as params:
        execute(params, ["REVOKE SELECT ON mneme_schema FROM CURRENT_USER"])
        with pytest.raises(errors.InsufficientPrivilege):
            PostgresHistoryStore.open(params, lambda _: T)


def test_public_x04_r34_no_current_schema_is_the_engine_error_and_creates_nothing() -> None:
    with pgdb.fresh_backend() as params:
        with pytest.raises(errors.InvalidSchemaName):
            PostgresHistoryStore.open(params | {"options": "-c search_path=no_such_schema"}, lambda _: T)
        with psycopg.connect(autocommit=True, **params) as peer:  # type: ignore[arg-type]
            peer.execute("SET temp_tablespaces = mneme_ts")
            peer.execute("CREATE TEMP TABLE mneme_schema (x integer)")
            temp = peer.execute("SELECT pg_my_temp_schema()::regnamespace::text").fetchone()
            assert temp is not None
            with pytest.raises(errors.InvalidSchemaName):
                PostgresHistoryStore.open(params | {"options": f"-c search_path={temp[0]}"}, lambda _: T)
        assert snapshot(params).relations == ()


@pytest.mark.parametrize("option", ["-c session_replication_role=replica", "-c server_version=13.0"])
def test_public_s06_v02_startup_refusals_propagate(option: str) -> None:
    with initialized() as params, pytest.raises(psycopg.OperationalError):
        PostgresHistoryStore.open(with_options(params, option), lambda _: T)


# ------------------------------------------------------------------------------- database environment (G-090)


@contextmanager
def database(encoding: str) -> Iterator[dict[str, str]]:
    values = pgdb.settings()
    name = f"mneme_env_{uuid.uuid4().hex[:12]}"
    admin = pgdb._params(values, values.get("PGDATABASE", "postgres"))
    with psycopg.connect(autocommit=True, **admin) as c:  # type: ignore[arg-type]
        c.execute(
            sql.SQL(
                "CREATE DATABASE {} TEMPLATE template0 ENCODING {} LC_COLLATE 'C' LC_CTYPE 'C'"
                " TABLESPACE mneme_ts"
            ).format(sql.Identifier(name), sql.Literal(encoding))
        )
    try:
        yield pgdb._params(values, name)
    finally:
        with psycopg.connect(autocommit=True, **admin) as c:  # type: ignore[arg-type]
            c.execute(sql.SQL("DROP DATABASE IF EXISTS {}").format(sql.Identifier(name)))


def _mneme_tables(params: dict[str, str]) -> list[Any]:
    with psycopg.connect(**params) as c:  # type: ignore[arg-type]
        return c.execute("SELECT relname FROM pg_class WHERE relname LIKE 'mneme%'").fetchall()


@pytest.mark.parametrize("encoding", ["SQL_ASCII", "LATIN1"])
def test_public_d02_d03_d05_a_non_utf8_database_is_refused_before_initialization(encoding: str) -> None:
    with database(encoding) as params:
        for options in ("", "-c client_encoding=UTF8"):
            with pytest.raises(MnemeValidationError) as info:
                PostgresHistoryStore.open(params | {"options": options}, lambda _: T)
            assert info.value.code == INCOMPATIBLE
        assert _mneme_tables(params) == []


# ------------------------------------------------------------------------------- transaction controls


def test_initialization_pins_its_settings_against_session_defaults() -> None:
    """CH11 transaction control: the initializer runs with `default_tablespace = ''` and
    `default_table_access_method = heap` whatever the session says (P44)."""
    probe = (
        "CREATE TABLE {ns}.probe_settings AS SELECT pg_catalog.current_setting('default_tablespace') AS ts,"
        " pg_catalog.current_setting('default_table_access_method') AS am"
    )
    with pgdb.fresh_backend() as params, pytest.MonkeyPatch.context() as patch:
        patch.setattr(postgres, "DDL", (*postgres.DDL, probe))
        options = "-c default_tablespace=mneme_ts -c default_table_access_method=heap"
        PostgresHistoryStore.open(with_options(params, options), lambda _: T).close()
        with psycopg.connect(**params) as c:  # type: ignore[arg-type]
            assert c.execute("SELECT ts, am FROM probe_settings").fetchall() == [("", "heap")]
        assert snapshot(params) == V1_CATALOG


def test_initialization_checks_its_postcondition_before_commit() -> None:
    """CE04-CP-002 transaction control: an initializer result outside the contract rolls back."""
    with pgdb.fresh_backend() as params, pytest.MonkeyPatch.context() as patch:
        patch.setattr(
            postgres, "DDL", (*postgres.DDL, "CREATE INDEX probe_idx ON {ns}.mneme_record (profile)")
        )
        with pytest.raises(MnemeValidationError) as info:
            PostgresHistoryStore.open(params, lambda _: T)
        assert info.value.code == INCOMPATIBLE
        assert snapshot(params).relations == ()


def test_x09_x10_open_holds_the_genuine_advisory_lock_and_never_calls_a_shadow() -> None:
    with initialized() as params:
        ns = schema_of(params)
        execute(
            params,
            [
                "CREATE TABLE probe_marker (calls integer)",
                "CREATE FUNCTION {ns}.pg_advisory_xact_lock(bigint) RETURNS void LANGUAGE plpgsql AS"
                " $$BEGIN INSERT INTO {ns}.probe_marker VALUES (1); END$$",
            ],
        )
        shadowed = with_options(params, f"-c search_path={ns},pg_catalog")
        opened: list[Any] = []
        high, low = divmod(SCHEMA_OPEN_LOCK_KEY % 2**64, 2**32)
        with psycopg.connect(**params) as gate:  # type: ignore[arg-type]
            # The gate: open's version read waits on this lock, after open has taken its own.
            gate.execute("LOCK TABLE mneme_schema IN ACCESS EXCLUSIVE MODE")
            opener = threading.Thread(
                target=lambda: opened.append(PostgresHistoryStore.open(shadowed, lambda _: T))
            )
            opener.start()
            with psycopg.connect(autocommit=True, **params) as observer:  # type: ignore[arg-type]
                deadline = time.monotonic() + 60
                held: list[Any] = []
                while not held:
                    assert time.monotonic() < deadline, "open never reached the gate"
                    held = observer.execute(
                        "SELECT a.pid, a.classid::bigint, a.objid::bigint, a.objsubid, a.granted"
                        " FROM pg_locks a JOIN pg_locks w ON w.pid = a.pid AND NOT w.granted"
                        " WHERE a.locktype = 'advisory' AND a.pid <> pg_backend_pid()"
                    ).fetchall()
                    time.sleep(0.1)
                assert len(held) == 1
                pid, classid, objid, objsubid, granted = held[0]
                assert (classid, objid, objsubid, granted) == (high, low, 1, True)
                gate.rollback()
                opener.join(60)
                assert len(opened) == 1
                assert observer.execute(
                    "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND pid = %s", (pid,)
                ).fetchone() == (0,)
        with opened[0]:
            pass
        with psycopg.connect(**params) as c:  # type: ignore[arg-type]
            assert c.execute("SELECT count(*) FROM probe_marker").fetchone() == (0,)
        protocol_n(shadowed)


# ------------------------------------------------------------------------------- CH12 direct engine control

_POISON_RELATIONS = (
    "pg_class",
    "pg_namespace",
    "pg_attribute",
    "pg_attrdef",
    "pg_constraint",
    "pg_index",
    "pg_inherits",
    "pg_trigger",
    "pg_rewrite",
    "pg_policy",
    "pg_am",
    "pg_opclass",
    "pg_collation",
    "pg_type",
    "pg_proc",
    "pg_database",
    "mneme_schema",
    "mneme_store",
    "mneme_record",
    "mneme_operation",
)
_POISON_TYPES = ("text", "int8", "int4", "int2", "oid", "bool", "bytea")


def _poison(connection: psycopg.Connection[Any]) -> None:
    """Temporary objects that every unqualified relation or type name would bind to first."""
    for name in _POISON_RELATIONS:
        connection.execute(f"CREATE TEMP VIEW {name} AS SELECT 1 AS poisoned")
    for name in _POISON_TYPES:
        connection.execute(f"CREATE DOMAIN pg_temp.{name} AS pg_catalog.{name} CHECK (false)")


def test_ch12_the_stores_sql_texts_bind_genuine_objects_in_a_poisoned_session() -> None:
    with initialized() as params, pgdb.fresh_backend() as fresh:
        ns, ns2 = schema_of(params), schema_of(fresh)
        clean = snapshot(params)
        with psycopg.connect(autocommit=True, **params) as c:  # type: ignore[arg-type]
            environment = c.execute(ENVIRONMENT_SQL).fetchone()
            namespace = c.execute(NAMESPACE_SQL).fetchone()
            assert namespace is not None
            _poison(c)
            # Baseline: the poison is effective for unqualified names.
            with pytest.raises(errors.CheckViolation):
                c.execute("SELECT 'x'::text")
            assert c.execute(
                "SELECT to_regclass('pg_class') <> 'pg_catalog.pg_class'::regclass"
            ).fetchone() == (True,)
            # Open's texts.
            c.execute("BEGIN")
            c.execute(postgres.OPEN_LOCK, (SCHEMA_OPEN_LOCK_KEY,))
            c.execute("COMMIT")
            assert c.execute(ENVIRONMENT_SQL).fetchone() == environment
            assert c.execute(NAMESPACE_SQL).fetchone() == namespace
            assert fetch(c, namespace[0]) == clean == V1_CATALOG
            assert set(QUERIES) == {f.name for f in dataclasses.fields(Catalog)}
            # The initializer's texts, into a second namespace.
            for statement in postgres.DDL:
                c.execute(postgres.bind(statement, ns2))
            c.execute(postgres.bind(postgres._HEADER, ns2), (1,))
            other = c.execute(
                "SELECT oid::pg_catalog.text FROM pg_catalog.pg_namespace"
                " WHERE nspname OPERATOR(pg_catalog.=) %s",
                (ns2,),
            ).fetchone()
            assert other is not None and fetch(c, other[0]) == V1_CATALOG
            assert c.execute(postgres.bind(postgres._VERSION, ns)).fetchall() == [(1,)]
            # The operations' texts.
            run = {name: postgres.bind(text, ns) for name, text in postgres.STATEMENTS.items()}
            assert c.execute(run["create"], ("s",)).fetchall() == [("s",)]
            assert c.execute(run["create"], ("s",)).fetchall() == []
            assert c.execute(run["lock"], ("s",)).fetchall() == [(0,)]
            assert c.execute(run["prior"], ("s", "app", b"one")).fetchall() == []
            c.execute(run["record"], ("s", 1, "r1", "learning", "attempt", 1, b"{}"))
            c.execute(run["record"], ("s", 2, "r2", "learning", "attempt", 1, b"[]"))
            assert c.execute(run["taken"], ("s", ["r0", "r2"])).fetchall() == [("r2",)]
            c.execute(run["operation"], ("s", "app", b"one", b"m", 1, 2, T))
            c.execute(run["advance"], (2, "s"))
            assert c.execute(run["prior"], ("s", "app", b"one")).fetchall() == [(b"m", 1, 2, T)]
            assert c.execute(run["range"], ("s", 1, 2)).fetchall() == [(b"{}",), (b"[]",)]
            assert c.execute(run["page"], ("s", 1, 5)).fetchall() == [(2, b"[]")]
            assert c.execute(run["tail"], ("s",)).fetchall() == [(2,)]


def test_the_handle_session_is_private_and_holds_no_temporary_objects() -> None:
    with initialized() as params:
        with psycopg.connect(**params) as connection, pytest.raises(TypeError):  # type: ignore[arg-type]
            PostgresHistoryStore.open(connection, lambda _: T)  # type: ignore[arg-type]
        with PostgresHistoryStore.open(params, lambda _: T) as s:
            s.create_store("s")
            s.commit_history("s", WRITER, op("one"), [draft("r1")])
            s.read_history("s", {"store_id": "s", "position": 0}, 5)
            assert s._connection.execute(
                "SELECT pg_catalog.pg_my_temp_schema()::pg_catalog.int8"
            ).fetchone() == (0,)


# ------------------------------------------------------------------------------- predicate evidence


def _vary(
    part: str, change: Callable[[tuple[Any, ...]], tuple[Any, ...] | None], where: Callable[[Any], bool]
) -> Catalog:
    """V1_CATALOG with one property changed in the rows of `part` that `where` selects."""
    rows = getattr(V1_CATALOG, part)
    assert any(where(row) for row in rows)
    changed = tuple(r for r in ((change(row) if where(row) else row) for row in rows) if r is not None)
    return dataclasses.replace(V1_CATALOG, **{part: changed})


def _set(index: int, value: Any) -> Callable[[tuple[Any, ...]], tuple[Any, ...]]:
    return lambda row: (*row[:index], value, *row[index + 1 :])


PREDICATES: dict[str, tuple[str, Catalog]] = {
    # row: (the part that must differ, the synthesized catalog)
    "R03 unlogged mneme_store": (
        "relations",
        _vary("relations", _set(2, "u"), lambda r: r[0] == "mneme_store"),
    ),
    "R30 foreign table": ("relations", _vary("relations", _set(1, "f"), lambda r: r[0] == "mneme_record")),
    "CH03 non-heap access method": (
        "relations",
        _vary("relations", _set(3, "probe_am"), lambda r: r[0] == "mneme_record"),
    ),
    "CH11 non-default tablespace": (
        "relations",
        _vary("relations", _set(5, 1663), lambda r: r[0] == "mneme_record"),
    ),
    "R32 disabled internal RI trigger": (
        "triggers",
        _vary("triggers", _set(2, "D"), lambda r: r[4] == "RI_FKey_check_ins"),
    ),
    "R32 RI trigger with another function": (
        "triggers",
        _vary("triggers", _set(3, "public"), lambda r: r[4] == "RI_FKey_noaction_del"),
    ),
    "R32 RI trigger unlinked from its constraint": (
        "triggers",
        _vary("triggers", _set(6, None), lambda r: r[4] == "RI_FKey_check_upd" and r[0] == "mneme_record"),
    ),
    "R32 a missing RI trigger": (
        "triggers",
        _vary("triggers", lambda r: None, lambda r: r[4] == "RI_FKey_noaction_upd"),
    ),
    "I15 backing-index collation": (
        "indexes",
        _vary(
            "indexes",
            _set(14, ("pg_catalog.default", "public.probe_loose", ".")),
            lambda r: r[0] == "mneme_operation",
        ),
    ),
    "I16 backing-index operator class": (
        "indexes",
        _vary(
            "indexes",
            _set(13, ("pg_catalog.text_ops", "pg_catalog.text_pattern_ops", "pg_catalog.bytea_ops")),
            lambda r: r[0] == "mneme_operation",
        ),
    ),
    "I16 invalid backing index": (
        "indexes",
        _vary("indexes", _set(6, False), lambda r: r[0] == "mneme_store"),
    ),
    "I17 missing backing index": (
        "indexes",
        _vary("indexes", lambda r: None, lambda r: r[0] == "mneme_operation"),
    ),
}


@pytest.mark.parametrize("row", sorted(PREDICATES))
def test_predicate_a_non_constructible_difference_is_refused(row: str) -> None:
    part, synthesized = PREDICATES[row]
    assert mismatch(synthesized, V1_CATALOG) == part


def test_predicate_the_pinned_catalog_is_accepted() -> None:
    assert mismatch(V1_CATALOG, V1_CATALOG) is None


@pytest.mark.parametrize(
    ("version", "encoding", "collate", "ctype", "accepted"),
    [
        (140012, "UTF8", "C", "C", True),
        (140000, "UTF8", "POSIX", "C", True),  # D01
        (149999, "UTF8", "C", "POSIX", True),  # D04
        (130016, "UTF8", "C", "C", False),  # CH10
        (150008, "UTF8", "C", "C", False),  # CH10
        (140012, "SQL_ASCII", "C", "C", False),
        (140012, "LATIN1", "C", "C", False),
        (140012, "UTF8", "en_US.UTF-8", "C", False),
        (140012, "UTF8", "C", "English_World.1252", False),
    ],
)
def test_predicate_the_environment(
    version: int, encoding: str, collate: str, ctype: str, accepted: bool
) -> None:
    assert (environment_mismatch(version, encoding, collate, ctype) is None) is accepted
