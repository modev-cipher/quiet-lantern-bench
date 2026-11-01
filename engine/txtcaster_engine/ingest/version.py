from txtcaster_engine.ingest.reflow import (
    REFLOW_FONT_SIZE,
    REFLOW_LAYOUT_VERSION,
    REFLOW_PAGE_HEIGHT,
    REFLOW_PAGE_WIDTH,
)

EXTRACT_ALGO_VERSION = 8
"""Bump whenever stored block output changes.

3 harvests every layout label instead of `page_header`/`page_footer` only, so a book
extracted at 2 carries no `code` label and scripting decides its cues with a text
heuristic measured at 0.727 precision. The version is what tells an already-imported book
it is behind: `library::generation_is_current` compares it against the base the engine
reports on `/health`, and a stale book is offered a re-read rather than being told it was
already analysed. Nothing re-extracts on its own — a 923-page pass is 3.5 minutes on a GPU
and 2.9 hours on CPU, which is a decision, not a spinner.
"""

GEOMETRY_ALGO_VERSION = 1
ROLE_ALGO_VERSION = 2
"""Bump whenever role assignment changes.

Roles are deliberately not persisted, but things *derived* from them are: chapter word
counts, the language sample, and pseudo-chapter boundaries. A stored derivative of an
unpersisted computation has to be versioned by that computation's algorithm, or changing
it leaves every already-imported book with stale numbers and nothing to invalidate them.
Measured when this was missed: a 613-page book overstated its narration by 3141 words.
"""

DEFAULT_SCALE = 3.0
"""Raster scale for page PNGs, and part of the geometry stamp.

`page_cache_key` already carries the scale, so a change mints new cache keys — but
`page.render_hash` still points at the *old* key and that file still exists, so the reader
would find an `image_path`, skip the page, and serve the old-scale PNG forever. The stamp is
the only thing checked at both ends, so the scale has to live in it or a scale change never
reaches an already-imported book. Measured when this was missed: a 1008px page served into a
1500px reading column, upscaled 1.44x, which is the blur the scale exists to prevent.
"""


def render_identity() -> str:
    reflow = f"{REFLOW_PAGE_WIDTH}x{REFLOW_PAGE_HEIGHT}@{REFLOW_FONT_SIZE}/{REFLOW_LAYOUT_VERSION}"
    from txtcaster_engine.ingest.pdfium_adapter import engine_identity

    return f"{engine_identity()}|reflow={reflow}"


def geometry_version() -> str:
    return f"v1|geom={GEOMETRY_ALGO_VERSION}|scale={DEFAULT_SCALE:g}|{render_identity()}"


def extractor_base_version() -> str:
    return f"v1|extract={EXTRACT_ALGO_VERSION}|roles={ROLE_ALGO_VERSION}|{render_identity()}"


def extractor_version(docling_artifacts_sha: str | None) -> str:
    sha = (docling_artifacts_sha or "none")[:16]
    return f"{extractor_base_version()}|docling={sha}"
