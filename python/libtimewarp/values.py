"""Reading and writing the numbers a user types: durations, instants, rates, and clock names.

Durations are spelled the way the library's own environment variables spell them, so ``-2h`` means
the same thing on the command line as it does in ``LIBTIMEWARP_SHIFT``.
"""

from __future__ import annotations

import re
import time
from datetime import datetime, timedelta, timezone
from decimal import Decimal, InvalidOperation
from typing import Optional

#: Nanoseconds in a second, the scale everything here is counted in.
SECOND = 1_000_000_000

#: What each unit is worth in nanoseconds. A day is 86400 seconds; a year is written in days,
#: because years differ in length.
UNITS = {
    "ns": 1,
    "us": 1_000,
    "ms": 1_000_000,
    "s": SECOND,
    "m": 60 * SECOND,
    "h": 3_600 * SECOND,
    "d": 86_400 * SECOND,
}

_DURATION = re.compile(r"^([+-]?\d+(?:\.\d+)?)(ns|us|ms|s|m|h|d)?$")

_EPOCH = datetime(1970, 1, 1, tzinfo=timezone.utc)


def parse_duration(text: str) -> int:
    """Nanoseconds from one number and one unit. A bare number means seconds."""
    matched = _DURATION.match(text.strip())
    if not matched:
        known = ", ".join(UNITS)
        raise ValueError(f"{text!r} is not a duration: one number and one unit out of {known}")
    number, unit = matched.groups()
    try:
        scaled = Decimal(number) * UNITS[unit or "s"]
    except InvalidOperation:
        raise ValueError(f"{text!r} is not a duration") from None
    return int(scaled.to_integral_value())


def format_duration(nanos: int) -> str:
    """A duration back as seconds with nanosecond precision, which reads back in."""
    sign = "-" if nanos < 0 else ""
    whole, fraction = divmod(abs(nanos), SECOND)
    return f"{sign}{whole}.{fraction:09d}s"


def parse_instant(text: str) -> int:
    """Nanoseconds since the epoch, from an RFC 3339 timestamp, ``@`` and Unix seconds, or a
    duration.

    The duration spelling is what the monotonic families take: their values are time since boot
    rather than a date.
    """
    text = text.strip()
    if text.startswith("@"):
        return int((Decimal(text[1:]) * SECOND).to_integral_value())
    if _DURATION.match(text):
        return parse_duration(text)
    stamp = text[:-1] + "+00:00" if text.endswith(("Z", "z")) else text
    try:
        when = datetime.fromisoformat(stamp)
    except ValueError:
        raise ValueError(
            f"{text!r} is not an instant: an RFC 3339 timestamp, @ and Unix seconds, or a duration"
        ) from None
    if when.tzinfo is None:
        when = when.replace(tzinfo=timezone.utc)
    delta: timedelta = when - _EPOCH
    return (delta.days * 86_400 + delta.seconds) * SECOND + delta.microseconds * 1_000


def format_instant(nanos: int) -> str:
    """A wall-clock value as an RFC 3339 timestamp in UTC, with nanoseconds."""
    seconds, fraction = divmod(nanos, SECOND)
    when = _EPOCH + timedelta(seconds=seconds)
    return when.strftime("%Y-%m-%dT%H:%M:%S") + f".{fraction:09d}Z"


def parse_rate(text: str) -> float:
    """How fast a family runs. Zero freezes it; negative is no rate."""
    try:
        rate = float(text)
    except ValueError:
        raise ValueError(f"{text!r} is not a rate") from None
    if rate < 0 or rate != rate:
        raise ValueError(f"{text!r} is not a rate: a rate is at or above zero")
    return rate


def _clock_names() -> dict[int, str]:
    """Every clock id this interpreter knows a name for, which is the platform's own table."""
    names: dict[int, str] = {}
    for name in dir(time):
        if not name.startswith("CLOCK_"):
            continue
        value = getattr(time, name)
        if isinstance(value, int):
            names.setdefault(value, name)
    return names


_NAMES = _clock_names()


def clock_name(clock: Optional[int]) -> str:
    """The name of a clock id, or the number when this platform has no name for it."""
    if clock is None:
        return ""
    return _NAMES.get(clock, str(clock))
