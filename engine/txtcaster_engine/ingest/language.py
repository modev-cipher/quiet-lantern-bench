import re
import unicodedata

STOPWORDS: dict[str, frozenset[str]] = {
    "en": frozenset(
        ("the", "of", "and", "to", "in", "is", "that", "it", "was", "for")
        + ("on", "are", "as", "with", "his", "they", "at", "be", "this", "from")
    ),
    "es": frozenset(
        ("el", "la", "de", "que", "y", "en", "los", "se", "del", "las")
        + ("un", "por", "con", "no", "una", "su", "para", "es", "al", "como")
    ),
    "fr": frozenset(
        ("le", "de", "la", "et", "les", "des", "en", "un", "du", "une")
        + ("que", "est", "pour", "qui", "dans", "par", "sur", "au", "plus", "ne")
    ),
    "de": frozenset(
        ("der", "die", "und", "in", "den", "von", "zu", "das", "mit", "sich")
        + ("des", "auf", "ist", "im", "dem", "nicht", "ein", "eine", "als")
    ),
    "it": frozenset(
        ("di", "che", "la", "il", "un", "a", "per", "in", "una", "sono")
        + ("le", "con", "non", "si", "da", "come", "lo", "dei", "nel", "alla")
    ),
    "pt": frozenset(
        ("de", "a", "o", "que", "e", "do", "da", "em", "um", "para")
        + ("com", "uma", "os", "no", "se", "na", "por", "mais", "as", "dos")
    ),
}

MIN_HITS = 5
SAMPLE_WORD_LIMIT = 2000

SCRIPT_RANGES: dict[str, tuple[tuple[int, int], ...]] = {
    "han": ((0x4E00, 0x9FFF), (0x3400, 0x4DBF), (0xF900, 0xFAFF)),
    "kana": ((0x3040, 0x309F), (0x30A0, 0x30FF)),
    "hangul": ((0xAC00, 0xD7A3), (0x1100, 0x11FF), (0x3130, 0x318F)),
    "arabic": ((0x0600, 0x06FF), (0x0750, 0x077F), (0x08A0, 0x08FF)),
    "hebrew": ((0x0590, 0x05FF),),
}

SCRIPT_SHARE_FLOOR = 0.20
MIN_SCRIPT_HITS = 10
KANA_SHARE_FOR_JAPANESE = 0.05
SAMPLE_CHAR_LIMIT = 8000
SCRIPT_SAMPLE_STRIDES = 24

LOWEST_TRACKED_CODE_POINT = min(start for ranges in SCRIPT_RANGES.values() for start, _ in ranges)


def _script_of(char: str) -> str | None:
    """Which tracked script a character belongs to, or None.

    The early return is exact, not an approximation: it is derived from `SCRIPT_RANGES`
    rather than written down, so it cannot fall out of step with the table when a script
    is added. Measured over every code point 0..0x110000, no tracked point lies below it.

    It is here because the script sample is now the whole document rather than its first
    8000 characters, and this table walk is the per-character cost of that. Latin prose
    reaches the loop on every letter and never matches. Over 184 real documents and
    32 640 827 characters the early return moves the scan from 2.43 to 23.61 M chars/s
    (73.0 -> 7.5 ms per document, and 1640 -> 157 ms on the largest book measured), with
    the two implementations disagreeing on zero documents.
    """
    point = ord(char)
    if point < LOWEST_TRACKED_CODE_POINT:
        return None
    for script, ranges in SCRIPT_RANGES.items():
        for start, end in ranges:
            if start <= point <= end:
                return script
    return None


def _script_sample(text: str) -> str:
    """`SCRIPT_SAMPLE_STRIDES` looks of `SAMPLE_CHAR_LIMIT` characters, spread across the text.

    Reading one look at the front is what made the script share depend on where a book keeps
    its code. Measured on a 40-page real translated manual with a real code appendix in front,
    the front-only window held the correct verdict to 4 leading code pages in Chinese and 1 in
    Japanese; spreading the same kind of look across the document holds it to 17 and 9 — from
    10% and 2.5% of the book to 42.5% and 22.5%. On the 184 real documents available this
    sample agrees with reading the whole text on every one of them, for the script verdict and
    for the final verdict alike.

    A budget rather than the whole text, because this runs on every ingest and a hosted product
    is handed documents of any size: the work is capped at 192 000 characters (~8 ms) however
    long the book is, where reading it all is unbounded and already costs 157 ms on the largest
    book measured here. Nothing is bought with that cap except boundedness — 160 of those 184
    documents are shorter than the budget and are read whole.

    Neither constant is load-bearing. On a document ten times the budget with its first 400
    pages of listings, every stride count from 2 to 64 and every budget from 8 000 to 1 000 000
    returns the same verdict; only a single stride — today's behaviour — misses. The count is
    then set by estimator error against the floor: the striped share must not move a document
    across `SCRIPT_SHARE_FLOOR` on its own, and the floor sits 0.040 above the highest share any
    real English document reaches. Worst-case error over 189 documents is 0.092 at 8 strides and
    0.041 at 16 — both able to cross that — against 0.017 at 24, a 2.4x margin. The grid is in
    `script_share_corpus.json`, and changing either constant to a shape it has not measured
    fails the suite rather than silently shipping.
    """
    budget = SCRIPT_SAMPLE_STRIDES * SAMPLE_CHAR_LIMIT
    if len(text) <= budget:
        return text
    step = (len(text) - SAMPLE_CHAR_LIMIT) / (SCRIPT_SAMPLE_STRIDES - 1)
    return "".join(
        text[round(index * step) : round(index * step) + SAMPLE_CHAR_LIMIT]
        for index in range(SCRIPT_SAMPLE_STRIDES)
    )


def _detect_script_language(text: str) -> str | None:
    counts = dict.fromkeys(SCRIPT_RANGES, 0)
    letters = 0
    for char in _script_sample(text):
        if not char.isalpha():
            continue
        letters += 1
        script = _script_of(char)
        if script is not None:
            counts[script] += 1
    if letters == 0:
        return None
    cjk = counts["han"] + counts["kana"]
    kana_floor = max(MIN_SCRIPT_HITS, cjk * KANA_SHARE_FOR_JAPANESE)
    candidates = {
        "ja": cjk if counts["kana"] >= kana_floor else 0,
        "zh": cjk if counts["kana"] < kana_floor else 0,
        "ko": counts["hangul"],
        "ar": counts["arabic"],
        "he": counts["hebrew"],
    }
    best = max(candidates, key=lambda lang: candidates[lang])
    hits = candidates[best]
    if hits < MIN_SCRIPT_HITS or hits / letters < SCRIPT_SHARE_FLOOR:
        return None
    return best


NON_LATIN_WORD_MIN_RUN = 2


def non_latin_word_share(text: str, min_run: int = NON_LATIN_WORD_MIN_RUN) -> tuple[float, int]:
    """What fraction of the text's letters sit inside a non-Latin *word*, and how many.

    A word here is `min_run` or more **adjacent** non-Latin letters. Anything that is not a
    non-Latin letter breaks the run -- a space, a digit, an operator, punctuation, or a
    Latin letter -- so the alpha in `α + β` is never counted while the six letters of
    `Привет` are.

    Counting bare non-Latin letters instead was measurably wrong, not merely imprecise.
    A Latin-alphabet speech engine mispronounces non-Latin *words* (espeak `en-us` renders
    `Привет мир` as "pe er i ve ye te em i er", spelling each letter) but reads isolated
    non-Latin *symbols* correctly (`Let α + β = θ` becomes "let alpha plus beta equals
    theta", which is right for a maths book). A per-character share cannot separate those,
    and on real input it inverts: display maths such as `α ≤ β ≤ γ ≤ … ≤ κ` scores a
    per-character share of 1.000 -- the maximum, indistinguishable from a Chinese sentence
    -- while espeak speaks it perfectly. The run makes that case 0.000.

    Measured over the 15 samples this repo ships or the reviewer supplied, the run
    predicate separates the two populations by **+0.700** (highest speakable 0.133, lowest
    must-refuse 0.833) where the per-character share scores **-0.167** on the same fixtures:
    inverted, meaning no threshold exists that both speaks display maths and refuses Cyrillic
    diluted with English words. It was unfixable, not mis-tuned.

    `unicodedata` rather than a range table because the two callers have different budgets
    and different costs of being wrong. `_script_of` walks whole-document samples (up to
    192 000 characters per ingest) and a script it misses only softens a language guess;
    this runs once per TTS segment, where a Latin block left out of a hand-written table
    would read as foreign and refuse text the engine could have spoken. It also covers
    blocks a table would forget: fullwidth `Ｔｈｅ` is `FULLWIDTH LATIN ...`, not `LATIN ...`,
    so it counts as non-Latin -- correctly, because espeak reads it as "letter ef ef five
    four", the hex codepoint.

    Returns the share and the count together because a share over a handful of letters is
    noise: callers are expected to require a floor of letters before trusting it.
    """
    letters = sum(1 for char in text if char.isalpha())
    if letters == 0:
        return 0.0, 0
    counted = 0
    run = 0
    for char in text:
        if char.isalpha() and not _is_latin_letter(char):
            run += 1
            continue
        if run >= min_run:
            counted += run
        run = 0
    if run >= min_run:
        counted += run
    return counted / letters, counted


def _is_latin_letter(char: str) -> bool:
    return unicodedata.name(char, "").startswith("LATIN")


def non_latin_letter_share(text: str) -> tuple[float, int]:
    """What fraction of the text's letters are not Latin, and how many those are.

    Superseded for speech decisions by `non_latin_word_share`, which is what the piper
    backend uses: this per-character view cannot tell a non-Latin word from an isolated
    non-Latin symbol, and espeak treats those two completely differently. Kept because it
    is the honest primitive for "how much of this text is not written in Latin script",
    a question with other answers than speakability.

    Deliberately broader than `SCRIPT_RANGES`, and not built on it. That table tracks the
    five scripts `detect_language` can name, but a Latin-alphabet speech engine mispronounces
    every script it does not have, and measured against espeak's `en-us` voice that includes
    Cyrillic, Greek, Thai and Devanagari as well -- all of which come back as strings of
    letter names. Widening `SCRIPT_RANGES` to cover them would change what `detect_language`
    reports and move the population `SCRIPT_SHARE_FLOOR` was calibrated against, so the
    predicate a speech engine needs is kept separate from the one language detection needs.

    `unicodedata` rather than a range table because the two callers have different budgets
    and different costs of being wrong. `_script_of` walks whole-document samples (up to
    192 000 characters per ingest) and a script it misses only softens a language guess;
    this runs once per TTS segment, where a Latin block left out of a hand-written table
    would read as foreign and refuse text the engine could have spoken. Measured at
    6.7 M chars/s, which is immaterial against a 280-character segment.

    Returns the share and the count together because a share over a handful of letters is
    noise: callers are expected to require a floor of letters before trusting it.
    """
    letters = 0
    non_latin = 0
    for char in text:
        if not char.isalpha():
            continue
        letters += 1
        if not _is_latin_letter(char):
            non_latin += 1
    if letters == 0:
        return 0.0, 0
    return non_latin / letters, non_latin


def detect_language(text: str, *, script_sample: str | None = None) -> str | None:
    """The document's language, from its script where one is decisive and its words otherwise.

    The two decisions want different amounts of text, which is why the caller may hand them
    different amounts. Script share is a ratio and a whole-document property, so the widest
    representative sample is the best estimate of it. The word scorer is an argmax over counts
    of short function words, and those are exactly what non-prose contributes: measured over 184
    real documents, widening its input from the opening pages to the whole book turns two real
    English lecture decks Portuguese, because a bullet glyph extracts as `a` 243 times and `a`
    is a Portuguese stopword. Widening both together therefore trades one defect for another.
    `text` is the prose sample the word scorer has always been calibrated on; `script_sample`
    defaults to it and is widened by callers that can afford to. The scorer's own fragility is
    #150; until that is fixed, widening its input is a regression rather than an improvement.
    """
    scripted = _detect_script_language(text if script_sample is None else script_sample)
    if scripted is not None:
        return scripted
    words = re.findall(r"[a-z\u00e0-\u00ff]+", text.lower())[:SAMPLE_WORD_LIMIT]
    if not words:
        return None
    scores = {
        lang: sum(1 for word in words if word in stopwords) for lang, stopwords in STOPWORDS.items()
    }
    best = max(scores, key=lambda lang: scores[lang])
    if scores[best] < MIN_HITS:
        return None
    return best
