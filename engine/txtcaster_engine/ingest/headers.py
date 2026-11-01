import re
from collections import Counter

from txtcaster_engine.ingest.blocks import FURNITURE_BAND_FRACTION, TextBlock
from txtcaster_engine.script.shape import reads_as_sentence

BAND_FRACTION = FURNITURE_BAND_FRACTION
REPEAT_MIN_FRACTION = 0.6
MIN_PAGES_FOR_DETECTION = 3

ROLE_BY_LABEL = {
    "page_header": "header",
    "page_footer": "footer",
    "artifact_header": "header",
    "artifact_footer": "footer",
}
"""What the layout model reported, mapped to what we do with it.

A label outranks the text heuristic: it comes from a model that looked at the page,
whereas the heuristic infers furniture from repetition across pages and cannot reach a
running head that appears once, or one whose wording changes every section.
"""


def _normalize(text: str) -> str:
    return re.sub(r"\d+", "#", " ".join(text.split())).lower()


def _band_of(block: TextBlock, page_height: float) -> str | None:
    if page_height <= 0:
        return None
    top_limit = page_height * BAND_FRACTION
    bottom_limit = page_height * (1 - BAND_FRACTION)
    if block.bbox[3] <= top_limit:
        return "header"
    if block.bbox[1] >= bottom_limit:
        return "footer"
    return None


def detect_repeated_regions(
    pages: list[list[TextBlock]], page_heights: list[float]
) -> dict[str, set[str]]:
    if len(pages) < MIN_PAGES_FOR_DETECTION:
        return {"header": set(), "footer": set()}
    counts: dict[str, Counter[str]] = {"header": Counter(), "footer": Counter()}
    sides: list[dict[str, Counter[str]]] = [
        {"header": Counter(), "footer": Counter()},
        {"header": Counter(), "footer": Counter()},
    ]
    numbers: dict[tuple[str, str], list[tuple[int, tuple[int, ...]]]] = {}
    for index, (blocks, height) in enumerate(zip(pages, page_heights, strict=True)):
        seen: dict[str, set[str]] = {"header": set(), "footer": set()}
        for block in blocks:
            band = _band_of(block, height)
            if band is not None and _normalize(block.text) not in seen[band]:
                seen[band].add(_normalize(block.text))
                found = tuple(int(n) for n in re.findall(r"\d+", block.text))
                numbers.setdefault((band, _normalize(block.text)), []).append((index, found))
        for band, texts in seen.items():
            counts[band].update(texts)
            sides[index % 2][band].update(texts)
    threshold = len(pages) * REPEAT_MIN_FRACTION
    repeated = {
        band: {text for text, count in counter.items() if count >= threshold}
        for band, counter in counts.items()
    }
    side_pages = [(len(pages) + 1) // 2, len(pages) // 2]
    for band in repeated:
        one_sided = [
            {
                text
                for text in _frequent(sides[side][band], side_pages[side]) - repeated[band]
                if _numbers_hold_or_track_the_page(
                    [seen for seen in numbers[(band, text)] if seen[0] % 2 == side]
                )
            }
            for side in (0, 1)
        ]
        if all(one_sided):
            repeated[band] |= one_sided[0] | one_sided[1]
    return repeated


def _numbers_hold_or_track_the_page(seen: list[tuple[int, tuple[int, ...]]]) -> bool:
    steady = 0
    for (page, found), (next_page, next_found) in zip(seen, seen[1:], strict=False):
        if all(b - a in (0, next_page - page) for a, b in zip(found, next_found, strict=False)):
            steady += 1
    return steady * 2 >= len(seen) - 1


def _frequent(counter: Counter[str], pages: int) -> set[str]:
    if pages < MIN_PAGES_FOR_DETECTION:
        return set()
    threshold = max(pages * REPEAT_MIN_FRACTION, MIN_PAGES_FOR_DETECTION)
    return {text for text, count in counter.items() if count >= threshold}


def apply_roles(blocks: list[TextBlock], page_height: float, repeated: dict[str, set[str]]) -> None:
    for block in blocks:
        labelled = ROLE_BY_LABEL.get(block.label or "")
        if labelled is not None:
            block.role = labelled
            continue
        band = _band_of(block, page_height)
        if band is None or _normalize(block.text) not in repeated[band]:
            continue
        if reads_as_sentence(block.text):
            # Repetition in a band is weak evidence on its own — a running head repeats,
            # but so does a paragraph that happens to sit at the top of every page in a
            # dense layout. When the only evidence is repetition and the text reads as
            # prose, narrate it: a running head left in is audible and reportable, while
            # a deleted sentence is silent and, with no telemetry, permanent. A layout
            # label overrides this, because that model looked at the page.
            continue
        block.role = band
