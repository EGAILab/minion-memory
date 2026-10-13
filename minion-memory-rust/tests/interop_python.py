"""Thin interop transport. Expected outcomes belong to Rust contract controls, not this file."""
import json
import os
import sys

from minion_memory.core import JsonText
from minion_memory.persistence.postgres import PostgresHistoryStore


def respond(value):
    print(json.dumps(value, ensure_ascii=True, allow_nan=False), flush=True)


store = PostgresHistoryStore.open(
    os.environ["MNEME_INTEROP_DSN"],
    lambda _: os.environ.get("MNEME_INTEROP_CLOCK", "2026-10-11T08:00:00.000000Z"),
)
respond({"ready": True})
try:
    for line in sys.stdin:
        request = json.loads(line)
        operation = request.pop("operation_name")
        arguments = {
            name: JsonText(request[name + "_json"])
            if name + "_json" in request
            else request[name]
            for name in {
                "create_store": ("store_id",),
                "store_tail": ("store_id",),
                "commit_history": ("store_id", "writer", "operation", "records"),
                "read_history": ("store_id", "since", "limit"),
            }[operation]
        }
        try:
            respond({"value": getattr(store, operation)(**arguments)})
        except Exception as error:
            respond({"error": getattr(error, "code", type(error).__name__),
                     "argument": getattr(error, "argument", None)})
finally:
    store.close()
