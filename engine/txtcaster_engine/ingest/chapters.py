from dataclasses import dataclass

from txtcaster_engine.ingest.blocks import TextBlock

PSEUDO_CHAPTER_PAGES = 10
HEADING_FONT_RATIO = 1.3
HEADING_MAX_WORDS = 12
MAX_HEURISTIC_CHAPTERS = 200
FONT_SIZE_BUCKET_PT = 0.5
MIN_TOC_CHAPTERS = 3
SPARSE_CHAPTER_WORDS = 50
MAX_SPARSE_RATIO = 1 / 3
MIN_MEDIAN_CHAPTER_WORDS = 750
MAX_TOC_CHAPTERS = 200
MAX_TOC_DEPTH = 32
COARSE_SHARE = 0.3
COARSE_CHAPTER_WORDS = 20_000


@dataclass
class Chapter:
    idx: int
    title: str
    page_start: int
    page_end: int
    word_count: int
    is_pseudo: bool

    def to_json(self) -> dict[str, object]:
        return {
            "idx": self.idx,
            "title": self.title,
            "page_start": self.page_start,
            "page_end": self.page_end,
            "word_count": self.word_count,
            "is_pseudo": self.is_pseudo,
        }


def _finalize(
    starts: list[tuple[str, int]], page_count: int, page_words: list[int], is_pseudo: bool
) -> list[Chapter]:
    chapters: list[Chapter] = []
    for idx, (title, page_start) in enumerate(starts):
        page_end = starts[idx + 1][1] - 1 if idx + 1 < len(starts) else page_count
        page_end = max(page_end, page_start)
        word_count = sum(page_words[page_start - 1 : page_end])
        chapters.append(
            Chapter(
                idx=idx,
                title=title,
                page_start=page_start,
                page_end=page_end,
                word_count=word_count,
                is_pseudo=is_pseudo,
            )
        )
    return chapters


def _valid_toc_entries(toc: list[list[object]], page_count: int) -> list[tuple[int, str, int]]:
    entries: list[tuple[int, str, int]] = []
    for entry in toc:
        if len(entry) < 3:
            continue
        raw_level, raw_title, raw_page = entry[0], entry[1], entry[2]
        if type(raw_level) is not int or type(raw_page) is not int:
            continue
        if not (1 <= raw_level <= MAX_TOC_DEPTH) or not (1 <= raw_page <= page_count):
            continue
        entries.append((raw_level, str(raw_title).strip(), raw_page))
    return entries


def _starts_at_depth(entries: list[tuple[int, str, int]], depth: int) -> list[tuple[str, int]]:
    starts: list[tuple[int, str, int]] = []
    for level, title, page_no in entries:
        if level > depth:
            continue
        resolved = title or f"Chapter {len(starts) + 1}"
        if starts and page_no == starts[-1][2] and level > starts[-1][0]:
            if title:
                starts[-1] = (level, resolved, page_no)
            continue
        if starts and page_no <= starts[-1][2]:
            continue
        starts.append((level, resolved, page_no))
    return [(title, page_no) for _, title, page_no in starts]


def _chapter_words(
    starts: list[tuple[str, int]], page_count: int, page_words: list[int]
) -> list[int]:
    words: list[int] = []
    for idx, (_, page_start) in enumerate(starts):
        page_end = starts[idx + 1][1] - 1 if idx + 1 < len(starts) else page_count
        page_end = max(page_end, page_start)
        words.append(sum(page_words[page_start - 1 : page_end]))
    return words


def _mostly_sparse(chapter_words: list[int]) -> bool:
    sparse = sum(1 for words in chapter_words if words < SPARSE_CHAPTER_WORDS)
    return sparse > len(chapter_words) * MAX_SPARSE_RATIO


def _median(values: list[int]) -> float:
    ordered = sorted(values)
    middle = len(ordered) // 2
    if len(ordered) % 2:
        return float(ordered[middle])
    return (ordered[middle - 1] + ordered[middle]) / 2


def chapters_from_toc(
    toc: list[list[object]], page_count: int, page_words: list[int]
) -> list[Chapter] | None:
    if len(page_words) != page_count:
        return None
    entries = _valid_toc_entries(toc, page_count)
    if not entries:
        return None
    starts: list[tuple[str, int]] | None = None
    fallback: list[tuple[str, int]] | None = None
    for depth in sorted({level for level, _, _ in entries}):
        candidate = _starts_at_depth(entries, depth)
        if len(candidate) < MIN_TOC_CHAPTERS:
            continue
        if len(candidate) > MAX_TOC_CHAPTERS:
            continue
        chapter_words = _chapter_words(candidate, page_count, page_words)
        if _mostly_sparse(chapter_words):
            continue
        if _median(chapter_words) < MIN_MEDIAN_CHAPTER_WORDS:
            continue
        if fallback is None:
            fallback = candidate
        total = sum(chapter_words)
        largest = max(chapter_words)
        if total > 0 and largest > total * COARSE_SHARE and largest > COARSE_CHAPTER_WORDS:
            continue
        starts = candidate
        break
    if starts is None:
        starts = fallback if fallback is not None else _starts_at_depth(entries, 1)
    if not starts:
        return None
    if starts[0][1] > 1:
        starts.insert(0, ("Front matter", 1))
    return _finalize(starts, page_count, page_words, is_pseudo=False)


def _bucket_font_size(size: float) -> float:
    return round(size / FONT_SIZE_BUCKET_PT) * FONT_SIZE_BUCKET_PT


def _body_font_size(pages: list[list[TextBlock]]) -> float:
    sizes: dict[float, int] = {}
    for blocks in pages:
        for block in blocks:
            if block.role == "body" and block.font_size > 0:
                words = len(block.text.split())
                bucket = _bucket_font_size(block.font_size)
                sizes[bucket] = sizes.get(bucket, 0) + words
    if not sizes:
        return 0.0
    return max(sizes, key=lambda size: sizes[size])


def chapters_from_headings(
    pages: list[list[TextBlock]], page_count: int, page_words: list[int]
) -> list[Chapter] | None:
    body_size = _body_font_size(pages)
    if body_size <= 0:
        return None
    starts: list[tuple[str, int]] = []
    for page_index, blocks in enumerate(pages):
        for block in blocks:
            if block.role != "body":
                continue
            words = block.text.split()
            if (
                block.font_size >= body_size * HEADING_FONT_RATIO
                and 0 < len(words) <= HEADING_MAX_WORDS
            ):
                title = " ".join(block.text.split())
                page_no = page_index + 1
                if not starts or starts[-1][1] < page_no:
                    starts.append((title, page_no))
                break
    if len(starts) < 2 or len(starts) > MAX_HEURISTIC_CHAPTERS:
        return None
    if starts[0][1] > 1:
        starts.insert(0, ("Front matter", 1))
    return _finalize(starts, page_count, page_words, is_pseudo=False)


def pseudo_chapters(page_count: int, page_words: list[int]) -> list[Chapter]:
    starts = [
        (f"Pages {start}\u2013{min(start + PSEUDO_CHAPTER_PAGES - 1, page_count)}", start)
        for start in range(1, page_count + 1, PSEUDO_CHAPTER_PAGES)
    ]
    return _finalize(starts, page_count, page_words, is_pseudo=True)


def detect_chapters(
    toc: list[list[object]], pages: list[list[TextBlock]], page_words: list[int]
) -> list[Chapter]:
    page_count = len(pages)
    from_toc = chapters_from_toc(toc, page_count, page_words)
    if from_toc is not None:
        return from_toc
    from_headings = chapters_from_headings(pages, page_count, page_words)
    if from_headings is not None:
        return from_headings
    return pseudo_chapters(page_count, page_words)
