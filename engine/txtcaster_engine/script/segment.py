from collections.abc import Iterator
from dataclasses import dataclass, field

import pysbd

from txtcaster_engine.script.assemble import SourceUnit
from txtcaster_engine.script.cues import CODE_KIND, cue_opener, decide_code
from txtcaster_engine.script.normalize import MATH_DROPPED_FLAG, NormalizedText, normalize_text

MAX_SEGMENT_CHARS = 280
CODE_SUMMARY = cue_opener(CODE_KIND)
CODE_BLOCK_FLAG = "code_block"

_SEGMENTER = pysbd.Segmenter(language="en", clean=False)


@dataclass
class ScriptSegment:
    idx: int
    chapter_idx: int
    page_no: int
    kind: str
    text: str
    source_span: dict[str, int]
    flags: list[str] = field(default_factory=list)
    figure_id: str | None = None

    def to_json(self) -> dict[str, object]:
        result: dict[str, object] = {
            "idx": self.idx,
            "chapter_idx": self.chapter_idx,
            "page_no": self.page_no,
            "kind": self.kind,
            "text": self.text,
            "source_span": dict(self.source_span),
            "flags": list(self.flags),
        }
        if self.figure_id is not None:
            result["figure_id"] = self.figure_id
        return result


def _split_long(sentence: str) -> list[str]:
    if len(sentence) <= MAX_SEGMENT_CHARS:
        return [sentence]
    parts: list[str] = []
    remaining = sentence
    while len(remaining) > MAX_SEGMENT_CHARS:
        window = remaining[:MAX_SEGMENT_CHARS]
        split_at = max(window.rfind(", "), window.rfind("; "), window.rfind(" "))
        if split_at <= 0:
            split_at = MAX_SEGMENT_CHARS
            head, remaining = remaining[:split_at], remaining[split_at:]
        else:
            head, remaining = remaining[:split_at].rstrip(" ,;"), remaining[split_at:].lstrip()
        parts.append(head)
    if remaining:
        parts.append(remaining)
    return parts


def _sentence_chunks(text: str) -> Iterator[tuple[str, int, int, int, int]]:
    search_from = 0
    for sentence in _SEGMENTER.segment(text):
        trimmed = sentence.strip()
        if not trimmed:
            continue
        sentence_start = text.find(trimmed, search_from)
        if sentence_start < 0:
            sentence_start = text.find(trimmed)
        sentence_end = sentence_start + len(trimmed)
        search_from = sentence_end
        cursor = sentence_start
        for chunk in _split_long(trimmed):
            chunk_text = chunk.strip()
            if not chunk_text:
                continue
            chunk_start = text.find(chunk_text, cursor)
            if chunk_start < 0:
                chunk_start = sentence_start
            chunk_end = chunk_start + len(chunk_text)
            cursor = chunk_end
            yield chunk_text, chunk_start, chunk_end, sentence_start, sentence_end


def _tile_spans(normalized: NormalizedText, starts: list[int], total: int) -> list[tuple[int, int]]:
    tiled: list[int] = []
    cursor = -1
    for position, local_start in enumerate(starts):
        if position == 0:
            start = 0
        else:
            room = max(total - (len(starts) - position), cursor + 1)
            start = min(max(normalized.source_offset(local_start), cursor + 1), room)
        tiled.append(start)
        cursor = start
    return list(zip(tiled, [*tiled[1:], total], strict=True))


def _source_span(unit: SourceUnit, char_start: int, char_end: int) -> dict[str, int]:
    span = {"page": unit.page_no, "char_start": char_start, "char_end": char_end}
    if unit.block_index is not None:
        span["block_index"] = unit.block_index
    return span


def _announcement_still_open(announced: tuple[int, int] | None, page_no: int, emitted: int) -> bool:
    """Is this page's code cue still the last thing the listener heard?

    One listing is one announcement. A PDF that emits one block per source line turns a
    37-line listing into 37 blocks, and cueing each of them says "A code block appears
    here." 37 times in a row — the verbosity the user reported, amplified rather than
    fixed by a classifier that finds more listings. Measured on
    `C++ Concurrency in Action 2e`: 7 152 code blocks fall into 549 page-bounded runs, so
    collapsing takes spoken mentions from 2 913 to 549 while destroying less prose.

    The state is `(page, segments emitted at the time of the cue)` and **not** a flag read
    back off the last segment. `base_flags` is prepended to every segment this function
    emits, so a caller passing `code_block` in `flags` would make every prose segment look
    like a code cue and silently suppress the real ones — a caller-injectable value can
    never be the identity of the thing under test. The emitted count answers the same
    question and nothing outside this function can set it: unchanged means nothing has been
    spoken since, so a block that produces no speech cannot break a run either.

    It is bounded to one page because a segment cannot span pages (DATA_MODELS §2.4e); the
    longest unbounded run here is 147 blocks across several pages, and a cue anchored to a
    page it does not sit on has no geometry to highlight.
    """
    return announced == (page_no, emitted)


def segment_units(units: list[SourceUnit], flags: list[str] | None = None) -> list[ScriptSegment]:
    base_flags = list(flags or [])
    segments: list[ScriptSegment] = []
    idx = 0
    announced: tuple[int, int] | None = None
    for unit in units:
        if unit.kind == "figure":
            normalized = normalize_text(unit.text)
            cue_flags = list(base_flags)
            if normalized.math_dropped:
                cue_flags.append(MATH_DROPPED_FLAG)
            chunks = list(_sentence_chunks(normalized.text))
            if chunks:
                texts = [chunk[0] for chunk in chunks]
                spans = _tile_spans(normalized, [chunk[1] for chunk in chunks], len(unit.text))
            else:
                texts = [normalized.text.strip() or unit.text]
                spans = [(0, len(unit.text))]
            pending_figure_id = unit.figure_id
            pending_flags = cue_flags
            for text, (char_start, char_end) in zip(texts, spans, strict=True):
                segments.append(
                    ScriptSegment(
                        idx=idx,
                        chapter_idx=unit.chapter_idx,
                        page_no=unit.page_no,
                        kind="figure_cue",
                        text=text,
                        source_span=_source_span(unit, char_start, char_end),
                        flags=pending_flags,
                        figure_id=pending_figure_id,
                    )
                )
                pending_figure_id = None
                pending_flags = list(base_flags)
                idx += 1
            continue
        if unit.kind == "heading":
            normalized = normalize_text(unit.text)
            text = normalized.text.strip()
            if not text:
                continue
            segments.append(
                ScriptSegment(
                    idx=idx,
                    chapter_idx=unit.chapter_idx,
                    page_no=unit.page_no,
                    kind="heading",
                    text=text,
                    source_span=_source_span(unit, 0, len(unit.text)),
                    flags=list(base_flags),
                )
            )
            idx += 1
            continue
        if decide_code(unit.label, unit.text).is_code:
            if _announcement_still_open(announced, unit.page_no, len(segments)):
                continue
            segments.append(
                ScriptSegment(
                    idx=idx,
                    chapter_idx=unit.chapter_idx,
                    page_no=unit.page_no,
                    kind="text",
                    text=CODE_SUMMARY,
                    source_span=_source_span(unit, 0, len(unit.text)),
                    flags=[*base_flags, CODE_BLOCK_FLAG],
                )
            )
            idx += 1
            announced = (unit.page_no, len(segments))
            continue
        normalized = normalize_text(unit.text)
        pending_figure_id = unit.figure_id
        for chunk_text, _, _, sentence_start, sentence_end in _sentence_chunks(normalized.text):
            char_start = normalized.source_offset(sentence_start)
            char_end = normalized.source_offset(sentence_end - 1) + 1
            segments.append(
                ScriptSegment(
                    idx=idx,
                    chapter_idx=unit.chapter_idx,
                    page_no=unit.page_no,
                    kind="text",
                    text=chunk_text,
                    source_span=_source_span(unit, char_start, char_end),
                    flags=list(base_flags),
                    figure_id=pending_figure_id,
                )
            )
            pending_figure_id = None
            idx += 1
    return segments
