"""Timestamp input grammar and canonical normalization (HIST-004; G-022, G-026, G-028, G-029, G-030).

The accepted grammar is the contract's, implemented here directly; `datetime` is used only for
checked calendar arithmetic over 0001..9999, never as the parser of the accepted language.
"""

from __future__ import annotations

import calendar
import re
from datetime import datetime, timedelta

from .errors import MnemeValidationError
from .identity import _domain, _typed
from .values import Value

_GRAMMAR = re.compile(
    r"([0-9]{4})-([0-9]{2})-([0-9]{2})[Tt]([0-9]{2}):([0-9]{2}):([0-9]{2})(?:\.([0-9]{1,6}))?"
    r"([Zz]|[+-][0-9]{2}:[0-9]{2})",
    re.ASCII,
)


def _invalid(detail: str) -> MnemeValidationError:
    return MnemeValidationError("timestamp.invalid", detail)


def _timestamp_rule(value: Value) -> str:
    if not isinstance(value, str):
        raise _invalid("a timestamp is a string")
    match = _GRAMMAR.fullmatch(value)
    if match is None:
        raise _invalid(f"{value!r} is not in the timestamp grammar")
    year, month, day, hour, minute, second = (int(part) for part in match.groups()[:6])
    fraction, offset = match.group(7), match.group(8)
    if not (1 <= year <= 9999 and 1 <= month <= 12):
        raise _invalid("year must be 0001-9999 and month 01-12")
    if not 1 <= day <= calendar.monthrange(year, month)[1]:
        raise _invalid("invalid calendar day")
    if hour > 23 or minute > 59 or second > 59:
        raise _invalid("hour 00-23, minute 00-59, second 00-59 (no leap second)")
    if offset in ("Z", "z"):
        offset_minutes = 0
    else:
        offset_hour, offset_minute = int(offset[1:3]), int(offset[4:6])
        if offset_hour > 23 or offset_minute > 59:
            raise _invalid("offset hour 00-23, minute 00-59")
        offset_minutes = (offset_hour * 60 + offset_minute) * (1 if offset[0] == "+" else -1)
    local = datetime(year, month, day, hour, minute, second, int((fraction or "").ljust(6, "0") or "0"))
    try:
        instant = local - timedelta(minutes=offset_minutes)
    except OverflowError:
        raise _invalid("the UTC instant leaves 0001-01-01..9999-12-31") from None
    return (
        f"{instant.year:04d}-{instant.month:02d}-{instant.day:02d}T{instant.hour:02d}:"
        f"{instant.minute:02d}:{instant.second:02d}.{instant.microsecond:06d}Z"
    )


def normalize_timestamp(value: object) -> str:
    """Accept exactly the shared grammar and return `YYYY-MM-DDTHH:MM:SS.ffffffZ`."""
    return _typed(_domain({"timestamp": value}), _timestamp_rule)[0]
