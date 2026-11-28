"""Small string utilities."""

# One import to a line, and one name to a line in __all__: writers add lines here at once,
# and a merge keeps the lines of each (merge=union in .gitattributes).

from .errors import StrkitError
__all__ = [
    "StrkitError",
]
