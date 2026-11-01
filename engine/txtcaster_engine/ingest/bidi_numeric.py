"""UAX #9 weak-type resolution over a unit sequence; each maximal number is ONE run.

W1-W6 applied; W7 opt-in (off by default) because callers group numbers for L2 ordering, where an
L-attached number already reads left to right. Called by ``bidi._emit`` per geometry cluster (not
per isolating run sequence, which is what UAX #9 defines the rules over). Units are
``(glyph, raw_class)`` pairs, the raw class being ``unicodedata.bidirectional`` of the base glyph.
Order is kept; consecutive units whose resolved class is the same number type (EN or AN) form one
run, every other unit is its own run. W1 NSM takes the previous type (``sos`` first), ON after an
isolate initiator or PDI; W2 EN after AL -> AN; W3 AL -> R; W4 a SINGLE ES between two EN -> EN and
a SINGLE CS between two same-type numbers -> that type; W5 ETs adjacent to EN -> EN; W6 remaining
ES/ET/CS -> ON; W7 (opt-in) EN after L -> L.
"""

from __future__ import annotations

import unicodedata
from collections.abc import Sequence
from dataclasses import dataclass, field
from typing import Generic, TypeVar

G = TypeVar("G")
NUMBERS = frozenset({"EN", "AN"})
ISOLATES = frozenset({"LRI", "RLI", "FSI", "PDI"})


def classify(text: str) -> str:
    return (unicodedata.bidirectional(text[0]) or "ON") if text else "ON"


@dataclass
class Run(Generic[G]):
    cls: str
    units: list[G] = field(default_factory=list)


def resolve_classes(classes: Sequence[str], sos: str = "R", apply_w7: bool = False) -> list[str]:
    out = list(classes)
    prev = sos
    for i, c in enumerate(out):  # W1: NSM takes the previous type, ON after an isolate control
        out[i] = ("ON" if prev in ISOLATES else prev) if c == "NSM" else c
        prev = out[i]
    strong = sos
    for i, c in enumerate(out):  # W2 (backward search folded into a forward scan)
        if c in ("L", "R", "AL"):
            strong = c
        elif c == "EN" and strong == "AL":
            out[i] = "AN"
    out = ["R" if c == "AL" else c for c in out]  # W3
    for i in range(1, len(out) - 1):  # W4
        a, b = out[i - 1], out[i + 1]
        if out[i] == "ES" and a == "EN" and b == "EN":
            out[i] = "EN"
        elif out[i] == "CS" and a in NUMBERS and a == b:
            out[i] = a
    for i in range(len(out)):  # W5: a sequence of ETs adjacent to EN
        if out[i] == "ET":
            j = i
            while j < len(out) and out[j] == "ET":
                j += 1
            if (i > 0 and out[i - 1] == "EN") or (j < len(out) and out[j] == "EN"):
                out[i:j] = ["EN"] * (j - i)
    out = ["ON" if c in ("ES", "ET", "CS") else c for c in out]  # W6
    if apply_w7:
        strong = sos
        for i, c in enumerate(out):
            if c in ("L", "R"):
                strong = c
            elif c == "EN" and strong == "L":
                out[i] = "L"
    return out


def resolve_numeric_runs(
    units: Sequence[tuple[G, str]], sos: str = "R", apply_w7: bool = False
) -> list[Run[G]]:
    resolved = resolve_classes([c for _, c in units], sos, apply_w7)
    runs: list[Run[G]] = []
    for (glyph, _), cls in zip(units, resolved, strict=True):
        if runs and cls in NUMBERS and runs[-1].cls == cls:
            runs[-1].units.append(glyph)
        else:
            runs.append(Run(cls, [glyph]))
    return runs
