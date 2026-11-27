from .errors import RomanError



VALUES = [(1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"), (50, "L"), (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")]


def to_roman(n):
    if not isinstance(n, int) or isinstance(n, bool):
        raise TypeError("n must be an int")
    if not 1 <= n <= 3999:
        raise RomanError("out of range")
    out = []
    for value, letters in VALUES:
        while n >= value:
            out.append(letters)
            n -= value
    return "".join(out)


CANON = {to_roman(n): n for n in range(1, 4000)}


def from_roman(text):
    if not isinstance(text, str) or text.upper() not in CANON:
        raise RomanError(f"not a canonical numeral: {text!r}")
    return CANON[text.upper()]
