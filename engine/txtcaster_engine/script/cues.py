from dataclasses import dataclass

from txtcaster_engine.script.shape import looks_like_code

_VOWELS = frozenset("aeiou")

CODE_KIND = "code"

DECIDED_BY_LABEL = "label"
DECIDED_BY_HEURISTIC = "heuristic"


@dataclass(frozen=True)
class CuePolicy:
    noun: str


CUE_POLICY: dict[str, CuePolicy] = {
    "figure": CuePolicy(noun="figure"),
    "chart": CuePolicy(noun="chart"),
    "table": CuePolicy(noun="table"),
    "equation": CuePolicy(noun="equation"),
    CODE_KIND: CuePolicy(noun="code block"),
}


def policy_for(kind: str) -> CuePolicy:
    return CUE_POLICY.get(kind, CuePolicy(noun=kind))


def article_for(noun: str) -> str:
    return "An" if noun[:1].lower() in _VOWELS else "A"


def cue_opener(kind: str) -> str:
    noun = policy_for(kind).noun
    return f"{article_for(noun)} {noun} appears here."


@dataclass(frozen=True)
class CodeDecision:
    is_code: bool
    decided_by: str


def decide_code(label: str | None, text: str) -> CodeDecision:
    if label:
        return CodeDecision(is_code=label == CODE_KIND, decided_by=DECIDED_BY_LABEL)
    return CodeDecision(is_code=looks_like_code(text), decided_by=DECIDED_BY_HEURISTIC)


def suppressed_by_collapse(pages: list[int], decisions: list[CodeDecision]) -> int:
    """How many code blocks fall inside a run that is already announced.

    Reported so the log reconciles on its own: `code` counts blocks the classifier called
    code, and only `code - suppressed` of them are ever spoken. Without it the tally and the
    listener's experience differ by a factor of 13 on a line-split book and nothing in the
    log says why.

    Computed over the same page/decision order `segment_units` walks, so the arithmetic
    matches the segments it will emit — but it is an upper bound on what the collapse saves
    where a unit between two code blocks speaks nothing, which is the disclosed boundary in
    PIPELINE_SPEC §5.1.
    """
    suppressed = 0
    open_page: int | None = None
    for page_no, decision in zip(pages, decisions, strict=True):
        if not decision.is_code:
            open_page = None
            continue
        if open_page == page_no:
            suppressed += 1
            continue
        open_page = page_no
    return suppressed


def format_cue_tally(pages: list[int], decisions: list[CodeDecision]) -> str:
    blocks = len(decisions)
    by_label = sum(1 for d in decisions if d.is_code and d.decided_by == DECIDED_BY_LABEL)
    by_heuristic = sum(1 for d in decisions if d.is_code and d.decided_by == DECIDED_BY_HEURISTIC)
    unlabelled = sum(1 for d in decisions if d.decided_by == DECIDED_BY_HEURISTIC)
    suppressed = suppressed_by_collapse(pages, decisions)
    return (
        f"script cues: blocks={blocks} code={by_label + by_heuristic} "
        f"by=label:{by_label}/heuristic:{by_heuristic} unlabelled={unlabelled}/{blocks} "
        f"suppressed={suppressed}"
    )
