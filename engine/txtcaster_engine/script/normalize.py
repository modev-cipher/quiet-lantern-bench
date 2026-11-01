import re
from dataclasses import dataclass

CITATION_RE = re.compile(r"\[\d+(?:\s*[,\u2013-]\s*\d+)*\]")
URL_RE = re.compile(r"https?://\S+|www\.\S+")
DEGREE_RE = re.compile(r"(\d+(?:\.\d+)?)\s*\u00b0\s*([CF])\b")
PERCENT_RE = re.compile(r"(\d)\s*%")

UNIT_WORDS = {
    "kg": "kilograms",
    "km": "kilometers",
    "cm": "centimeters",
    "mm": "millimeters",
    "kb": "kilobytes",
    "mb": "megabytes",
    "gb": "gigabytes",
    "tb": "terabytes",
    "hz": "hertz",
    "khz": "kilohertz",
    "mhz": "megahertz",
    "ghz": "gigahertz",
}
UNIT_RE = re.compile(
    r"(?<=\d)\s?(" + "|".join(sorted(UNIT_WORDS, key=len, reverse=True)) + r")\b",
    re.IGNORECASE,
)

ABBREVIATIONS = {
    "e.g.": "for example",
    "i.e.": "that is",
    "etc.": "and so on",
    "vs.": "versus",
    "approx.": "approximately",
    "Fig.": "Figure",
    "fig.": "figure",
    "Eq.": "Equation",
    "eq.": "equation",
    "Dr.": "Doctor",
    "Mr.": "Mister",
    "Mrs.": "Missus",
    "Ms.": "Miss",
    "Prof.": "Professor",
    "St.": "Saint",
}
ABBREVIATION_RE = re.compile(
    "|".join(re.escape(key) for key in sorted(ABBREVIATIONS, key=len, reverse=True))
)

SUPERSCRIPT_DIGITS = {
    "\u00b2": "squared",
    "\u00b3": "cubed",
    "\u2070": "to the zeroth",
    "\u00b9": "to the first",
    "\u2074": "to the fourth",
    "\u2075": "to the fifth",
    "\u2076": "to the sixth",
    "\u2077": "to the seventh",
    "\u2078": "to the eighth",
    "\u2079": "to the ninth",
    "\u207a": "plus",
    "\u207b": "minus",
}
SUPERSCRIPT_RE = re.compile(
    r"([A-Za-z0-9\)])\s*([\u00b2\u00b3\u2070\u00b9\u2074-\u2079\u207a\u207b])"
)

SUBSCRIPT_DIGITS = {
    "\u2080": "zero",
    "\u2081": "one",
    "\u2082": "two",
    "\u2083": "three",
    "\u2084": "four",
    "\u2085": "five",
    "\u2086": "six",
    "\u2087": "seven",
    "\u2088": "eight",
    "\u2089": "nine",
}
SUBSCRIPT_RE = re.compile(r"([A-Za-z\)])([\u2080-\u2089]+)")

DIGIT_WORDS = {
    "0": "zero",
    "1": "one",
    "2": "two",
    "3": "three",
    "4": "four",
    "5": "five",
    "6": "six",
    "7": "seven",
    "8": "eight",
    "9": "nine",
}
ORDINAL_WORDS = {
    "2": "squared",
    "3": "cubed",
    "4": "to the fourth",
    "5": "to the fifth",
    "6": "to the sixth",
    "7": "to the seventh",
    "8": "to the eighth",
    "9": "to the ninth",
}

MATH_DROPPED_FLAG = "math_dropped"

TEX_SPAN_MAX_CHARS = 48
TEX_WORDS = {
    "alpha": "alpha",
    "beta": "beta",
    "gamma": "gamma",
    "delta": "delta",
    "epsilon": "epsilon",
    "theta": "theta",
    "lambda": "lambda",
    "mu": "mu",
    "pi": "pi",
    "rho": "rho",
    "sigma": "sigma",
    "tau": "tau",
    "phi": "phi",
    "omega": "omega",
    "cup": "union",
    "cap": "intersection",
    "cdot": "dot",
    "times": "times",
    "div": "divided by",
    "pm": "plus or minus",
    "leq": "less than or equal to",
    "geq": "greater than or equal to",
    "neq": "not equal to",
    "approx": "approximately",
    "infty": "infinity",
    "to": "goes to",
    "rightarrow": "goes to",
    "sqrt": "square root of",
    "exp": "exp",
    "log": "log",
    "sin": "sine",
    "cos": "cosine",
    "tan": "tangent",
}
TEX_COMPLEX_COMMANDS = frozenset(
    {
        "frac",
        "left",
        "right",
        "sum",
        "int",
        "prod",
        "begin",
        "end",
        "matrix",
        "cases",
        "binom",
        "over",
        "atop",
    }
)

TEX_SPAN_RE = re.compile(r"\$[^$\n]{1,400}\$|\\\((.{1,400}?)\\\)|\\\[(.{1,400}?)\\\]", re.DOTALL)
TEX_BARE_RE = re.compile(r"\\[A-Za-z]+|\\[{}]")
TEX_TOKEN_RE = re.compile(r"\\([A-Za-z]+)")
TEX_SUPERSCRIPT_RE = re.compile(r"([A-Za-z0-9\)])\s*\^\s*\{?([0-9])\}?")
TEX_SUBSCRIPT_RE = re.compile(r"([A-Za-z\)])\s*_\s*\{?([A-Za-z0-9]{1,3})\}?")
_TEX_HINT_RE = re.compile(r"[\\_^]")
_TEX_SIMPLE_TOKEN_RE = re.compile(r"^[A-Za-z0-9']{1,4}$")

MATH_SYMBOLS = {
    "=": " equals ",
    "\u00d7": " times ",
    "\u2212": " minus ",
    "\u2264": " less than or equal to ",
    "\u2265": " greater than or equal to ",
    "\u2260": " not equal to ",
    "\u2248": " approximately ",
    "\u00b1": " plus or minus ",
    "\u00f7": " divided by ",
    "\u221a": " square root of ",
    "\u03c0": " pi ",
    "\u221e": " infinity ",
    "\u0394": " delta ",
    "\u2192": " goes to ",
}


@dataclass
class NormalizedText:
    text: str
    offsets: list[int]
    math_dropped: bool = False

    def source_offset(self, index: int) -> int:
        if not self.offsets:
            return index
        clamped = max(0, min(index, len(self.offsets) - 1))
        return self.offsets[clamped]


class _Builder:
    def __init__(self, source: str) -> None:
        self._source = source
        self._out: list[str] = []
        self._offsets: list[int] = []

    def emit(self, text: str, source_start: int) -> None:
        for i, ch in enumerate(text):
            self._out.append(ch)
            self._offsets.append(min(source_start + i, len(self._source)))

    def emit_mapped(self, text: str, source_start: int, source_end: int) -> None:
        for ch in text:
            self._out.append(ch)
            self._offsets.append(source_start)
        if text:
            self._offsets[-1] = max(source_start, source_end - 1)

    def build(self) -> NormalizedText:
        return NormalizedText("".join(self._out), list(self._offsets))


def _collapse_spaces(value: str) -> str:
    collapsed = re.sub(r"[ \t]{2,}", " ", value)
    collapsed = re.sub(r"\s+([.,;:!?])", r"\1", collapsed)
    collapsed = re.sub(r",(\s*,)+", ",", collapsed)
    collapsed = re.sub(r",\s*([.;:!?])", r"\1", collapsed)
    collapsed = re.sub(r"[ \t]{2,}", " ", collapsed)
    return collapsed.strip()


def _tex_inner(span: str) -> str:
    if span.startswith("$") and span.endswith("$"):
        return span[1:-1]
    return span[2:-2]


def _looks_like_tex(inner: str) -> bool:
    stripped = inner.strip()
    if not stripped:
        return False
    if _TEX_HINT_RE.search(stripped):
        return True
    return _TEX_SIMPLE_TOKEN_RE.match(stripped) is not None


def _tex_is_complex(inner: str) -> bool:
    if len(inner) > TEX_SPAN_MAX_CHARS:
        return True
    if inner.count("{") > 1:
        return True
    return any(match.group(1) in TEX_COMPLEX_COMMANDS for match in TEX_TOKEN_RE.finditer(inner))


def _widen_dropped_span(source: str, start: int, end: int) -> tuple[int, int]:
    leading = source[:start].rstrip()
    if leading.endswith(",") and end < len(source) and source[end] == ",":
        return len(leading) - 1, end + 1
    return start, end


def _tex_command_word(match: re.Match[str]) -> str:
    return f" {TEX_WORDS.get(match.group(1), '')} "


def _digits_to_words(digits: str) -> str:
    return " ".join(DIGIT_WORDS.get(char, char) for char in digits)


def _subscript_words(chars: str) -> str:
    return " ".join(SUBSCRIPT_DIGITS.get(char, char) for char in chars)


def _verbalise_tex(inner: str) -> str:
    text = TEX_TOKEN_RE.sub(_tex_command_word, inner)
    text = TEX_SUPERSCRIPT_RE.sub(
        lambda m: f"{m.group(1)} {ORDINAL_WORDS.get(m.group(2), 'to the ' + m.group(2))}", text
    )
    text = TEX_SUBSCRIPT_RE.sub(
        lambda m: (
            f"{m.group(1)} {_digits_to_words(m.group(2)) if m.group(2).isdigit() else m.group(2)}"
        ),
        text,
    )
    text = text.replace("'", " prime")
    text = re.sub(r"[{}\\]", " ", text)
    for symbol, replacement in MATH_SYMBOLS.items():
        text = text.replace(symbol, replacement)
    return _collapse_spaces(text)


def normalize_text(source: str) -> NormalizedText:
    matches: list[tuple[int, int, str]] = []
    math_dropped = False
    for match in TEX_SPAN_RE.finditer(source):
        inner = _tex_inner(match.group(0))
        if not _looks_like_tex(inner):
            continue
        if _tex_is_complex(inner):
            start, end = _widen_dropped_span(source, match.start(), match.end())
            matches.append((start, end, ""))
            math_dropped = True
            continue
        spoken = _verbalise_tex(inner)
        matches.append((match.start(), match.end(), f" {spoken}," if spoken else ""))
    for match in TEX_BARE_RE.finditer(source):
        token = match.group(0)
        if token in {"\\{", "\\}"}:
            matches.append((match.start(), match.end(), " "))
            continue
        name = token[1:]
        if name in TEX_COMPLEX_COMMANDS:
            math_dropped = True
            matches.append((match.start(), match.end(), " "))
            continue
        matches.append((match.start(), match.end(), f" {TEX_WORDS.get(name, '')} "))
    for match in SUBSCRIPT_RE.finditer(source):
        matches.append(
            (match.start(), match.end(), f" {match.group(1)} {_subscript_words(match.group(2))} ")
        )
    for match in CITATION_RE.finditer(source):
        matches.append((match.start(), match.end(), ""))
    for match in URL_RE.finditer(source):
        end = match.end()
        while end > match.start() and source[end - 1] in ",.;:!?)":
            end -= 1
        matches.append((match.start(), end, "see the link on this page"))
    for match in DEGREE_RE.finditer(source):
        unit = "Celsius" if match.group(2) == "C" else "Fahrenheit"
        matches.append((match.start(), match.end(), f"{match.group(1)} degrees {unit}"))
    for match in PERCENT_RE.finditer(source):
        matches.append((match.start(), match.end(), f"{match.group(1)} percent"))
    for match in SUPERSCRIPT_RE.finditer(source):
        word = SUPERSCRIPT_DIGITS[match.group(2)]
        matches.append((match.start(), match.end(), f"{match.group(1)} {word}"))
    for match in UNIT_RE.finditer(source):
        matches.append((match.start(), match.end(), " " + UNIT_WORDS[match.group(1).lower()]))
    for match in ABBREVIATION_RE.finditer(source):
        matches.append((match.start(), match.end(), ABBREVIATIONS[match.group(0)]))
    for symbol, replacement in MATH_SYMBOLS.items():
        start = 0
        while True:
            idx = source.find(symbol, start)
            if idx < 0:
                break
            matches.append((idx, idx + len(symbol), replacement))
            start = idx + len(symbol)

    matches.sort(key=lambda item: (item[0], -item[1]))
    builder = _Builder(source)
    cursor = 0
    for start, end, replacement in matches:
        if end <= cursor:
            continue
        if start < cursor:
            start = cursor
        builder.emit(source[cursor:start], cursor)
        builder.emit_mapped(replacement, start, end)
        cursor = end
    builder.emit(source[cursor:], cursor)

    normalized = builder.build()
    normalized.math_dropped = math_dropped
    collapsed = _collapse_spaces(normalized.text)
    if collapsed == normalized.text:
        return normalized
    reprojected = _reproject(normalized, collapsed)
    reprojected.math_dropped = math_dropped
    return reprojected


def _reproject(normalized: NormalizedText, collapsed: str) -> NormalizedText:
    text = normalized.text
    offsets = normalized.offsets
    stripped_leading = len(text) - len(text.lstrip())
    new_offsets: list[int] = []
    src_index = stripped_leading
    for ch in collapsed:
        while src_index < len(text) and text[src_index] != ch:
            src_index += 1
        if src_index < len(offsets):
            new_offsets.append(offsets[src_index])
        elif offsets:
            new_offsets.append(offsets[-1])
        else:
            new_offsets.append(0)
        src_index += 1
    return NormalizedText(collapsed, new_offsets)
