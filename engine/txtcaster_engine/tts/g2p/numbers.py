import re
from decimal import Decimal

import inflect

_ENGINE = inflect.engine()
_IRREGULAR_NTH = {
    "one": "first",
    "two": "second",
    "three": "third",
    "five": "fifth",
    "eight": "eighth",
    "nine": "ninth",
    "twelve": "twelfth",
}


def _cardinal(value: int) -> str:
    return str(_ENGINE.number_to_words(str(value), andword="and"))


def _nth(words: str) -> str:
    last = re.split(r"[^a-z]", words)[-1]
    head = words[: len(words) - len(last)]
    if last in _IRREGULAR_NTH:
        return head + _IRREGULAR_NTH[last]
    if last.endswith("y"):
        return f"{head}{last[:-1]}ieth"
    return f"{head}{last}th"


def _year(value: int) -> str:
    high, low = divmod(value, 100)
    if high == 0 or (high % 10 == 0 and low < 10) or high >= 100:
        return _cardinal(value)
    if low == 0:
        tail = "hundred"
    elif low < 10:
        tail = f"oh-{_cardinal(low)}"
    else:
        tail = _cardinal(low)
    return f"{_cardinal(high)} {tail}"


def _decimal(value: float) -> str:
    digits = format(Decimal(repr(value)), "f")
    return str(_ENGINE.number_to_words(digits, andword="and"))


def num2words(number: int | float, to: str = "cardinal") -> str:
    if to == "cardinal":
        if isinstance(number, float):
            return _decimal(number)
        return _cardinal(number)
    if to == "ordinal":
        return _nth(_cardinal(int(number)))
    if to == "year":
        return _year(int(number))
    raise NotImplementedError(f"number mode {to!r} is not spoken")
