"""The Layer 04 canonical history store on the PostgreSQL reference backend (WP-04; Owner G-077..G-087;
`spec/persistence.md`, "Layer 04 canonical history store contract").

Validation (commit steps 1-8, read steps 1-4) happens before any database work and has no effect.
Commit steps 9-13 run in one transaction that first locks the store row: that lock is the store's
single serialization boundary (G-084). Reads use one REPEATABLE READ READ ONLY snapshot (WP04-CR-004).
Opening runs in an explicit READ COMMITTED transaction whose first statement takes the shared
schema-open advisory lock (WP04-CR-007).
"""

from __future__ import annotations

import threading
from collections.abc import Callable, Iterator, Mapping
from contextlib import contextmanager
from typing import Any

import psycopg
from psycopg import IsolationLevel
from psycopg.conninfo import make_conninfo

from ..core.errors import MnemeValidationError, describe
from ..core.history import _admit, _boundary_rule, _reader_rule, _UnknownRequired
from ..core.identity import _operation_rule, _stable_id_rule
from ..core.time import _timestamp_rule
from ..core.values import SAFE_INTEGER_MAX, Value, canonical_bytes, decode_json, validate_arguments

SCHEMA_VERSION = 1
# First 8 bytes of SHA-256("mneme.schema.open.v1") as a big-endian signed 64-bit integer (CR-007).
SCHEMA_OPEN_LOCK_KEY = -4695452043516644990

_TABLES = ("mneme_operation", "mneme_record", "mneme_schema", "mneme_store")
# The catalog shape of schema version 1 created by `_DDL` (WP04-CR-008): per table, its columns in
# order as (name, type, NOT NULL, default) and its constraints as (kind, definition). Compared before
# any query relies on a column, so a malformed shape is `persistence.schema_incompatible`, never a
# leaked catalog error or a usable-looking handle. Constraint text is PostgreSQL's
# `pg_get_constraintdef` on the reference backend (G-081); if a server release rendered it
# differently, open fails visibly as incompatible rather than accepting an unverified shape.
_V1_COLUMNS = (
    ("mneme_operation", "store_id", "text", True, None),
    ("mneme_operation", "source_namespace", "text", True, None),
    ("mneme_operation", "source_operation_id", "bytea", True, None),
    ("mneme_operation", "mutation", "bytea", True, None),
    ("mneme_operation", "first_position", "bigint", True, None),
    ("mneme_operation", "last_position", "bigint", True, None),
    ("mneme_operation", "recorded_at", "text", True, None),
    ("mneme_record", "store_id", "text", True, None),
    ("mneme_record", "position", "bigint", True, None),
    ("mneme_record", "record_id", "text", True, None),
    ("mneme_record", "profile", "text", True, None),
    ("mneme_record", "record_type", "text", True, None),
    ("mneme_record", "schema_version", "bigint", True, None),
    ("mneme_record", "envelope", "bytea", True, None),
    ("mneme_schema", "singleton", "boolean", True, "true"),
    ("mneme_schema", "version", "integer", True, None),
    ("mneme_store", "store_id", "text", True, None),
    ("mneme_store", "tail", "bigint", True, "0"),
)
_V1_CONSTRAINTS = (
    ("mneme_operation", "f", "FOREIGN KEY (store_id) REFERENCES mneme_store(store_id)"),
    ("mneme_operation", "p", "PRIMARY KEY (store_id, source_namespace, source_operation_id)"),
    ("mneme_record", "c", 'CHECK ((("position" >= 1) AND ("position" <= \'9007199254740991\'::bigint)))'),
    ("mneme_record", "f", "FOREIGN KEY (store_id) REFERENCES mneme_store(store_id)"),
    ("mneme_record", "p", 'PRIMARY KEY (store_id, "position")'),
    ("mneme_record", "u", "UNIQUE (store_id, record_id)"),
    ("mneme_schema", "c", "CHECK (singleton)"),
    ("mneme_schema", "p", "PRIMARY KEY (singleton)"),
    ("mneme_store", "c", "CHECK (((tail >= 0) AND (tail <= '9007199254740991'::bigint)))"),
    ("mneme_store", "p", "PRIMARY KEY (store_id)"),
)
_COLUMNS_SQL = (
    "SELECT c.relname, a.attname, format_type(a.atttypid, a.atttypmod), a.attnotnull,"
    " pg_get_expr(d.adbin, d.adrelid) FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid"
    " LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum"
    " WHERE c.relnamespace = current_schema()::regnamespace AND c.relname LIKE 'mneme\\_%'"
    " AND c.relkind = 'r' AND a.attnum > 0 AND NOT a.attisdropped ORDER BY 1, a.attnum"
)
_CONSTRAINTS_SQL = (
    "SELECT c.relname, con.contype, pg_get_constraintdef(con.oid) FROM pg_constraint con"
    " JOIN pg_class c ON c.oid = con.conrelid"
    " WHERE c.relnamespace = current_schema()::regnamespace AND c.relname LIKE 'mneme\\_%'"
    " ORDER BY 1, 2, 3"
)
_DDL = (
    """CREATE TABLE mneme_schema (singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
                               version integer NOT NULL)""",
    """CREATE TABLE mneme_store (store_id text PRIMARY KEY,
                              tail bigint NOT NULL DEFAULT 0
                                   CHECK (tail BETWEEN 0 AND 9007199254740991))""",
    """CREATE TABLE mneme_record (store_id text NOT NULL REFERENCES mneme_store,
                               position bigint NOT NULL
                                    CHECK (position BETWEEN 1 AND 9007199254740991),
                               record_id text NOT NULL,
                               profile text NOT NULL, record_type text NOT NULL,
                               schema_version bigint NOT NULL,
                               envelope bytea NOT NULL,
                               PRIMARY KEY (store_id, position),
                               UNIQUE (store_id, record_id))""",
    """CREATE TABLE mneme_operation (store_id text NOT NULL REFERENCES mneme_store,
                                  source_namespace text NOT NULL,
                                  source_operation_id bytea NOT NULL,
                                  mutation bytea NOT NULL,
                                  first_position bigint NOT NULL, last_position bigint NOT NULL,
                                  recorded_at text NOT NULL,
                                  PRIMARY KEY (store_id, source_namespace, source_operation_id))""",
)

Clock = Callable[[str], object]
"""`clock(store_id)` yields a HIST-004 timestamp; the store stamps its canonical form (G-082)."""


class ClockDefectError(RuntimeError):
    """The injected clock yielded a value HIST-004 rejects: a defect of the clock, not a store outcome."""


def _named(error: MnemeValidationError, argument: str) -> MnemeValidationError:
    return error.for_argument(argument)


def _store_id(value: Value) -> str:
    try:
        return _stable_id_rule(value)
    except MnemeValidationError as error:
        raise _named(error, "store_id") from None


def _limit(value: Value) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not 1 <= value <= SAFE_INTEGER_MAX:
        raise MnemeValidationError(
            "persistence.limit_invalid", f"limit {describe(value)} is not an integer 1..2^53-1", "limit"
        )
    return value


def _drafts(reader: tuple[str, frozenset[tuple[str, int]]], value: Value) -> list[dict[str, Value]]:
    """Commit steps 5-7: the batch shape, draft gates D1-D4 in array order, then in-batch repeats."""
    if not isinstance(value, list) or not value:
        raise MnemeValidationError("persistence.batch_invalid", "records is a non-empty array", "records")
    drafts: list[dict[str, Value]] = []
    for item in value:
        try:
            admission = _admit(reader, item, draft=True)
        except MnemeValidationError as error:
            raise _named(error, "records") from None
        except _UnknownRequired as stop:
            raise MnemeValidationError(
                "persistence.record_unrecognized", f"{stop}; not an acceptable write (G-083)", "records"
            ) from None
        if admission.outcome != "decode":
            raise MnemeValidationError(
                "persistence.record_unrecognized",
                f"the writer's reader outcome is {admission.outcome}, not an acceptable write (G-083)",
                "records",
            )
        assert admission.envelope is not None
        drafts.append(admission.envelope)
    seen: set[object] = set()
    for draft in drafts:
        if draft["record_id"] in seen:
            raise MnemeValidationError(
                "history.duplicate_record_id", f"record_id {describe(draft['record_id'])} repeats", "records"
            )
        seen.add(draft["record_id"])
    return drafts


def _receipt(
    store_id: str,
    namespace: str,
    operation_id: str,
    first: int,
    last: int,
    recorded_at: str,
    envelopes: list[Value],
) -> dict[str, Any]:
    return {
        "store_id": store_id,
        "operation": {"source_namespace": namespace, "source_operation_id": operation_id},
        "first_position": first,
        "last_position": last,
        "recorded_at": recorded_at,
        "records": envelopes,
    }


class PostgresHistoryStore:
    """One backend handle on one PostgreSQL database holding any number of stores (G-079)."""

    def __init__(self, connection: psycopg.Connection[Any], clock: Clock) -> None:
        self._connection = connection
        self._clock = clock
        # One connection serves the handle, so overlapping public calls on it are serialized over
        # their whole transaction lifecycle (isolation setup to commit or rollback); the store-row
        # lock then serializes across handles (G-084, WP04-CR-009).
        self._serial = threading.Lock()

    # ----------------------------------------------------------------------------- open (WP04-CR-007)

    @classmethod
    def open(cls, conninfo: str | Mapping[str, str], clock: Clock) -> PostgresHistoryStore:
        """Open (and on an empty database, initialize) the shared schema version 1."""
        info = conninfo if isinstance(conninfo, str) else make_conninfo("", **conninfo)
        connection = psycopg.connect(info, autocommit=True)
        try:
            connection.execute("SET synchronous_commit = on")
            connection.isolation_level = IsolationLevel.READ_COMMITTED  # explicit, never the default
            with connection.transaction():
                connection.execute("SELECT pg_advisory_xact_lock(%s)", (SCHEMA_OPEN_LOCK_KEY,))
                names = sorted(
                    row[0]
                    for row in connection.execute(
                        "SELECT tablename FROM pg_tables"
                        " WHERE schemaname = current_schema() AND tablename LIKE 'mneme\\_%'"
                    )
                )
                if not names:
                    for statement in _DDL:
                        connection.execute(statement)
                    connection.execute("INSERT INTO mneme_schema (version) VALUES (%s)", (SCHEMA_VERSION,))
                elif (
                    tuple(names) != _TABLES
                    or tuple(tuple(row) for row in connection.execute(_COLUMNS_SQL)) != _V1_COLUMNS
                    or tuple(tuple(row) for row in connection.execute(_CONSTRAINTS_SQL)) != _V1_CONSTRAINTS
                    or connection.execute("SELECT array_agg(version) FROM mneme_schema").fetchone()
                    != ([SCHEMA_VERSION],)
                ):
                    raise MnemeValidationError(
                        "persistence.schema_incompatible", f"mneme tables {names} are not schema version 1"
                    )
        except BaseException:
            connection.close()
            raise
        return cls(connection, clock)

    def close(self) -> None:
        self._connection.close()

    def __enter__(self) -> PostgresHistoryStore:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    @contextmanager
    def _transaction(self, level: IsolationLevel, read_only: bool = False) -> Iterator[None]:
        with self._serial:
            self._connection.isolation_level = level
            self._connection.read_only = read_only
            with self._connection.transaction():
                yield

    # ----------------------------------------------------------------------------- the four operations

    def create_store(self, store_id: object) -> None:
        """Create an empty store (tail 0); an existing ID is `persistence.store_exists`."""
        sid = _store_id(validate_arguments({"store_id": store_id})["store_id"])
        with self._transaction(IsolationLevel.READ_COMMITTED):
            created = self._connection.execute(
                "INSERT INTO mneme_store (store_id) VALUES (%s) ON CONFLICT DO NOTHING RETURNING store_id",
                (sid,),
            ).fetchone()
        if created is None:
            raise MnemeValidationError("persistence.store_exists", f"store {sid!r} exists", "store_id")

    def store_tail(self, store_id: object) -> dict[str, Any]:
        """The HistoryBoundary of the last committed record (position 0 for an empty store)."""
        sid = _store_id(validate_arguments({"store_id": store_id})["store_id"])
        with self._transaction(IsolationLevel.READ_COMMITTED, read_only=True):
            row = self._connection.execute(
                "SELECT tail FROM mneme_store WHERE store_id = %s", (sid,)
            ).fetchone()
        if row is None:
            raise MnemeValidationError("persistence.store_unknown", f"no store {sid!r}", "store_id")
        return {"store_id": sid, "position": row[0]}

    def commit_history(
        self, store_id: object, writer: object, operation: object, records: object
    ) -> dict[str, Any]:
        """Append one batch atomically, or return the original receipt of an identical retry."""
        values = validate_arguments(
            {"store_id": store_id, "writer": writer, "operation": operation, "records": records}
        )
        sid = _store_id(values["store_id"])
        try:
            reader = _reader_rule(values["writer"])
        except MnemeValidationError as error:
            raise _named(error, "writer") from None
        try:
            identity = _operation_rule(values["operation"])
        except MnemeValidationError as error:
            raise _named(error, "operation") from None
        drafts = _drafts(reader, values["records"])
        namespace, operation_id = identity.source_namespace, identity.source_operation_id
        mutation = canonical_bytes(drafts)
        key = operation_id.encode("utf-8")  # exact UTF-8 bytes (G-033, WP04-CR-001)
        db = self._connection
        with self._transaction(IsolationLevel.READ_COMMITTED):
            # Step 8, then the serialization boundary for steps 9-13 (G-084).
            row = db.execute("SELECT tail FROM mneme_store WHERE store_id = %s FOR UPDATE", (sid,)).fetchone()
            if row is None:
                raise MnemeValidationError("persistence.store_unknown", f"no store {sid!r}", "store_id")
            tail: int = row[0]
            # Step 9: idempotency (G-082, G-086).
            prior = db.execute(
                "SELECT mutation, first_position, last_position, recorded_at FROM mneme_operation"
                " WHERE store_id = %s AND source_namespace = %s AND source_operation_id = %s",
                (sid, namespace, key),
            ).fetchone()
            if prior is not None:
                if bytes(prior[0]) != mutation:
                    raise MnemeValidationError(
                        "persistence.operation_conflict",
                        "a different mutation under a committed operation identity (G-013)",
                        "operation",
                    )
                committed = [
                    decode_json(bytes(r[0]))
                    for r in db.execute(
                        "SELECT envelope FROM mneme_record WHERE store_id = %s"
                        " AND position BETWEEN %s AND %s ORDER BY position",
                        (sid, prior[1], prior[2]),
                    )
                ]
                return _receipt(sid, namespace, operation_id, prior[1], prior[2], prior[3], committed)
            # Step 10: record IDs are unique across the store (G-083).
            taken = db.execute(
                "SELECT record_id FROM mneme_record WHERE store_id = %s AND record_id = ANY(%s)"
                " ORDER BY position LIMIT 1",
                (sid, [draft["record_id"] for draft in drafts]),
            ).fetchone()
            if taken is not None:
                raise MnemeValidationError(
                    "history.duplicate_record_id", f"record_id {taken[0]!r} is already committed", "records"
                )
            # Step 11: capacity (G-086).
            if len(drafts) > SAFE_INTEGER_MAX - tail:
                raise MnemeValidationError(
                    "persistence.store_full", f"{len(drafts)} records do not fit after position {tail}"
                )
            # Step 12: one clock sample (G-085); a later abort persists nothing.
            sampled = self._clock(sid)
            try:
                recorded_at = _timestamp_rule(validate_arguments({"clock": sampled})["clock"])
            except MnemeValidationError as error:
                raise ClockDefectError(f"the clock yielded an invalid instant: {error}") from None
            # Step 13: gapless consecutive positions in draft order (G-080).
            envelopes = [{**draft, "recorded_at": recorded_at} for draft in drafts]
            first = tail + 1
            with db.cursor() as cursor:
                cursor.executemany(
                    "INSERT INTO mneme_record (store_id, position, record_id, profile, record_type,"
                    " schema_version, envelope) VALUES (%s, %s, %s, %s, %s, %s, %s)",
                    [
                        (
                            sid,
                            first + i,
                            e["record_id"],
                            e["profile"],
                            e["record_type"],
                            e["schema_version"],
                            canonical_bytes(e),
                        )
                        for i, e in enumerate(envelopes)
                    ],
                )
            last = tail + len(envelopes)
            db.execute(
                "INSERT INTO mneme_operation (store_id, source_namespace, source_operation_id, mutation,"
                " first_position, last_position, recorded_at) VALUES (%s, %s, %s, %s, %s, %s, %s)",
                (sid, namespace, key, mutation, first, last, recorded_at),
            )
            db.execute("UPDATE mneme_store SET tail = %s WHERE store_id = %s", (last, sid))
        # The transaction has committed durably (synchronous_commit = on): the durability barrier.
        stored: list[Value] = [decode_json(canonical_bytes(e)) for e in envelopes]
        return _receipt(sid, namespace, operation_id, first, last, recorded_at, stored)

    def read_history(self, store_id: object, since: object, limit: object) -> dict[str, Any]:
        """One record-counted page after `since`, from one consistent snapshot (WP04-CR-004)."""
        values = validate_arguments({"store_id": store_id, "since": since, "limit": limit})
        sid = _store_id(values["store_id"])
        try:
            since_store, since_position = _boundary_rule(values["since"])
        except MnemeValidationError as error:
            raise _named(error, "since") from None
        count = _limit(values["limit"])
        db = self._connection
        with self._transaction(IsolationLevel.REPEATABLE_READ, read_only=True):
            row = db.execute("SELECT tail FROM mneme_store WHERE store_id = %s", (sid,)).fetchone()
            if row is None:
                raise MnemeValidationError("persistence.store_unknown", f"no store {sid!r}", "store_id")
            tail: int = row[0]
            if since_store != sid:
                raise MnemeValidationError(
                    "history.boundary_incomparable", "a boundary of another store (G-051)", "since"
                )
            if since_position > tail:
                raise MnemeValidationError(
                    "history.boundary_invalid",
                    f"position {since_position} is beyond the tail {tail}",
                    "since",
                )
            entries: list[Value] = [
                {"position": position, "record": decode_json(bytes(envelope))}
                for position, envelope in db.execute(
                    "SELECT position, envelope FROM mneme_record WHERE store_id = %s AND position > %s"
                    " ORDER BY position LIMIT %s",
                    (sid, since_position, count),
                )
            ]
        through = entries[-1]["position"] if entries else since_position  # type: ignore[index,call-overload]
        return {
            "entries": entries,
            "through": {"store_id": sid, "position": through},
            "tail": {"store_id": sid, "position": tail},
        }
