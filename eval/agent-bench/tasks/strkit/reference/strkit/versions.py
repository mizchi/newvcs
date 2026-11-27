import re
from typing import NamedTuple

from .errors import VersionError



class Version(NamedTuple):
    major: int
    minor: int
    patch: int
    pre: tuple = ()


NUM = r"(0|[1-9][0-9]*)"
FORM = re.compile(rf"{NUM}\.{NUM}\.{NUM}(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?")


def parse_version(text):
    m = FORM.fullmatch(text) if isinstance(text, str) else None
    if not m:
        raise VersionError(f"not a version: {text!r}")
    pre = []
    for ident in (m.group(4).split(".") if m.group(4) else []):
        if ident.isdigit():
            if len(ident) > 1 and ident[0] == "0":
                raise VersionError("leading zero")
            pre.append(int(ident))
        else:
            pre.append(ident)
    return Version(int(m.group(1)), int(m.group(2)), int(m.group(3)), tuple(pre))


def _key(ident):
    return (0, ident, "") if isinstance(ident, int) else (1, 0, ident)


def compare_versions(a, b):
    va, vb = parse_version(a), parse_version(b)
    if va[:3] != vb[:3]:
        return -1 if va[:3] < vb[:3] else 1
    if va.pre == vb.pre:
        return 0
    if not va.pre:
        return 1
    if not vb.pre:
        return -1
    for x, y in zip(va.pre, vb.pre):
        if x != y:
            return -1 if _key(x) < _key(y) else 1
    return -1 if len(va.pre) < len(vb.pre) else 1


def bump(text, part):
    v = parse_version(text)
    if part == "major":
        return f"{v.major + 1}.0.0"
    if part == "minor":
        return f"{v.major}.{v.minor + 1}.0"
    if part == "patch":
        return f"{v.major}.{v.minor}.{v.patch + 1}"
    raise VersionError(f"unknown part {part!r}")
