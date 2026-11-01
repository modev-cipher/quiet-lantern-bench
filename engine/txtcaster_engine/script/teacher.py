import json
import logging
import re
from collections.abc import Callable
from typing import Protocol

from txtcaster_engine.ingest.errors import IngestCancelled
from txtcaster_engine.oom import is_oom
from txtcaster_engine.script.assemble import SourceUnit, figure_prompt_block
from txtcaster_engine.script.chunker import CHARS_PER_TOKEN, Chunk, chunk_units
from txtcaster_engine.script.cues import decide_code
from txtcaster_engine.script.segment import ScriptSegment, segment_units

logger = logging.getLogger(__name__)


class GenerateFn(Protocol):
    def __call__(self, prompt: str, max_tokens: int, temperature: float = 0.0) -> str: ...


NARRATOR_FALLBACK_FLAG = "fallback_narrator"
MAX_ATTEMPTS = 3
RETRY_TEMPERATURE = 0.3
MIN_OUTPUT_TOKENS = 512
CTX_SAFETY_TOKENS = 64
TEACHER_OUTPUT_RATIO = 1.5
TEACHER_PROMPT_OVERHEAD_TOKENS = 800
MIN_TEACHER_CHUNK_TOKENS = 128
MAX_TEACHER_CHUNK_CHARS = 6000
SUMMARY_MAX_TOKENS = 400
SUMMARY_MAX_CHARS = SUMMARY_MAX_TOKENS * 4
MARKER_DETAIL_MAX_CHARS = 500

TEACHER_PERSONA = (
    "You are a patient teacher recording an audio lecture. Re-explain the source "
    "material below as a clear spoken lecture: simplify the language, explain each "
    "key concept, and add one short contextual example per key concept. Keep poetry "
    "and quotations verbatim. Never introduce facts, numbers, names, or claims that "
    "are not present in the source. For text between [CODE] and [/CODE], briefly "
    "describe what the code does in one sentence; never read code aloud character "
    "by character."
)
FIGURE_RULE = (
    "Each [FIGURE:id] block gives a figure's type, caption and description. Teach that "
    "figure in your own words as part of the lecture. Preserve every [FIGURE:id] marker "
    "exactly once, immediately before the sentence where you begin explaining it."
)
OUTPUT_RULE = (
    'Output ONLY a JSON array of objects, each with a "text" field '
    'containing one spoken paragraph: [{"text": "..."}]'
)
TEACHER_SYSTEM = f"{TEACHER_PERSONA} {FIGURE_RULE} {OUTPUT_RULE}"
STRICT_SUFFIX = (
    "\nYour previous answer was not valid JSON. Respond with NOTHING except a valid "
    'JSON array of objects with a "text" string field. No prose, no code fences.'
)
MARKER_SUFFIX = (
    "\nYour previous answer misused figure markers ({detail}). Each marker from the "
    "source may appear AT MOST once, copied exactly; never invent new markers."
)
SUMMARY_PROMPT = (
    "Summarize the lecture so far in plain text, at most 300 words, keeping the key "
    "concepts and terminology so the next part of the lecture stays consistent.\n\n"
    "Previous summary:\n{summary}\n\nLatest lecture part:\n{lecture}\n\nSummary:"
)

_FENCE_RE = re.compile(r"^```[a-zA-Z]*\n|\n?```$")
_MARKER_RE = re.compile(r"\[FIGURE:([^\]]+)]")
_MARKER_LIKE_RE = re.compile(r"\[\s*FIGURE\b", re.IGNORECASE)


def teacher_chunk_budget_chars(ctx: int) -> int:
    usable = ctx - TEACHER_PROMPT_OVERHEAD_TOKENS - CTX_SAFETY_TOKENS
    source_tokens = int(usable / (1 + TEACHER_OUTPUT_RATIO))
    return min(
        MAX_TEACHER_CHUNK_CHARS,
        max(MIN_TEACHER_CHUNK_TOKENS, source_tokens) * CHARS_PER_TOKEN,
    )


def output_budget_tokens(source_chars: int, prompt_chars: int, ctx: int) -> int:
    source_tokens = source_chars // CHARS_PER_TOKEN
    desired = max(MIN_OUTPUT_TOKENS, int(source_tokens * TEACHER_OUTPUT_RATIO))
    prompt_tokens = prompt_chars // CHARS_PER_TOKEN
    remaining = ctx - prompt_tokens - CTX_SAFETY_TOKENS
    return max(1, min(desired, remaining))


def build_prompt(
    summary: str,
    source: str,
    strict: bool,
    marker_detail: str = "",
    has_figures: bool = True,
) -> str:
    rules = [TEACHER_PERSONA]
    if has_figures:
        rules.append(FIGURE_RULE)
    rules.append(OUTPUT_RULE)
    parts = [" ".join(rules)]
    if strict:
        if marker_detail:
            parts.append(MARKER_SUFFIX.format(detail=marker_detail))
        else:
            parts.append(STRICT_SUFFIX)
    if summary:
        parts.append(f"\nCONTEXT SUMMARY (previous material):\n{summary}")
    parts.append(f"\nSOURCE:\n{source}\n\nJSON:")
    return "\n".join(parts)


def parse_lecture(raw: str) -> list[str] | None:
    cleaned = _FENCE_RE.sub("", raw.strip()).strip()
    start = cleaned.find("[")
    end = cleaned.rfind("]")
    if start < 0 or end <= start:
        return None
    try:
        data = json.loads(cleaned[start : end + 1])
    except json.JSONDecodeError:
        return None
    if not isinstance(data, list) or not data:
        return None
    texts: list[str] = []
    for item in data:
        if not isinstance(item, dict):
            return None
        text = item.get("text")
        if not isinstance(text, str) or not text.strip():
            return None
        texts.append(text.strip())
    return texts


def chunk_source(chunk: Chunk) -> str:
    parts: list[str] = []
    for unit in chunk.units:
        if unit.kind == "text" and decide_code(unit.label, unit.text).is_code:
            parts.append(f"[CODE]\n{unit.text}\n[/CODE]")
        elif unit.kind == "figure" and unit.figure_id is not None:
            parts.append(f"[FIGURE:{unit.figure_id}]\n{figure_prompt_block(unit)}")
        else:
            parts.append(unit.text)
    return "\n\n".join(parts)


_PERIPHERY_RE = re.compile(r"^(?:page\s+)?\d+$|^chapter\s+\d+$", re.IGNORECASE)


def _substantive_source_units(units: list[SourceUnit]) -> list[SourceUnit]:
    substantive: list[SourceUnit] = []
    for u in units:
        if u.kind == "heading" and u.block_index is not None and u.text.strip():
            substantive.append(u)
        elif u.kind == "text" and u.block_index is not None:
            text = u.text.strip()
            if len(text) < 25 and _PERIPHERY_RE.match(text):
                continue
            if u.label in ("header", "footer", "page_number", "footnote"):
                continue
            substantive.append(u)
    if not substantive:
        substantive = [u for u in units if u.block_index is not None]
    return substantive


def _substantive_topic_block(units: list[SourceUnit]) -> int | None:
    for u in units:
        if u.kind == "heading" and u.block_index is not None and u.text.strip():
            return u.block_index
    for u in units:
        if u.kind == "text" and u.block_index is not None:
            text = u.text.strip()
            if len(text) < 25 and _PERIPHERY_RE.match(text):
                continue
            if u.label in ("header", "footer", "page_number", "footnote"):
                continue
            if len(text) >= 25 or len(text.split()) >= 5:
                return u.block_index
    for u in units:
        if u.block_index is not None:
            return u.block_index
    return None


def _lecture_units(chunk: Chunk, texts: list[str]) -> list[SourceUnit]:
    figure_pages = {
        unit.figure_id: unit.page_no
        for unit in chunk.units
        if unit.kind == "figure" and unit.figure_id is not None
    }
    known = set(figure_pages.keys())
    queued: set[str] = set()
    pending: list[str] = []

    raw_pieces: list[tuple[str, str | None]] = []
    for text in texts:
        cursor = 0
        for match in _MARKER_RE.finditer(text):
            before = text[cursor : match.start()].strip()
            if before:
                assigned = pending.pop(0) if pending else None
                raw_pieces.append((before, assigned))
            figure_id = match.group(1)
            if figure_id in known and figure_id not in queued:
                queued.add(figure_id)
                pending.append(figure_id)
            cursor = match.end()
        after = text[cursor:].strip()
        if after:
            assigned = pending.pop(0) if pending else None
            raw_pieces.append((after, assigned))

    if pending and raw_pieces and raw_pieces[-1][1] is None:
        raw_pieces[-1] = (raw_pieces[-1][0], pending.pop(0))

    if not raw_pieces:
        return []

    fig_positions: dict[str, int] = {
        u.figure_id: idx
        for idx, u in enumerate(chunk.units)
        if u.kind == "figure" and u.figure_id is not None
    }
    all_substantive = _substantive_source_units(chunk.units)
    fallback_block = _substantive_topic_block(chunk.units)

    MAX_FIGURE_HOLD_SENTENCES = 4
    units: list[SourceUnit] = []
    current_slice_start = 0

    i = 0
    while i < len(raw_pieces):
        text, assigned_fig = raw_pieces[i]
        if assigned_fig is not None and assigned_fig in figure_pages:
            fig_idx = fig_positions.get(assigned_fig, current_slice_start)
            fig_page = figure_pages[assigned_fig]
            nearby_block = None
            for offset in (1, -1, 2, -2, 0):
                target_idx = fig_idx + offset
                if 0 <= target_idx < len(chunk.units):
                    u = chunk.units[target_idx]
                    if u.block_index is not None:
                        nearby_block = u.block_index
                        break
            if nearby_block is None:
                nearby_block = fallback_block

            unit = SourceUnit(chunk.chapter_idx, fig_page, "text", text)
            unit.figure_id = assigned_fig
            unit.block_index = nearby_block
            units.append(unit)

            hold_remaining = MAX_FIGURE_HOLD_SENTENCES - 1
            current_slice_start = fig_idx + 1
            i += 1

            while i < len(raw_pieces) and hold_remaining > 0 and raw_pieces[i][1] is None:
                hold_text, _ = raw_pieces[i]
                unit = SourceUnit(chunk.chapter_idx, fig_page, "text", hold_text)
                unit.block_index = nearby_block
                units.append(unit)
                hold_remaining -= 1
                i += 1
        else:
            run_start = i
            while i < len(raw_pieces) and raw_pieces[i][1] is None:
                i += 1
            run_pieces = raw_pieces[run_start:i]

            next_fig = raw_pieces[i][1] if i < len(raw_pieces) else None
            slice_end = fig_positions[next_fig] if next_fig in fig_positions else len(chunk.units)

            slice_source = chunk.units[current_slice_start:slice_end]
            substantive = _substantive_source_units(slice_source) or all_substantive

            for k, (piece_text, _) in enumerate(run_pieces):
                if substantive:
                    sub_idx = min(len(substantive) - 1, int(k * len(substantive) / len(run_pieces)))
                    src = substantive[sub_idx]
                    page_no = src.page_no
                    block_idx = src.block_index
                else:
                    page_no = chunk.page_no
                    block_idx = fallback_block

                unit = SourceUnit(chunk.chapter_idx, page_no, "text", piece_text)
                unit.block_index = block_idx
                units.append(unit)
            current_slice_start = slice_end

    return units


def anchored_segments(chunk: Chunk, texts: list[str]) -> list[ScriptSegment]:
    segments = segment_units(_lecture_units(chunk, texts))
    if not segments:
        return segment_units(chunk.units, flags=[NARRATOR_FALLBACK_FLAG])
    return segments


def _marker_problems(markers: list[str], figure_ids: set[str], mangled: int) -> str:
    problems: list[str] = []
    seen: set[str] = set()
    unknown = 0
    for marker in markers:
        if marker in figure_ids:
            if marker in seen:
                problems.append(f"[FIGURE:{marker}] appears more than once")
            seen.add(marker)
        else:
            unknown += 1
    if unknown:
        problems.append(f"{unknown} marker(s) are not markers from the source")
    if mangled:
        problems.append(f"{mangled} marker(s) have broken [FIGURE:id] syntax")
    return "; ".join(dict.fromkeys(problems))[:MARKER_DETAIL_MAX_CHARS]


def _generate_lecture(
    generate: GenerateFn, summary: str, source: str, ctx: int, figure_ids: set[str]
) -> tuple[list[str], int, int] | None:
    marker_detail = ""
    for attempt in range(MAX_ATTEMPTS):
        prompt = build_prompt(
            summary,
            source,
            strict=attempt > 0,
            marker_detail=marker_detail,
            has_figures=bool(figure_ids),
        )
        budget = output_budget_tokens(len(source), len(prompt), ctx)
        temperature = 0.0 if attempt == 0 else RETRY_TEMPERATURE
        try:
            raw = generate(prompt, budget, temperature)
        except Exception as exc:
            if is_oom(exc):
                raise
            logger.warning(
                "teacher generation failed (attempt %d/%d): %s", attempt + 1, MAX_ATTEMPTS, exc
            )
            continue
        texts = parse_lecture(raw)
        if texts is None:
            marker_detail = ""
            logger.warning(
                "teacher output malformed (attempt %d/%d): %d chars against a %d token budget",
                attempt + 1,
                MAX_ATTEMPTS,
                len(raw),
                budget,
            )
            continue
        markers = [match for text in texts for match in _MARKER_RE.findall(text)]
        known = [marker for marker in markers if marker in figure_ids]
        mangled = sum(len(_MARKER_LIKE_RE.findall(text)) for text in texts) - len(markers)
        if mangled <= 0:
            dropped = len(markers) - len(set(known))
            if dropped:
                logger.warning(
                    "teacher output had %d unusable figure marker(s); assembly dropped "
                    "them and kept the lecture",
                    dropped,
                )
            omitted = len(figure_ids - set(known))
            if omitted:
                logger.info(
                    "teacher output omitted %d of %d figure marker(s); each is spoken "
                    "once as a cue at the chunk end and no retry is burned",
                    omitted,
                    len(figure_ids),
                )
            return texts, dropped, omitted
        marker_detail = _marker_problems(markers, figure_ids, max(0, mangled))
        logger.warning(
            "teacher output has %d marker(s) with broken syntax (attempt %d/%d)",
            max(0, mangled),
            attempt + 1,
            MAX_ATTEMPTS,
        )
    return None


def _update_summary(generate: GenerateFn, summary: str, lecture: str, ctx: int) -> str:
    prompt = SUMMARY_PROMPT.format(summary=summary or "(none)", lecture=lecture)
    remaining = ctx - len(prompt) // CHARS_PER_TOKEN - CTX_SAFETY_TOKENS
    budget = max(1, min(SUMMARY_MAX_TOKENS, remaining))
    try:
        updated = generate(prompt, budget).strip()
    except Exception as exc:
        if is_oom(exc):
            raise
        logger.warning("teacher summary update failed, keeping previous summary: %s", exc)
        return summary
    if not updated:
        return summary
    return updated[:SUMMARY_MAX_CHARS]


def _split_chunk(chunk: Chunk) -> list[Chunk]:
    middle = len(chunk.units) // 2
    return [
        Chunk(
            idx=chunk.idx,
            chapter_idx=chunk.chapter_idx,
            page_no=chunk.page_no,
            units=units,
        )
        for units in (chunk.units[:middle], chunk.units[middle:])
    ]


def teach_units(
    units: list[SourceUnit],
    ctx: int,
    generate: GenerateFn,
    on_chunk: Callable[[int, int], None] | None = None,
    is_cancelled: Callable[[], bool] | None = None,
    on_markers_dropped: Callable[[int], None] | None = None,
    on_markers_omitted: Callable[[int], None] | None = None,
) -> list[ScriptSegment]:
    chunks = chunk_units(units, ctx, budget_chars=teacher_chunk_budget_chars(ctx))
    total = len(chunks)
    if on_chunk is not None:
        on_chunk(0, total)
    segments: list[ScriptSegment] = []
    summary = ""
    for done, chunk in enumerate(chunks, start=1):
        if is_cancelled is not None and is_cancelled():
            raise IngestCancelled()
        figure_ids = {
            unit.figure_id
            for unit in chunk.units
            if unit.kind == "figure" and unit.figure_id is not None
        }
        lecture = _generate_lecture(generate, summary, chunk_source(chunk), ctx, figure_ids)
        if lecture is None and len(chunk.units) > 1:
            halves = _split_chunk(chunk)
            logger.info(
                "teacher retrying chunk %d/%d as %d smaller chunks before giving up",
                done,
                total,
                len(halves),
            )
            recovered: list[ScriptSegment] = []
            recovered_texts: list[str] = []
            for half in halves:
                half_ids = {
                    unit.figure_id
                    for unit in half.units
                    if unit.kind == "figure" and unit.figure_id is not None
                }
                part = _generate_lecture(generate, summary, chunk_source(half), ctx, half_ids)
                if part is None:
                    recovered.extend(segment_units(half.units, flags=[NARRATOR_FALLBACK_FLAG]))
                    continue
                part_texts, part_dropped, part_omitted = part
                if part_dropped and on_markers_dropped is not None:
                    on_markers_dropped(part_dropped)
                if part_omitted and on_markers_omitted is not None:
                    on_markers_omitted(part_omitted)
                recovered.extend(anchored_segments(half, part_texts))
                recovered_texts.extend(part_texts)
            segments.extend(recovered)
            if recovered_texts and done < total:
                summary = _update_summary(generate, summary, "\n\n".join(recovered_texts), ctx)
        elif lecture is None:
            segments.extend(segment_units(chunk.units, flags=[NARRATOR_FALLBACK_FLAG]))
        else:
            texts, dropped, omitted = lecture
            if dropped and on_markers_dropped is not None:
                on_markers_dropped(dropped)
            if omitted and on_markers_omitted is not None:
                on_markers_omitted(omitted)
            segments.extend(anchored_segments(chunk, texts))
            if done < total:
                summary = _update_summary(generate, summary, "\n\n".join(texts), ctx)
        if on_chunk is not None:
            on_chunk(done, total)
    for idx, segment in enumerate(segments):
        segment.idx = idx
    return segments
