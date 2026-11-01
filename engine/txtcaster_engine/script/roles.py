import logging
from dataclasses import replace

from txtcaster_engine.ingest.blocks import TextBlock
from txtcaster_engine.ingest.headers import apply_roles, detect_repeated_regions

logger = logging.getLogger(__name__)

SUPPRESSED_ROLES = frozenset({"header", "footer"})
MIN_SECTION_PAGES = 2


def apply_union_roles(
    pages: list[list[TextBlock]],
    heights: list[float],
    section_ranges: list[tuple[int, int]],
) -> None:
    added = 0
    repeated = detect_repeated_regions(pages, heights)
    for blocks, height in zip(pages, heights, strict=True):
        apply_roles(blocks, height, repeated)

    for start_index, end_index in section_ranges:
        window = pages[start_index:end_index]
        window_heights = heights[start_index:end_index]
        if len(window) < MIN_SECTION_PAGES:
            continue
        shadow = [[replace(block) for block in blocks] for blocks in window]
        section_repeated = detect_repeated_regions(shadow, window_heights)
        for blocks, height in zip(shadow, window_heights, strict=True):
            apply_roles(blocks, height, section_repeated)
        for live_blocks, shadow_blocks in zip(window, shadow, strict=True):
            for live, marked in zip(live_blocks, shadow_blocks, strict=True):
                if live.role == "body" and marked.role in SUPPRESSED_ROLES:
                    live.role = marked.role
                    added += 1

    wide = sum(1 for page in pages for block in page if block.role in SUPPRESSED_ROLES) - added
    if section_ranges:
        logger.info(
            "roles: %d header/footer block(s) from the book-wide pass, %d more from %d "
            "declared section(s)",
            wide,
            added,
            len(section_ranges),
        )
    else:
        logger.info(
            "roles: %d header/footer block(s) from the book-wide pass; no declared "
            "structure, so no section pass ran",
            wide,
        )
