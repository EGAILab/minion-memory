"""Layer 04 canonical history store on the PostgreSQL reference backend (WP-04): the implementation
tests named [PT] and [CT] in `assurance/layers/04-feasibility-matrix.md`. Expectations come from
`spec/persistence.md` (contract agreed at docs d857fcc2), never from this implementation."""

from __future__ import annotations

import contextlib
import random
import threading
from collections.abc import Callable, Iterator
from contextlib import contextmanager
from typing import Any

import pgdb
import psycopg
import pytest

from minion_memory.core import JsonText, MnemeValidationError
from minion_memory.persistence import SCHEMA_OPEN_LOCK_KEY, ClockDefectError, PostgresHistoryStore
from minion_memory.persistence._v1_catalog import V1_CATALOG
from minion_memory.persistence.catalog import NAMESPACE_SQL, Catalog, fetch
from minion_memory.persistence.postgres import DDL, bind


def catalog(params: dict[str, str]) -> Catalog:
    """The current schema's v1 catalog state (CE-04-01 structured fetch)."""
    with psycopg.connect(autocommit=True, **params) as c:  # type: ignore[arg-type]
        row = c.execute(NAMESPACE_SQL).fetchone()
        assert row is not None
        return fetch(c, row[0])


MAX = 2**53 - 1
T = "2026-10-11T08:00:00.000000Z"
WRITER = {"profile": "learning", "recognized": [{"record_type": "attempt", "schema_version": 1}]}


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


class CountingClock:
    """Yields distinct canonical instants and counts every call (G-085 sampling evidence)."""

    def __init__(self, hook: Callable[[], None] | None = None) -> None:
        self.calls = 0
        self.hook = hook
        self._lock = threading.Lock()

    def __call__(self, store_id: str) -> str:
        with self._lock:
            self.calls += 1
            n = self.calls
        if self.hook is not None:
            self.hook()
        return f"2026-10-11T08:00:00.{n:06d}Z"


@contextmanager
def store(
    clock: Callable[[str], object] | None = None,
) -> Iterator[tuple[PostgresHistoryStore, dict[str, str]]]:
    with pgdb.empty_backend() as params:
        handle = PostgresHistoryStore.open(params, clock or CountingClock())
        try:
            handle.create_store("s")
            yield handle, params
        finally:
            handle.close()


def counts(params: dict[str, str]) -> tuple[int, int, int]:
    with psycopg.connect(**params) as c:  # type: ignore[arg-type]
        row = c.execute(
            "SELECT (SELECT coalesce(max(tail), 0) FROM mneme_store), (SELECT count(*) FROM mneme_record),"
            " (SELECT count(*) FROM mneme_operation)"
        ).fetchone()
    assert row is not None
    return row[0], row[1], row[2]


def code(fn: Callable[[], object]) -> str:
    with pytest.raises(MnemeValidationError) as info:
        fn()
    return info.value.code


# ------------------------------------------------------------------------------- paging (PERS-007)


def test_pages_from_zero_to_the_tail_yield_every_record_once_in_order() -> None:
    rng = random.Random(4)
    with store() as (s, _):
        committed: list[str] = []
        for k in range(12):
            batch = [draft(f"o-{k}-{j}") for j in range(rng.randint(1, 4))]
            s.commit_history("s", WRITER, op(f"p-{k}"), batch)
            committed += [str(d["record_id"]) for d in batch]
        full = s.read_history("s", {"store_id": "s", "position": 0}, MAX)
        assert [e["record"]["record_id"] for e in full["entries"]] == committed
        for _ in range(20):
            limit, since, seen = rng.randint(1, 5), 0, []
            while True:
                page = s.read_history("s", {"store_id": "s", "position": since}, limit)
                seen += page["entries"]
                assert page["through"]["position"] <= page["tail"]["position"]
                if page["through"]["position"] == page["tail"]["position"]:
                    break
                since = page["through"]["position"]
            assert seen == full["entries"]


# ------------------------------------------------------------------------------- capacity (G-086)


def test_capacity_rejects_only_new_batches_that_do_not_fit_and_never_samples() -> None:
    clock = CountingClock()
    with store(clock) as (s, params):
        receipt = s.commit_history("s", WRITER, op("a"), [draft("o-1")])
        with psycopg.connect(**params) as c:  # type: ignore[arg-type]
            c.execute("UPDATE mneme_store SET tail = %s WHERE store_id = 's'", (MAX - 1,))  # synthetic tail
        calls = clock.calls
        assert code(lambda: s.commit_history("s", WRITER, op("b"), [draft("o-2"), draft("o-3")])) == (
            "persistence.store_full"
        )
        assert clock.calls == calls
        last = s.commit_history("s", WRITER, op("c"), [draft("o-4")])
        assert (last["first_position"], last["last_position"]) == (MAX, MAX)
        calls = clock.calls
        assert (
            code(lambda: s.commit_history("s", WRITER, op("d"), [draft("o-5")])) == "persistence.store_full"
        )
        assert s.commit_history("s", WRITER, op("a"), [draft("o-1")]) == receipt  # retry at a full store
        assert code(lambda: s.commit_history("s", WRITER, op("a"), [draft("o-9")])) == (
            "persistence.operation_conflict"
        )
        assert code(lambda: s.commit_history("s", WRITER, op("e"), [draft("o-1")])) == (
            "history.duplicate_record_id"  # step 10 precedes the capacity check
        )
        assert clock.calls == calls
        with pytest.raises(psycopg.errors.CheckViolation), psycopg.connect(**params) as c:  # type: ignore[arg-type]
            c.execute("UPDATE mneme_store SET tail = %s WHERE store_id = 's'", (MAX + 1,))


# ------------------------------------------------------------------------------- clock (G-082, G-085)


def test_only_a_new_commit_that_reaches_step_12_samples_the_clock() -> None:
    clock = CountingClock()
    with store(clock) as (s, _):
        s.commit_history("s", WRITER, op("a"), [draft("o-1")])
        assert clock.calls == 1
        attempts: list[Callable[[], object]] = [
            lambda: s.commit_history("s", WRITER, op("a"), [draft("o-1")]),  # identical retry
            lambda: s.commit_history("s", WRITER, op("a"), [draft("o-2")]),  # conflict
            lambda: s.commit_history("s", WRITER, op("b"), [draft("o-1")]),  # store duplicate
            lambda: s.commit_history("s", WRITER, op("b"), [draft("BAD")]),  # draft gate
            lambda: s.commit_history("t", WRITER, op("b"), [draft("o-2")]),  # unknown store
        ]
        for attempt in attempts:
            with contextlib.suppress(MnemeValidationError):
                attempt()
        assert clock.calls == 1


def test_a_clock_defect_is_not_a_store_outcome_and_persists_nothing() -> None:
    with store(lambda _: "not a timestamp") as (s, params):
        with pytest.raises(ClockDefectError):
            s.commit_history("s", WRITER, op("a"), [draft("o-1")])
        assert counts(params) == (0, 0, 0)


def test_an_attempt_aborted_after_sampling_persists_nothing_and_a_retry_takes_a_new_instant() -> None:
    def boom() -> None:
        raise RuntimeError("abort after the sample")

    clock = CountingClock(boom)
    with store(clock) as (s, params):
        with pytest.raises(RuntimeError):
            s.commit_history("s", WRITER, op("a"), [draft("o-1")])
        assert clock.calls == 1 and counts(params) == (0, 0, 0)
        clock.hook = None
        receipt = s.commit_history("s", WRITER, op("a"), [draft("o-1")])
        assert receipt["first_position"] == 1 and receipt["recorded_at"] == "2026-10-11T08:00:00.000002Z"


def test_a_connection_killed_after_sampling_persists_nothing() -> None:
    with pgdb.empty_backend() as params:
        victim: dict[str, int] = {}

        def kill() -> None:
            with psycopg.connect(autocommit=True, **params) as c:  # type: ignore[arg-type]
                c.execute("SELECT pg_terminate_backend(%s)", (victim["pid"],))

        s = PostgresHistoryStore.open(params, CountingClock(kill))
        victim["pid"] = s._connection.info.backend_pid
        try:
            s.create_store("s")
            with pytest.raises(psycopg.OperationalError):
                s.commit_history("s", WRITER, op("a"), [draft("o-1")])
        finally:
            s.close()
        assert counts(params) == (0, 0, 0)
        with PostgresHistoryStore.open(params, CountingClock()) as again:
            assert again.commit_history("s", WRITER, op("a"), [draft("o-1")])["first_position"] == 1


# ------------------------------------------------------------------------------- representation


def test_nul_in_payloads_and_operation_ids_round_trips_exactly() -> None:
    with store() as (s, params):
        record = draft("o-1", payload={"text": "a\u0000b", "\u0000": 1})
        receipt = s.commit_history("s", WRITER, op("\u0000"), [record])
        page = s.read_history("s", {"store_id": "s", "position": 0}, 1)
        assert page["entries"][0]["record"]["payload"] == {"text": "a\u0000b", "\u0000": 1}
        assert s.commit_history("s", WRITER, op("\u0000"), [record]) == receipt
        with psycopg.connect(**params) as c:  # type: ignore[arg-type]
            assert c.execute("SELECT source_operation_id FROM mneme_operation").fetchone() == (b"\x00",)


def test_receipts_rebuild_identically_from_another_handle() -> None:
    with store() as (s, params):
        receipt = s.commit_history(
            "s", WRITER, op("a"), [draft("o-1", occurred_at="2026-10-11T10:00:00+02:00")]
        )
        with PostgresHistoryStore.open(params, CountingClock()) as other:
            assert other.commit_history("s", WRITER, op("a"), [draft("o-1", occurred_at=T)]) == receipt


def test_commits_run_with_synchronous_commit_on() -> None:
    with store() as (s, _):
        assert s._connection.execute("SHOW synchronous_commit").fetchone() == ("on",)


# ------------------------------------------------------------------------------- open (Q3, CR-007)


def test_open_initializes_an_empty_backend_once() -> None:
    with pgdb.fresh_backend() as params:
        PostgresHistoryStore.open(params, CountingClock()).close()
        PostgresHistoryStore.open(params, CountingClock()).close()
        with psycopg.connect(**params) as c:  # type: ignore[arg-type]
            assert c.execute("SELECT singleton, version FROM mneme_schema").fetchall() == [(True, 1)]


@pytest.mark.parametrize(
    "damage",
    [
        "UPDATE mneme_schema SET version = 2",
        "DELETE FROM mneme_schema",
        "CREATE TABLE mneme_extra (x int)",
        "DROP TABLE mneme_operation",
    ],
)
def test_open_rejects_an_incompatible_schema_and_changes_nothing(damage: str) -> None:
    with pgdb.fresh_backend() as params:
        PostgresHistoryStore.open(params, CountingClock()).close()
        with psycopg.connect(**params) as c:  # type: ignore[arg-type]
            c.execute(damage)
            c.commit()
            before = c.execute(
                "SELECT tablename FROM pg_tables WHERE schemaname = current_schema() ORDER BY 1"
            ).fetchall()
        assert (
            code(lambda: PostgresHistoryStore.open(params, CountingClock()))
            == "persistence.schema_incompatible"
        )
        with psycopg.connect(**params) as c:  # type: ignore[arg-type]
            after = c.execute(
                "SELECT tablename FROM pg_tables WHERE schemaname = current_schema() ORDER BY 1"
            ).fetchall()
        assert before == after


def test_concurrent_first_opens_initialize_once_even_with_a_repeatable_read_default() -> None:
    with pgdb.fresh_backend() as params:
        rr = params | {"options": params["options"] + " -c default_transaction_isolation=repeatable\\ read"}
        errors: list[BaseException] = []
        start = threading.Barrier(6)

        def opener() -> None:
            try:
                start.wait()
                PostgresHistoryStore.open(rr, CountingClock()).close()
            except BaseException as error:
                errors.append(error)

        threads = [threading.Thread(target=opener) for _ in range(6)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        assert errors == []
        with psycopg.connect(**params) as c:  # type: ignore[arg-type]
            assert c.execute("SELECT count(*) FROM mneme_schema").fetchone() == (1,)


def test_a_waiting_opener_sees_the_initialization_committed_while_it_waited() -> None:
    with pgdb.fresh_backend() as params:
        rr = params | {"options": params["options"] + " -c default_transaction_isolation=repeatable\\ read"}
        result: list[object] = []
        with psycopg.connect(**params) as holder:  # type: ignore[arg-type]
            # Another opener holds the shared lock and initializes while the waiter is blocked.
            holder.execute("SELECT pg_advisory_xact_lock(%s)", (SCHEMA_OPEN_LOCK_KEY,))
            waiter = threading.Thread(
                target=lambda: result.append(PostgresHistoryStore.open(rr, CountingClock()))
            )
            waiter.start()
            waiter.join(1.0)
            assert waiter.is_alive()  # blocked on the shared lock
            schema = params["options"].split("search_path=")[1]
            for statement in DDL:
                holder.execute(bind(statement, schema))
            holder.execute("INSERT INTO mneme_schema (version) VALUES (1)")
            holder.commit()  # releases the lock with the schema committed
        waiter.join(30)
        assert len(result) == 1 and isinstance(result[0], PostgresHistoryStore)
        result[0].close()
        with psycopg.connect(**params) as c:  # type: ignore[arg-type]
            assert c.execute("SELECT count(*) FROM mneme_schema").fetchone() == (1,)


# ------------------------------------------------------------------------------- concurrency (G-084)


def _parallel(n: int, work: Callable[[int], Any]) -> list[Any]:
    results: list[Any] = [None] * n
    start = threading.Barrier(n)

    def run(i: int) -> None:
        start.wait()
        try:
            results[i] = work(i)
        except BaseException as error:
            results[i] = error

    threads = [threading.Thread(target=run, args=(i,)) for i in range(n)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    return results


def test_parallel_writers_get_gapless_disjoint_ranges() -> None:
    with store() as (_, params):
        clock = CountingClock()

        def writer(i: int) -> list[tuple[int, int]]:
            with PostgresHistoryStore.open(params, clock) as s:
                return [
                    (r["first_position"], r["last_position"])
                    for k in range(15)
                    for r in [
                        s.commit_history(
                            "s", WRITER, op(f"w{i}-{k}"), [draft(f"o{i}-{k}-a"), draft(f"o{i}-{k}-b")]
                        )
                    ]
                ]

        ranges = sorted(r for rs in _parallel(8, writer) for r in rs)
        assert [p for a, b in ranges for p in range(a, b + 1)] == list(range(1, 8 * 15 * 2 + 1))
        assert all(b == a + 1 for a, b in ranges)
        assert counts(params) == (240, 240, 120)


def test_parallel_identical_retries_commit_once_and_share_one_receipt() -> None:
    with store() as (_, params):
        clock = CountingClock()

        def retry(_: int) -> object:
            with PostgresHistoryStore.open(params, clock) as s:
                return s.commit_history("s", WRITER, op("same"), [draft("o-1"), draft("o-2")])

        receipts = _parallel(8, retry)
        assert all(r == receipts[0] for r in receipts) and isinstance(receipts[0], dict)
        assert counts(params) == (2, 2, 1) and clock.calls == 1


def test_parallel_conflicting_commits_have_exactly_one_winner() -> None:
    with store() as (_, params):

        def attempt(i: int) -> object:
            with PostgresHistoryStore.open(params, CountingClock()) as s:
                return s.commit_history("s", WRITER, op("same"), [draft(f"o-{i}")])

        results = _parallel(6, attempt)
        winners = [r for r in results if isinstance(r, dict)]
        losers = [r for r in results if isinstance(r, MnemeValidationError)]
        assert len(winners) == 1 and len(losers) == 5
        assert {e.code for e in losers} == {"persistence.operation_conflict"}
        assert counts(params) == (1, 1, 1)


def test_reads_during_commits_see_one_snapshot_of_whole_commits() -> None:
    with store() as (_, params):
        stop = threading.Event()
        problems: list[str] = []

        def write() -> None:
            with PostgresHistoryStore.open(params, CountingClock()) as s:
                for k in range(60):
                    s.commit_history("s", WRITER, op(f"c{k}"), [draft(f"o{k}-{j}") for j in range(3)])
            stop.set()

        def read() -> None:
            with PostgresHistoryStore.open(params, CountingClock()) as s:
                while not stop.is_set():
                    page = s.read_history("s", {"store_id": "s", "position": 0}, MAX)
                    tail = page["tail"]["position"]
                    positions = [e["position"] for e in page["entries"]]
                    if positions != list(range(1, tail + 1)) or tail % 3:
                        problems.append(f"tail {tail}, positions {positions[-3:]}")

        threads = [threading.Thread(target=write)] + [threading.Thread(target=read) for _ in range(3)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        assert problems == []


# ------------------------------------------------------------------------------- remediation (CR-008..011)


@pytest.mark.parametrize(
    "damage",
    [
        "ALTER TABLE mneme_schema RENAME COLUMN version TO revision",
        "ALTER TABLE mneme_store DROP COLUMN tail",
        "ALTER TABLE mneme_store ALTER COLUMN tail TYPE integer",
        "ALTER TABLE mneme_store ALTER COLUMN tail DROP DEFAULT",
        "ALTER TABLE mneme_record ALTER COLUMN envelope DROP NOT NULL",
        "ALTER TABLE mneme_record ADD COLUMN extra text",
        "ALTER TABLE mneme_record DROP CONSTRAINT mneme_record_store_id_record_id_key",
        "ALTER TABLE mneme_store DROP CONSTRAINT mneme_store_tail_check",
        "ALTER TABLE mneme_operation DROP CONSTRAINT mneme_operation_store_id_fkey",
        # two schema rows: only possible once the singleton key and check are gone
        "ALTER TABLE mneme_schema DROP CONSTRAINT mneme_schema_pkey;"
        " ALTER TABLE mneme_schema DROP CONSTRAINT mneme_schema_singleton_check;"
        " INSERT INTO mneme_schema VALUES (false, 1)",
    ],
)
def test_open_rejects_a_malformed_v1_structure_and_changes_nothing(damage: str) -> None:
    with pgdb.fresh_backend() as params:
        PostgresHistoryStore.open(params, CountingClock()).close()
        with psycopg.connect(**params) as c:  # type: ignore[arg-type]
            c.execute(damage)
            c.commit()
        before = catalog(params)
        assert (
            code(lambda: PostgresHistoryStore.open(params, CountingClock()))
            == "persistence.schema_incompatible"
        )
        assert catalog(params) == before


def test_a_fresh_v1_schema_matches_the_pinned_catalog_shape() -> None:
    with pgdb.fresh_backend() as params:
        PostgresHistoryStore.open(params, CountingClock()).close()
        assert catalog(params) == V1_CATALOG


class GatedClock(CountingClock):
    """Signals when a sample begins and holds it until released, to fix an interleaving."""

    def __init__(self) -> None:
        self.sampling, self.release = threading.Event(), threading.Event()
        super().__init__(self._gate)

    def _gate(self) -> None:
        self.sampling.set()
        assert self.release.wait(30)


def test_overlapping_identical_retries_on_one_handle_share_one_commit_and_receipt() -> None:
    clock = GatedClock()
    with store(clock) as (s, params):
        results: list[object] = []

        def commit() -> None:
            try:
                results.append(s.commit_history("s", WRITER, op("same"), [draft("o-1")]))
            except BaseException as error:
                results.append(error)

        first = threading.Thread(target=commit)
        first.start()
        assert clock.sampling.wait(30)  # the first attempt is inside its transaction
        second = threading.Thread(target=commit)
        second.start()
        second.join(0.5)
        assert second.is_alive()  # waits for the handle instead of failing
        clock.release.set()
        first.join(30)
        second.join(30)
        assert len(results) == 2 and all(isinstance(r, dict) for r in results)
        assert results[0] == results[1] and clock.calls == 1
        assert counts(params) == (1, 1, 1)


def test_overlapping_mixed_operations_on_one_handle_all_succeed() -> None:
    clock = GatedClock()
    with store(clock) as (s, _):
        outcomes: dict[str, object] = {}

        def run(name: str, call: Callable[[], object]) -> threading.Thread:
            def body() -> None:
                try:
                    outcomes[name] = call()
                except BaseException as error:
                    outcomes[name] = error

            thread = threading.Thread(target=body)
            thread.start()
            return thread

        writer = run("commit", lambda: s.commit_history("s", WRITER, op("a"), [draft("o-1")]))
        assert clock.sampling.wait(30)
        others = [
            run("other", lambda: s.commit_history("s", WRITER, op("b"), [draft("o-2")])),
            run("read", lambda: s.read_history("s", {"store_id": "s", "position": 0}, 10)),
            run("tail", lambda: s.store_tail("s")),
            run("create", lambda: s.create_store("t")),
        ]
        clock.hook = None  # later samples do not wait
        clock.release.set()
        for thread in [writer, *others]:
            thread.join(30)
        assert not any(isinstance(v, BaseException) for v in outcomes.values()), outcomes
        assert outcomes["commit"]["first_position"] == 1  # type: ignore[index]
        assert s.store_tail("s") == {"store_id": "s", "position": 2}


@pytest.mark.parametrize(
    ("since", "limit", "expected"),
    [
        ({"store_id": "s"}, 1, ("history.boundary_invalid", "since")),
        ({"store_id": "s", "position": -1}, 1, ("history.boundary_invalid", "since")),
        ({"store_id": "S", "position": 0}, 1, ("history.boundary_invalid", "since")),
        ({"store_id": "s", "position": 0}, True, ("persistence.limit_invalid", "limit")),
        ({"store_id": "s", "position": 0}, MAX + 1, ("value.integer_out_of_range", "limit")),
        ({"store_id": "s"}, 0, ("history.boundary_invalid", "since")),  # since precedes limit (stage 3 order)
    ],
)
def test_read_rejects_malformed_arguments_with_their_argument_and_no_position(
    since: object, limit: object, expected: tuple[str, str]
) -> None:
    with store() as (s, _):
        with pytest.raises(MnemeValidationError) as info:
            s.read_history("s", since, limit)
        assert (info.value.code, info.value.argument, info.value.position) == (*expected, None)


def test_read_json_seam_reports_generic_stages_before_typed_checks() -> None:
    with store() as (s, _):
        with pytest.raises(MnemeValidationError) as info:
            s.read_history(JsonText('"S"'), JsonText('{"store_id": "s"}'), JsonText("1e400"))
        assert (info.value.code, info.value.argument) == ("value.number_not_finite", "limit")
