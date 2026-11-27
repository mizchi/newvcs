import re

from .errors import DurationError



UNITS = (("d", 86400), ("h", 3600), ("m", 60), ("s", 1))
PART = re.compile(r"\s*(\d+)([a-zA-Z])")


def parse_duration(text):
    if not isinstance(text, str) or not text.strip():
        raise DurationError("empty duration")
    pos, total, last = 0, 0, -1
    body = text.strip()
    while pos < len(body):
        m = PART.match(body, pos)
        if not m:
            raise DurationError(f"bad duration: {text!r}")
        unit = m.group(2).lower()
        order = [u for u, _ in UNITS]
        if unit not in order:
            raise DurationError(f"unknown unit {unit!r}")
        i = order.index(unit)
        if i <= last:
            raise DurationError("units out of order or repeated")
        last = i
        total += int(m.group(1)) * UNITS[i][1]
        pos = m.end()
    return total


def format_duration(seconds):
    if not isinstance(seconds, int) or isinstance(seconds, bool):
        raise TypeError("seconds must be an int")
    if seconds < 0:
        raise DurationError("negative duration")
    if seconds == 0:
        return "0s"
    parts = []
    for unit, size in UNITS:
        n, seconds = divmod(seconds, size)
        if n:
            parts.append(f"{n}{unit}")
    return " ".join(parts)
