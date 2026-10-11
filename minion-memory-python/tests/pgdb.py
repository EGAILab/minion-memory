"""PostgreSQL test backends for the Layer 04 reference backend (G-079, G-081).

Connection parameters come from the env file named by `MNEME_PG_ENV`, else the project root's
`.tmp/work/pg.env` (the non-superuser role `mneme_dev`; never committed). A missing configuration
fails loudly: the reference backend is required evidence, never a silent skip.

One throwaway database per test session is created from template0 with UTF-8 and the C collation in
the tablespace `MNEME_PG_TABLESPACE` (on E:, OWN-001), and dropped at exit. Each `fresh_backend()` is
a new empty schema in it, selected through `search_path`: the open rule inspects `current_schema()`,
so an empty schema is an empty backend. Creating a database per backend costs seconds on Windows.
"""

from __future__ import annotations

import atexit
import os
import uuid
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path

import psycopg
from psycopg import sql

_MAPPING = {"PGHOST": "host", "PGPORT": "port", "PGUSER": "user", "PGPASSWORD": "password"}
_session: dict[str, str] | None = None


def _env_file() -> Path:
    named = os.environ.get("MNEME_PG_ENV")
    if named:
        return Path(named)
    for parent in Path(__file__).resolve().parents:  # the project root holding .tmp (OWN-001)
        candidate = parent / ".tmp" / "work" / "pg.env"
        if candidate.is_file():
            return candidate
    raise RuntimeError("Layer 04 tests need MNEME_PG_ENV or a project .tmp/work/pg.env (G-081)")


def settings() -> dict[str, str]:
    values: dict[str, str] = {}
    for line in _env_file().read_text(encoding="utf-8").splitlines():
        if "=" in line and not line.lstrip().startswith("#"):
            name, value = line.split("=", 1)
            values[name.strip()] = value.strip()
    return values


def _params(values: dict[str, str], dbname: str) -> dict[str, str]:
    return {_MAPPING[k]: v for k, v in values.items() if k in _MAPPING} | {"dbname": dbname}


@contextmanager
def fresh_database() -> Iterator[dict[str, str]]:
    """Yield connection parameters for a new empty database; drop it afterwards."""
    values = settings()
    name = f"mneme_test_{uuid.uuid4().hex[:16]}"
    admin = _params(values, values.get("PGDATABASE", "postgres"))
    with psycopg.connect(autocommit=True, **admin) as connection:  # type: ignore[arg-type]
        connection.execute(
            sql.SQL(
                "CREATE DATABASE {} TEMPLATE template0 ENCODING 'UTF8' LC_COLLATE 'C' LC_CTYPE 'C'"
                " TABLESPACE {}"
            ).format(sql.Identifier(name), sql.Identifier(values.get("MNEME_PG_TABLESPACE", "mneme_ts")))
        )
    try:
        yield _params(values, name)
    finally:
        with psycopg.connect(autocommit=True, **admin) as connection:  # type: ignore[arg-type]
            connection.execute(
                sql.SQL("DROP DATABASE IF EXISTS {} WITH (FORCE)").format(sql.Identifier(name))
            )


def session_database() -> dict[str, str]:
    """The test session's shared database, created on first use and dropped at interpreter exit."""
    global _session
    if _session is None:
        context = fresh_database()
        _session = context.__enter__()
        atexit.register(context.__exit__, None, None, None)
    return _session


@contextmanager
def fresh_backend() -> Iterator[dict[str, str]]:
    """Yield connection parameters whose `search_path` is a new empty schema; drop it afterwards."""
    database = session_database()
    schema = f"b_{uuid.uuid4().hex[:16]}"
    with psycopg.connect(autocommit=True, **database) as connection:  # type: ignore[arg-type]
        connection.execute(sql.SQL("CREATE SCHEMA {}").format(sql.Identifier(schema)))
    try:
        yield database | {"options": f"-c search_path={schema}"}
    finally:
        with psycopg.connect(autocommit=True, **database) as connection:  # type: ignore[arg-type]
            connection.execute(sql.SQL("DROP SCHEMA {} CASCADE").format(sql.Identifier(schema)))


_shared: dict[str, str] | None = None


@contextmanager
def empty_backend() -> Iterator[dict[str, str]]:
    """Yield parameters for an initialized schema-v1 backend holding no stores.

    Creating schemas and tables creates files in the tablespace, which is slow and erratic on this
    host (seconds each). Tests that do not exercise opening share one initialized schema, emptied
    with DELETE (no new files) before each use. Open/initialize tests use `fresh_backend()`.
    """
    global _shared
    from minion_memory.persistence import PostgresHistoryStore

    if _shared is None:
        context = fresh_backend()
        _shared = context.__enter__()
        atexit.register(context.__exit__, None, None, None)
        PostgresHistoryStore.open(_shared, lambda _: "").close()  # initialize schema version 1
    with psycopg.connect(autocommit=True, **_shared) as connection, connection.transaction():  # type: ignore[arg-type]
        for table in ("mneme_record", "mneme_operation", "mneme_store"):
            connection.execute(sql.SQL("DELETE FROM {}").format(sql.Identifier(table)))
    yield _shared
