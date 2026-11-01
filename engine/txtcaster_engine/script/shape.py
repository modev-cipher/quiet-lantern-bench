import re

CODE_SIGNAL_MIN = 2
PROSE_FUNCTION_WORD_MIN = 2
PROSE_SENTENCE_WORD_MIN = 8

_WORD_RE = re.compile(r"[A-Za-z][A-Za-z'\u2019-]*")

_PROSE_FUNCTION_WORDS = frozenset(
    {
        "a",
        "about",
        "after",
        "all",
        "also",
        "an",
        "and",
        "any",
        "are",
        "as",
        "at",
        "back",
        "be",
        "because",
        "been",
        "between",
        "but",
        "by",
        "can",
        "come",
        "could",
        "day",
        "do",
        "even",
        "first",
        "for",
        "from",
        "get",
        "give",
        "go",
        "good",
        "had",
        "has",
        "have",
        "he",
        "her",
        "him",
        "his",
        "how",
        "i",
        "if",
        "in",
        "into",
        "is",
        "it",
        "its",
        "just",
        "know",
        "like",
        "look",
        "make",
        "me",
        "most",
        "my",
        "new",
        "no",
        "not",
        "now",
        "of",
        "on",
        "one",
        "only",
        "or",
        "other",
        "our",
        "out",
        "over",
        "people",
        "per",
        "say",
        "see",
        "she",
        "so",
        "some",
        "take",
        "than",
        "that",
        "the",
        "their",
        "them",
        "then",
        "there",
        "these",
        "they",
        "think",
        "this",
        "those",
        "time",
        "to",
        "two",
        "up",
        "us",
        "use",
        "want",
        "was",
        "way",
        "we",
        "well",
        "were",
        "what",
        "when",
        "where",
        "which",
        "while",
        "who",
        "will",
        "with",
        "work",
        "would",
        "year",
        "you",
        "your",
    }
)

_CODE_SIGNALS = (
    re.compile(
        r"\b(?:def|fn|struct|impl)\s+[A-Za-z_]|"
        r"\bclass\s+[A-Za-z_][A-Za-z0-9_]*\s*[:{(]|"
        r"\bclass\s+[A-Za-z_][A-Za-z0-9_]*\s+(?:extends|implements)\b|"
        r"\bfunction(?:\s+[A-Za-z_][A-Za-z0-9_]*)?\s*\(|"
        r"\bfrom\s+[A-Za-z0-9_.]+\s+import\s+|"
        r"\bimport\s+[A-Za-z0-9_.]+|"
        r"\b(?:elif\b|const\b|void\s+[A-Za-z_]|"
        r"public\s+|private\s+|static\s+|yield\s+|async\s+def\b|await\s+)|"
        r"\blet\s+(?:mut\s+)?[A-Za-z_][A-Za-z0-9_]*\s*[:=;]|"
        r"\blambda(?:\s+[A-Za-z_][A-Za-z0-9_]*(?:\s*,\s*[A-Za-z_][A-Za-z0-9_]*)*)?\s*:|"
        r"^\s*return(?:\s+[^.\n]+)?$|"
        r"\breturn\s+[^;.\n]+;|"
        r"\breturn\s+(?:None|True|False|self\b|[0-9]+|\"[^\"]*\"|'[^']*')",
        re.MULTILINE,
    ),
    re.compile(r"\b[A-Za-z_][A-Za-z0-9_]*\([^)]*\)"),
    re.compile(r"[{};]"),
    re.compile(r"(?:=>|::|->|\+=|-=|==|!=|<=|>=|&&|\|\|)"),
    re.compile(r"^[ \t]{2,}\S", re.MULTILINE),
    re.compile(r"[A-Za-z_][A-Za-z0-9_]*\s*=\s*[^=\s]"),
)

_COMMENT_RE = re.compile(r"^\s*(?:#|//|/\*|<!--|--\s+)")
_CODE_FENCE_RE = re.compile(r"```|~~~")
_MATH_FUNC_RE = re.compile(
    r"\b(?:min|max|sin|cos|tan|exp|log|ln|sqrt|softmax|arg\s*max|arg\s*min)\s*\("
)

_GREEK_UNICODE = r"[\u0370-\u03ff]"
_SCIENTIFIC_ARROWS = r"[→⇄⇌↔⇒⇐⇑⇓]"
_MATH_SYMBOLS_RE = re.compile(
    r"[·−×±÷≤≥≠√∑∏∫∂∇∈∉⊂⊆∪∩^°′″‰Å]"
    + f"|{_GREEK_UNICODE}"
    + f"|{_SCIENTIFIC_ARROWS}"
    + r"|\b(?:alpha|beta|gamma|delta|epsilon|theta|mu|pi|sigma|tau|phi|omega)\b",
    re.IGNORECASE,
)

_CHEM_PHASE_RE = re.compile(r"\((?:s|l|g|aq|cr|am)\)", re.IGNORECASE)

_ALGEBRAIC_EQUATION_RE = re.compile(
    r"^\s*[A-Za-z](?:_[A-Za-z0-9]+)?\s*=\s*[-+]?(?:[A-Za-z0-9_.]+|\([A-Za-z0-9_\s.+\-*/^]+\))"
    r"(?:\s*[-+*/^·−×]\s*(?:[A-Za-z0-9_.]+|\([A-Za-z0-9_\s.+\-*/^]+\)))*\s*$"
)


def is_prose_paragraph(text: str) -> bool:
    words = _WORD_RE.findall(text)
    if len(words) < 15:
        return False
    function_words = sum(1 for word in words if word.lower() in _PROSE_FUNCTION_WORDS)
    return function_words >= 4


def is_chemical_reaction(text: str) -> bool:
    has_arrow = bool(re.search(r"\s+(?:->|→|⇄|⇌)\s+", text)) or bool(re.search(r"[→⇄⇌]", text))
    has_phase = bool(_CHEM_PHASE_RE.search(text))
    has_formula_sum = bool(re.search(r"\s*\+\s*", text)) and bool(
        re.search(r"\b[A-Z][a-z]?\d*", text)
    )
    if not (has_arrow and (has_phase or has_formula_sum)):
        return False
    return not bool(_CODE_SIGNALS[0].search(text) or _CODE_SIGNALS[2].search(text))


def is_math_expression(text: str) -> bool:
    if _CODE_SIGNALS[0].search(text) or _CODE_SIGNALS[2].search(text):
        return False
    if is_chemical_reaction(text):
        return True
    if _ALGEBRAIC_EQUATION_RE.match(text.strip()):
        return True
    has_symbols = bool(_MATH_SYMBOLS_RE.search(text))
    has_func = bool(_MATH_FUNC_RE.search(text))
    if not (has_symbols or has_func):
        return False
    if has_symbols:
        return True
    return not (_CODE_SIGNALS[3].search(text) or _CODE_SIGNALS[4].search(text))


def looks_like_code(text: str) -> bool:
    if _COMMENT_RE.match(text):
        signals = sum(1 for pattern in _CODE_SIGNALS if pattern.search(text) is not None)
        return signals >= CODE_SIGNAL_MIN

    if is_math_expression(text):
        return False

    if is_prose_paragraph(text):
        signals = sum(1 for pattern in _CODE_SIGNALS if pattern.search(text) is not None)
        return signals >= 4 or bool(_CODE_FENCE_RE.search(text))

    signals = 0
    for idx, pattern in enumerate(_CODE_SIGNALS):
        if pattern.search(text) is not None:
            if idx == 1 and not _CODE_SIGNALS[2].search(text) and not _CODE_SIGNALS[0].search(text):
                calls = _CODE_SIGNALS[1].findall(text)
                non_chem_calls = [c for c in calls if not _CHEM_PHASE_RE.search(c)]
                if not non_chem_calls:
                    continue
                if all(
                    _MATH_FUNC_RE.match(call) for call in non_chem_calls
                ) and _MATH_SYMBOLS_RE.search(text):
                    continue
            signals += 1
    return signals >= CODE_SIGNAL_MIN


def looks_like_prose(text: str) -> bool:
    words = _WORD_RE.findall(text)
    if not words:
        return False
    function_words = sum(1 for word in words if word.lower() in _PROSE_FUNCTION_WORDS)
    if function_words >= PROSE_FUNCTION_WORD_MIN:
        return True
    return len(words) >= PROSE_SENTENCE_WORD_MIN and text.rstrip().endswith((".", "!", "?"))


def reads_as_sentence(text: str) -> bool:
    return len(_WORD_RE.findall(text)) >= PROSE_SENTENCE_WORD_MIN and looks_like_prose(text)
