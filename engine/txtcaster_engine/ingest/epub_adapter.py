"""EPUB reflow on the standard library, selectolax and Pillow: the `Ingestor` for `.epub`.

Each spine document becomes one or more synthetic 612x792 pages (`reflow.py`), starting on a
fresh page so `page_number_from_location(spine_index)` is the document's first page and
`chapter_count()` is the spine length — the contract `manifest._epub_toc_from_spine` bridges
the TOC over. Blocks are the document's block-level elements in DOM order (already reading
order, so `blocks.extract_page` keeps them as-is), each `(bbox, text, max font size)`; words
carry `(x0, y0, x1, y1, token, block_no, line_no)` for follow-along geometry.

Rules mirrored from the engine this replaces, measured on probe books (S1 map): font sizes
scale by the HTML defaults (h1 2em … h6 0.67em, small/sup/sub 0.83em, keywords, em/%/pt/px),
`display:none`, `visibility:hidden` and the `hidden` attribute drop text, soft hyphens are
dropped, `<br>` breaks a line, `ul` items carry `•  ` (nested `○  `), `ol` items `n. `,
`list-style-type: none` suppresses markers, table rows read cell by cell. Not mirrored, on
purpose: text the old engine clipped when a `white-space: nowrap` run overflowed the page —
the new path keeps it (documented per book in the retained pins). Embedded media (`object`,
`iframe`, `video`, `audio`, `canvas`, `noframes`) contribute their fallback prose in place, as
the old engine did; `<svg>` contributes only `title`/`desc` (the old engine dropped SVG).
"""

from __future__ import annotations

import io
import re
import unicodedata
import zipfile
from dataclasses import dataclass, field
from functools import lru_cache
from pathlib import PurePosixPath
from typing import TYPE_CHECKING

from selectolax.parser import HTMLParser, Node

from txtcaster_engine.ingest import arabic_shaping, bidi, textlayer
from txtcaster_engine.ingest.epub import (
    EpubPackage,
    _normalize_href,
    check_epub_drm,
    expand_self_closing,
    read_member,
    read_package,
)
from txtcaster_engine.ingest.errors import IngestError
from txtcaster_engine.ingest.ingestor import Bbox, PageText, RawBlock, RawWord
from txtcaster_engine.ingest.reflow import REFLOW_FONT_SIZE, REFLOW_PAGE_HEIGHT, REFLOW_PAGE_WIDTH

if TYPE_CHECKING:
    from PIL import ImageFont

MARGIN = 36.0
LINE_HEIGHT = 1.35
PARAGRAPH_GAP = 0.5
INDENT = 24.0
BULLETS = ("•  ", "○  ")
BLOCK_TAGS = frozenset(
    {
        "p",
        "div",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "li",
        "dt",
        "dd",
        "blockquote",
        "pre",
        "table",
        "tr",
        "figure",
        "figcaption",
        "section",
        "article",
        "aside",
        "nav",
        "header",
        "footer",
        "main",
        "address",
        "center",
        "hr",
        "ul",
        "ol",
        "dl",
        "caption",
        "body",
        "details",
        "summary",
    }
)
DROP_TAGS = frozenset({"script", "style", "head", "template", "title", "img"})
"""Subtrees with no readable prose. Embedded media (`object`, `iframe`, `video`, `audio`, `canvas`,
`embed`) are NOT here: their children are the author's fallback prose, which the replaced engine
rendered in place (measured 2026-09-09: `<object>` inline and block fallback, `<iframe>` fallback,
`<noframes>`), so the walker descends into them like any inline element. `template` stays dropped
on purpose: its contents are inert per HTML, although the replaced engine rendered them."""
SVG_PROSE_TAGS = frozenset({"title", "desc"})
"""Inside `<svg>` only the accessible title/description is prose; drawn `<text>` is graphics."""
HEADING_EM = {"h1": 2.0, "h2": 1.5, "h3": 1.17, "h4": 1.0, "h5": 0.83, "h6": 0.67}
INLINE_EM = {"small": 0.83, "sup": 0.83, "sub": 0.83, "big": 1.17}
KEYWORD_EM = {
    "xx-small": 0.58,
    "x-small": 0.69,
    "small": 0.83,
    "medium": 1.0,
    "large": 1.2,
    "x-large": 1.5,
    "xx-large": 2.0,
    "smaller": 0.8333,
    "larger": 1.2,
}

STYLE_ATTR = "data-txtcaster-style"
_RULE = re.compile(r"([^{}]+)\{([^{}]*)\}")
_COMMENT = re.compile(r"/\*.*?\*/", re.S)
_AT_BLOCK = re.compile(r"@[^{};]*\{(?:[^{}]*\{[^{}]*\})*[^{}]*\}", re.S)
_AT_LINE = re.compile(r"@[^{};]*;")
WATCHED = ("display", "visibility", "list-style-type", "list-style", "font-size", "white-space")


def _declarations(body: str) -> str:
    """Only the properties the layout reads, as `name:value;` pairs."""
    kept = []
    for declaration in body.split(";"):
        name, _, value = declaration.partition(":")
        name = name.strip().lower()
        if name in WATCHED and value.strip():
            kept.append(f"{name}:{value.strip().lower()}")
    return ";".join(kept)


def _stylesheets(archive: zipfile.ZipFile, tree: HTMLParser, doc_path: str) -> str:
    """Linked and embedded CSS in document order; @-blocks (media, font-face) are skipped."""
    base = PurePosixPath(doc_path).parent
    parts: list[str] = []
    for link in tree.css("link"):
        rel = (link.attributes.get("rel") or "").lower()
        href = link.attributes.get("href")
        if "stylesheet" not in rel or not href:
            continue
        try:
            parts.append(
                read_member(archive, _normalize_href(href, base)).decode("utf-8", "replace")
            )
        except KeyError:
            continue
    for style in tree.css("style"):
        parts.append(style.text() or "")
    css = _COMMENT.sub("", "\n".join(parts))
    css = _AT_BLOCK.sub("", css)
    return _AT_LINE.sub("", css)


def _apply_styles(tree: HTMLParser, css: str) -> None:
    """Annotate matched nodes with the watched declarations. Rules apply in source order,
    later winning, inline `style` last; specificity is not modelled (documented approximation)."""
    for match in _RULE.finditer(css):
        declarations = _declarations(match.group(2))
        if not declarations:
            continue
        for selector in match.group(1).split(","):
            selector = selector.strip()
            if not selector:
                continue
            try:
                nodes = tree.css(selector)
            except Exception:  # selectolax raises on selectors lexbor cannot parse
                continue
            for node in nodes:
                node.attrs[STYLE_ATTR] = f"{node.attributes.get(STYLE_ATTR) or ''};{declarations}"
    for node in tree.css("[style]"):
        declarations = _declarations(node.attributes.get("style") or "")
        if declarations:
            node.attrs[STYLE_ATTR] = f"{node.attributes.get(STYLE_ATTR) or ''};{declarations}"


def _style(node: Node) -> dict[str, str]:
    out: dict[str, str] = {}
    for declaration in (node.attributes.get(STYLE_ATTR) or "").split(";"):
        name, _, value = declaration.partition(":")
        if name:
            out[name] = value
    return out


_LENGTH = re.compile(r"^([0-9]*\.?[0-9]+)\s*(em|rem|%|pt|px|)$")


def _font_size(node: Node, parent_size: float, style: dict[str, str]) -> float:
    """Inherited size scaled by the element's default (headings, small/sup/sub) then by its
    `font-size` declaration; px counts as pt, as the engine this replaced did."""
    size = parent_size * HEADING_EM.get(node.tag, INLINE_EM.get(node.tag, 1.0))
    value = style.get("font-size", "")
    if not value:
        return size
    if value in KEYWORD_EM:
        return (parent_size if value in ("smaller", "larger") else REFLOW_FONT_SIZE) * KEYWORD_EM[
            value
        ]
    match = _LENGTH.match(value)
    if not match:
        return size
    number, unit = float(match.group(1)), match.group(2)
    if unit == "em":
        return parent_size * number
    if unit == "rem":
        return REFLOW_FONT_SIZE * number
    if unit == "%":
        return parent_size * number / 100.0
    return number


def _hidden(node: Node, style: dict[str, str]) -> bool:
    return (
        style.get("display") == "none"
        or style.get("visibility") == "hidden"
        or "hidden" in node.attributes
    )


def _marker(list_node: Node, style: dict[str, str], depth: int, counter: int) -> str:
    kind = style.get("list-style-type") or style.get("list-style") or ""
    if "none" in kind.split():
        return ""
    if list_node.tag == "ol":
        return f"{counter}. "
    return BULLETS[min(depth, len(BULLETS) - 1)]


@dataclass
class Run:
    text: str
    size: float


@dataclass
class Flow:
    """One block-level element's text: runs (inline size changes) and hard line breaks as
    `\\n` runs; `indent` in points; `pre` keeps the author's whitespace."""

    runs: list[Run] = field(default_factory=list)
    indent: float = 0.0
    pre: bool = False

    def size(self) -> float:
        return max((r.size for r in self.runs if r.text.strip()), default=float(REFLOW_FONT_SIZE))


_WS = re.compile(r"[ \t\r\n\f]+")


def _list_start(raw: str | None) -> int:
    try:
        return int((raw or "").strip() or 1)
    except ValueError:
        return 1


class _Walker:
    """Depth-first DOM walk producing `Flow`s in document order."""

    def __init__(self) -> None:
        self.flows: list[Flow] = []
        self._open: Flow | None = None

    def _current(self, indent: float, pre: bool = False) -> Flow:
        if self._open is None:
            self._open = Flow(indent=indent, pre=pre)
            self.flows.append(self._open)
        return self._open

    def _close(self) -> None:
        self._open = None

    def text(self, raw: str, size: float, indent: float, pre: bool) -> None:
        raw = raw.replace("­", "")
        if not pre:
            raw = _WS.sub(" ", raw)
            if not raw.strip() and self._open is None:
                return
        elif not raw.strip("\n\r"):
            pass
        self._current(indent, pre).runs.append(Run(raw, size))

    def walk(self, node: Node, size: float, indent: float, depth: int, pre: bool) -> None:
        for child in node.iter(include_text=True):
            if child.tag == "-text":
                self.text(child.text(deep=False) or "", size, indent, pre)
                continue
            if child.tag in DROP_TAGS:
                continue
            if child.tag == "svg":
                for prose in child.traverse():
                    if prose.tag in SVG_PROSE_TAGS and (prose.text() or "").strip():
                        self.text(prose.text() + " ", size, indent, pre)
                continue
            style = _style(child)
            if _hidden(child, style):
                continue
            child_size = _font_size(child, size, style)
            child_pre = pre or child.tag == "pre" or style.get("white-space", "").startswith("pre")
            if child.tag == "br":
                self._current(indent, pre).runs.append(Run("\n", size))
            elif child.tag in ("ul", "ol"):
                self._close()
                counter = _list_start(child.attributes.get("start"))
                for item in child.iter():
                    if item.tag != "li":
                        continue
                    item_style = _style(item)
                    if _hidden(item, item_style):
                        continue
                    marker = _marker(child, style, depth, counter)
                    if item_style.get("display", "list-item") != "list-item":
                        marker = ""  # `li { display: block }` (PG footer CSS) loses its marker
                    counter += 1
                    self._close()
                    flow = self._current(indent + INDENT)
                    if marker:
                        flow.runs.append(Run(marker, child_size))
                    self.walk(
                        item,
                        _font_size(item, child_size, item_style),
                        indent + INDENT,
                        depth + 1,
                        child_pre,
                    )
                    self._close()
            elif child.tag == "tr":
                self._close()
                self._current(indent)
                curr = child.child
                while curr is not None:
                    if curr.tag in ("td", "th"):
                        self.walk(
                            curr,
                            _font_size(curr, child_size, _style(curr)),
                            indent,
                            depth,
                            child_pre,
                        )
                        self._current(indent).runs.append(Run(" ", child_size))
                    curr = curr.next
                self._close()
            elif child.tag in BLOCK_TAGS:
                self._close()
                extra = INDENT if child.tag in ("blockquote", "dd") else 0.0
                self.walk(child, child_size, indent + extra, depth, child_pre)
                self._close()
            else:
                self.walk(child, child_size, indent, depth, child_pre)


ARABIC_FONT = "assets/fonts/NotoNaskhArabic-Regular.ttf"
"""Bundled Noto Naskh Arabic (OFL 1.1, licence beside it). Pillow's embedded default face
(Aileron) has no Arabic glyphs, so every Arabic character drew as a .notdef box."""
_ARABIC = (
    ("\u0600", "\u06ff"),  # Arabic
    ("\u0750", "\u077f"),  # Arabic Supplement
    ("\u08a0", "\u08ff"),  # Arabic Extended-A
    ("\ufb50", "\ufdff"),  # Presentation Forms-A
    ("\ufe70", "\ufeff"),  # Presentation Forms-B
)


def _arabic(token: str) -> bool:
    """Whether the token needs the Arabic face: any letter, mark or digit of the Arabic
    blocks (Arabic-Indic digits too — the default face lacks them as well)."""
    return any(lo <= ch <= hi for ch in token for lo, hi in _ARABIC)


@lru_cache(maxsize=128)
def _font(size: float, arabic: bool = False) -> ImageFont.FreeTypeFont:
    from PIL import ImageFont

    if arabic:
        from importlib.resources import files

        path = files("txtcaster_engine").joinpath(ARABIC_FONT)
        # BASIC on purpose: libraqm's fribidi is a system library Pillow's Windows wheel lacks,
        # so shaping must not depend on the layout engine or bundles would render differently.
        return ImageFont.truetype(str(path), max(size, 1.0), layout_engine=ImageFont.Layout.BASIC)
    return ImageFont.load_default(size=max(size, 1.0))  # type: ignore[return-value]


BIDI_CONTROLS = frozenset("\u200e\u200f\u202a\u202b\u202c\u202d\u202e\u2066\u2067\u2068\u2069")
"""Directional marks and embeddings: zero-width formatting no face has a glyph for."""


def _drawn(token: str) -> str:
    """What the painter gets for a logical token. Directional controls never paint; an Arabic
    token also drops other format characters, turns its spaces (hair space, no-break space)
    into the plain space and is joined and put in visual order (`arabic_shaping.drawn`); any
    other token is verbatim."""
    token = "".join(ch for ch in token if ch not in BIDI_CONTROLS)
    if not _arabic(token):
        return token
    cleaned = "".join(
        " " if unicodedata.category(ch) == "Zs" else ch
        for ch in token
        if unicodedata.category(ch) != "Cf"
    )
    return arabic_shaping.drawn(cleaned)


def _runs(drawn: str) -> list[tuple[str, bool]]:
    """Maximal same-face runs of a drawn string: Arabic-block characters take the Naskh face,
    everything else (Latin, ASCII digits and punctuation, spaces) the default face, so no face
    is ever asked for a glyph it lacks."""
    runs: list[tuple[str, bool]] = []
    for ch in drawn:
        arabic = _arabic(ch)
        if runs and runs[-1][1] == arabic:
            runs[-1] = (runs[-1][0] + ch, arabic)
        else:
            runs.append((ch, arabic))
    return runs


@lru_cache(maxsize=200_000)
def _width(token: str, size: float) -> float:
    drawn = _drawn(token)
    if not _arabic(drawn):
        return float(_font(round(size, 2)).getlength(drawn))
    return sum(float(_font(round(size, 2), arabic).getlength(run)) for run, arabic in _runs(drawn))


@dataclass
class Line:
    y0: float
    y1: float
    size: float
    words: list[tuple[float, float, str]]  # x0, x1, token

    def text(self) -> str:
        return " ".join(token for _, _, token in self.words)


@dataclass
class PageBlock:
    lines: list[Line]

    def bbox(self) -> Bbox:
        return (
            min(x0 for line in self.lines for x0, _, _ in line.words),
            self.lines[0].y0,
            max(x1 for line in self.lines for _, x1, _ in line.words),
            self.lines[-1].y1,
        )

    def size(self) -> float:
        return max(line.size for line in self.lines)


def _tokens(flow: Flow) -> list[list[tuple[str, float]]]:
    """Hard lines of (token, size). Adjacent runs with no space between them form one token
    (`noscript: <b>NOSCRIPT</b>.` is `NOSCRIPT.`); pre flows keep their own line breaks and
    spaces as tokens; a token's size is the largest run it spans."""
    lines: list[list[tuple[str, float]]] = [[]]
    open_token = False
    for run in flow.runs:
        for k, piece in enumerate(run.text.split("\n")):
            if k:
                lines.append([])
                open_token = False
            if flow.pre:
                if piece:
                    lines[-1].append((piece, run.size))
                continue
            for j, token in enumerate(piece.split(" ")):
                if j:
                    open_token = False
                if not token:
                    continue
                if open_token and lines[-1]:
                    prev, size = lines[-1][-1]
                    lines[-1][-1] = (prev + token, max(size, run.size))
                else:
                    lines[-1].append((token, run.size))
                open_token = True
            if piece.endswith(" "):
                open_token = False
    return [line for line in lines if line]


def _split_long(token: str, size: float, width: float) -> list[str]:
    """A token wider than the line breaks after hyphens first, then by character."""
    if _width(token, size) <= width:
        return [token]
    parts = [p + "-" for p in token.split("-")]
    parts[-1] = parts[-1][:-1]
    out: list[str] = []
    for part in parts:
        while part and _width(part, size) > width:
            cut = max(
                1, next((k for k in range(len(part), 0, -1) if _width(part[:k], size) <= width), 1)
            )
            out.append(part[:cut])
            part = part[cut:]
        if part:
            out.append(part)
    return out


def _visual(
    words: list[tuple[float, float, str]],
    sizes: list[float],
    rtl: bool,
    x_left: float,
    x_right: float,
) -> list[tuple[float, float, str]]:
    """UBA L2 placement of one line's words (logical order in, logical order out; only the
    rectangles move): on an RTL base the line hangs from the right margin and its words advance
    leftwards, embedded LTR runs and numbers keep their internal order (`bidi.visual_order`);
    an LTR base line with an embedded RTL run mirrors that. Pure-LTR lines never come here, so
    their positions are the greedy loop's own."""
    order = bidi.visual_order([token for _, _, token in words], rtl)
    advances = [x1 - x0 for x0, x1, _ in words]
    spaces = [0.0] + [_width(" ", sizes[i]) for i in order[1:]]
    x = x_right - (sum(advances) + sum(spaces)) if rtl else x_left
    placed = list(words)
    for k, i in enumerate(order):
        x += spaces[k]
        placed[i] = (x, x + advances[i], words[i][2])
        x += advances[i]
    return placed


def _layout(flows: list[Flow]) -> list[list[PageBlock]]:
    """Greedy line breaking onto fixed pages; a block that crosses a page boundary continues
    as a new block on the next page. Every document gets at least one page."""
    pages: list[list[PageBlock]] = [[]]
    y = MARGIN
    bottom = REFLOW_PAGE_HEIGHT - MARGIN
    for flow in flows:
        hard_lines = _tokens(flow)
        if not hard_lines:
            continue
        size = flow.size()
        gap = PARAGRAPH_GAP * size
        if pages[-1]:
            y += gap
        block: PageBlock | None = None
        x_left = MARGIN + flow.indent
        width = REFLOW_PAGE_WIDTH - MARGIN - x_left
        text = " ".join(run.text for run in flow.runs)
        rtl = bidi.has_rtl(text) and bidi.base_direction(text) == "R"
        for hard in hard_lines:
            queue = [(piece, s) for token, s in hard for piece in _split_long(token, s, width)]
            while queue:
                line_size = queue[0][1]
                words: list[tuple[float, float, str]] = []
                sizes: list[float] = []
                x = x_left
                while queue:
                    token, s = queue[0]
                    advance = _width(token, s)
                    space = _width(" ", s) if words else 0.0
                    if words and x + space + advance > x_left + width + 1e-6:
                        break
                    x += space
                    words.append((x, x + advance, token))
                    sizes.append(s)
                    x += advance
                    line_size = max(line_size, s)
                    queue.pop(0)
                if rtl or any(bidi.has_rtl(token) for _, _, token in words):
                    words = _visual(words, sizes, rtl, x_left, x_left + width)
                height = LINE_HEIGHT * line_size
                if y + height > bottom and (pages[-1] or block is not None):
                    pages.append([])
                    y = MARGIN
                    block = None
                line = Line(y, y + height, line_size, words)
                if block is None:
                    block = PageBlock([line])
                    pages[-1].append(block)
                else:
                    block.lines.append(line)
                y += height
        y += gap
    return pages


class EpubDocument:
    backend = "epub"
    source_label = "reflow"
    is_reflowable = True

    def __init__(self, archive: zipfile.ZipFile, package: EpubPackage, book_path: str) -> None:
        self._path = book_path
        self._package = package
        self._pages: list[list[PageBlock]] = []
        self._starts: list[int] = []
        names = set(archive.namelist())
        for idref in package.spine:
            item = package.items.get(idref)
            self._starts.append(len(self._pages))
            doc_path = package.archive_path(item.href) if item else ""
            if item is None or doc_path not in names:
                self._pages.append([])
                continue
            tree = HTMLParser(expand_self_closing(read_member(archive, doc_path)))
            _apply_styles(tree, _stylesheets(archive, tree, doc_path))
            walker = _Walker()
            if tree.body is not None:
                walker.walk(tree.body, float(REFLOW_FONT_SIZE), 0.0, 0, False)
            self._pages.extend(_layout(walker.flows))
        self.page_count = len(self._pages)

    def page_size(self, index: int) -> tuple[float, float]:
        return float(REFLOW_PAGE_WIDTH), float(REFLOW_PAGE_HEIGHT)

    def page_text(self, index: int) -> PageText:
        blocks: list[RawBlock] = []
        words: list[RawWord] = []
        for block in self._pages[index]:
            text = "\n".join(line.text() for line in block.lines).strip()
            if not text:
                continue
            blocks.append((block.bbox(), text, block.size()))
            for line_no, line in enumerate(block.lines):
                words.extend(
                    (x0, line.y0, x1, line.y1, token, len(blocks) - 1, line_no)
                    for x0, x1, token in line.words
                )
        blocks, words = textlayer.fold_fragments(blocks, words)
        return PageText(blocks=blocks, words=words)

    def render_png(self, index: int, scale: float, clip: Bbox | None = None) -> bytes:
        """The page as the layout drew it, so word rectangles and pixels agree."""
        from PIL import Image, ImageDraw

        width, height = round(REFLOW_PAGE_WIDTH * scale), round(REFLOW_PAGE_HEIGHT * scale)
        image = Image.new("RGB", (width, height), "white")
        draw = ImageDraw.Draw(image)
        for block in self._pages[index]:
            for line in block.lines:
                size = round(line.size * scale, 2)
                for x0, _, token in line.words:
                    text, x, y = _drawn(token), x0 * scale, line.y0 * scale
                    if not _arabic(text):
                        draw.text((x, y), text, fill="black", font=_font(size))
                        continue
                    for run, arabic in _runs(text):
                        font = _font(size, arabic)
                        draw.text((x, y), run, fill="black", font=font)
                        x += font.getlength(run)
        if clip is not None:
            x0, y0, x1, y1 = clip
            box = (
                max(0, round(x0 * scale)),
                max(0, round(y0 * scale)),
                min(width, round(x1 * scale)),
                min(height, round(y1 * scale)),
            )
            if box[2] > box[0] and box[3] > box[1]:
                image = image.crop(box)
        buffer = io.BytesIO()
        image.save(buffer, format="PNG")
        return buffer.getvalue()

    def toc(self) -> list[list[object]]:
        return []

    def metadata(self) -> dict[str, str]:
        pairs = (("title", self._package.title), ("author", self._package.author))
        return {key: value for key, value in pairs if value}

    def chapter_count(self) -> int:
        return len(self._starts)

    def page_number_from_location(self, spine_index: int) -> int:
        return self._starts[spine_index]

    def close(self) -> None:
        self._pages = []


def open_epub(book_path: str) -> EpubDocument:
    check_epub_drm(book_path)
    try:
        with zipfile.ZipFile(book_path) as archive:
            package = read_package(archive)
            if not package.spine:
                raise IngestError("ERR_EMPTY", "epub has no spine documents")
            return EpubDocument(archive, package, book_path)
    except (zipfile.BadZipFile, OSError) as exc:
        raise IngestError("ERR_OPEN", f"cannot open file: {exc}") from exc
