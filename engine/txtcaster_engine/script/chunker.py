from dataclasses import dataclass, field, replace

from txtcaster_engine.script.assemble import SourceUnit

CTX_BUDGET_FRACTION = 0.6
CHARS_PER_TOKEN = 4
MIN_CHUNK_CHARS = 512


@dataclass
class Chunk:
    idx: int
    chapter_idx: int
    page_no: int
    units: list[SourceUnit] = field(default_factory=list)

    def text(self) -> str:
        return "\n\n".join(unit.text for unit in self.units)


def chunk_budget_chars(ctx: int) -> int:
    return max(MIN_CHUNK_CHARS, int(ctx * CTX_BUDGET_FRACTION) * CHARS_PER_TOKEN)


def _unit_cost(unit: SourceUnit) -> int:
    return len(unit.text) + 2


def _split_oversized(unit: SourceUnit, budget: int) -> list[SourceUnit]:
    if len(unit.text) <= budget or unit.kind == "figure":
        return [unit]
    pieces: list[SourceUnit] = []
    remaining = unit.text
    while len(remaining) > budget:
        window = remaining[:budget]
        split_at = max(window.rfind(". "), window.rfind("\n"), window.rfind(" "))
        if split_at <= 0:
            split_at = budget
        head = remaining[: split_at + 1].strip()
        remaining = remaining[split_at + 1 :].strip()
        if head:
            pieces.append(replace(unit, text=head))
    if remaining:
        pieces.append(replace(unit, text=remaining))
    return pieces


def chunk_units(units: list[SourceUnit], ctx: int, budget_chars: int | None = None) -> list[Chunk]:
    budget = budget_chars if budget_chars is not None else chunk_budget_chars(ctx)
    chunks: list[Chunk] = []
    current: list[SourceUnit] = []
    current_cost = 0

    def flush() -> None:
        nonlocal current, current_cost
        if not current:
            return
        chunks.append(
            Chunk(
                idx=len(chunks),
                chapter_idx=current[0].chapter_idx,
                page_no=current[0].page_no,
                units=current,
            )
        )
        current = []
        current_cost = 0

    for unit in units:
        chapter_boundary = bool(current and unit.chapter_idx != current[0].chapter_idx)
        section_boundary = unit.kind == "heading"
        if (chapter_boundary or section_boundary) and current:
            flush()
        for piece in _split_oversized(unit, budget):
            cost = _unit_cost(piece)
            if current and current_cost + cost > budget:
                flush()
            current.append(piece)
            current_cost += cost
    flush()
    return chunks
