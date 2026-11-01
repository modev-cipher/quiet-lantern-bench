from __future__ import annotations

import logging
import mmap
import os
import re
import zlib

_LOG = logging.getLogger(__name__)

_OBJ = re.compile(rb"(?<![0-9])(\d+)\s+\d+\s+obj\b(.*?)endobj", re.S)
_STREAM = re.compile(rb"stream\r?\n(.*?)endstream", re.S)
_REF = re.compile(rb"(\d+)\s+0\s+R")
_INT = {b"/N": re.compile(rb"/N\s+(\d+)"), b"/First": re.compile(rb"/First\s+(\d+)")}


def _balanced(data: bytes, start: int, opener: bytes, closer: bytes) -> bytes:
    depth = 0
    i = start
    while i < len(data):
        if data.startswith(opener, i):
            depth += 1
            i += len(opener)
        elif data.startswith(closer, i):
            depth -= 1
            i += len(closer)
            if depth == 0:
                return data[start:i]
        else:
            i += 1
    return data[start:]


def value_of(dictionary: bytes, key: bytes) -> bytes:
    match = re.search(re.escape(key) + rb"(?![A-Za-z0-9_.])\s*", dictionary)
    if match is None:
        return b""
    start = match.end()
    if dictionary.startswith(b"<<", start):
        return _balanced(dictionary, start, b"<<", b">>")
    if dictionary.startswith(b"[", start):
        return _balanced(dictionary, start, b"[", b"]")
    token = re.match(rb"\d+\s+0\s+R|/?[^\s/<>\[\]()]+", dictionary[start:])
    return token.group(0) if token else b""


class PdfObjects:
    """Object index of one PDF for the pattern / font / text-object census.

    Holds only object HEADS (the dictionary text before `stream`) and, per object, the
    `(start, end)` byte span of its body in the source. Stream bodies are read lazily from
    that span (`body()` / `stream()`), never copied wholesale: an earlier version kept a copy
    of every body (`bodies`), which on image-heavy books duplicated the whole file in Python
    memory for the document's lifetime (SOAK-1a anomaly A1, ING-11). `PdfObjects(data)`
    indexes in-memory bytes and keeps them referenced; `PdfObjects.from_path(path)` indexes an
    mmap of the file and keeps only the path, so the resident cost after indexing is the heads.
    """

    def __init__(self, data: bytes | mmap.mmap, path: str | None = None) -> None:
        self.objects: dict[int, bytes] = {}
        self._spans: dict[int, tuple[int, int]] = {}
        self._path = path
        self._data: bytes | mmap.mmap | None = None if path else data
        self.problems: set[str] = set()
        for match in _OBJ.finditer(data):
            number = int(match.group(1))
            start, end = match.span(2)
            stream_at = data.find(b"stream", start, end)
            head_text = bytes(data[start : end if stream_at < 0 else stream_at])
            self.objects[number] = head_text
            self._spans[number] = (start, end)
            if b"/ObjStm" in head_text:
                self._expand(bytes(data[start:end]))
        root_at = data.rfind(b"/Root")
        self.root = _ref_number_at(data, root_at) if root_at >= 0 else 0
        if not self.root:
            for body in self.objects.values():
                if b"/XRef" in head(body) and b"/Root" in head(body):
                    self.root = _ref_number(value_of(head(body), b"/Root"))
        if b"%%EOF" not in bytes(data[-1024:]):
            self.problems.add("truncated_syntax")
        pages = _ref_number(value_of(self.objects.get(self.root, b""), b"/Pages"))
        if not self.root or pages not in self.objects:
            self.problems.add("no_pages_tree")
        if self.problems:
            _LOG.warning(
                "pattern census has no evidence for this document: %s",
                ",".join(sorted(self.problems)),
            )

    @classmethod
    def from_path(cls, path: str) -> PdfObjects:
        """Index the file through a read-only mmap that is unmapped before returning."""
        with open(path, "rb") as handle:
            if os.fstat(handle.fileno()).st_size == 0:
                return cls(b"", path=path)
            with mmap.mmap(handle.fileno(), 0, access=mmap.ACCESS_READ) as mapped:
                return cls(mapped, path=path)

    def _expand(self, body: bytes) -> None:
        stream = _STREAM.search(body)
        if stream is None:
            return
        raw = stream.group(1)
        try:
            data = zlib.decompress(raw) if b"/FlateDecode" in head(body) else raw
        except zlib.error:
            self.problems.add("objstm_zlib_error")
            return
        count = _int_field(head(body), b"/N")
        first = _int_field(head(body), b"/First")
        numbers = data[:first].split()
        pairs = [(int(numbers[i]), int(numbers[i + 1])) for i in range(0, 2 * count, 2)]
        for index, (number, offset) in enumerate(pairs):
            end = first + pairs[index + 1][1] if index + 1 < len(pairs) else len(data)
            self.objects.setdefault(number, head(data[first + offset : end]))

    def body(self, number: int) -> bytes:
        """The full `N 0 obj … endobj` body (head + stream), read from the source on demand."""
        span = self._spans.get(number)
        if span is None:
            return b""
        if self._data is not None:
            return bytes(self._data[span[0] : span[1]])
        if self._path is None:
            return b""
        with open(self._path, "rb") as handle:
            handle.seek(span[0])
            return handle.read(span[1] - span[0])

    def resolve(self, value: bytes) -> bytes:
        ref = re.fullmatch(rb"\s*(\d+)\s+0\s+R\s*", value)
        return self.objects.get(int(ref.group(1)), b"") if ref else value

    def dictionary(self, value: bytes, key: bytes) -> bytes:
        return self.resolve(value_of(head(self.resolve(value)), key))

    def stream(self, number: int) -> bytes:
        return decoded_stream(self.body(number))


def decoded_stream(body: bytes) -> bytes:
    """Stream data of an object body: raw when unfiltered, inflated when FlateDecode, else empty."""
    match = _STREAM.search(body)
    if match is None:
        return b""
    raw = match.group(1)
    if b"/FlateDecode" not in head(body):
        return raw if b"/Filter" not in head(body) else b""
    try:
        return zlib.decompressobj().decompress(raw)
    except zlib.error:
        return b""


def head(body: bytes) -> bytes:
    return body.split(b"stream", 1)[0]


def _ref_number(value: bytes) -> int:
    match = _REF.search(value)
    return int(match.group(1)) if match else 0


def _ref_number_at(data: bytes | mmap.mmap, pos: int) -> int:
    """`_ref_number(data[pos:])` without copying the tail of the buffer."""
    match = _REF.search(data, pos)
    return int(match.group(1)) if match else 0


def _int_field(dictionary: bytes, key: bytes) -> int:
    match = _INT[key].search(dictionary)
    return int(match.group(1)) if match else 0


def _page_resources(objects: PdfObjects, index: int) -> list[bytes]:
    inherited: list[bytes] = []
    counter = [0]

    def walk(number: int, chain: list[bytes]) -> list[bytes] | None:
        body = head(objects.objects.get(number, b""))
        chain = chain + [objects.resolve(value_of(body, b"/Resources"))]
        if value_of(body, b"/Type") == b"/Page" or b"/Kids" not in body:
            if counter[0] == index:
                return chain
            counter[0] += 1
            return None
        for kid in _REF.findall(value_of(body, b"/Kids")):
            found = walk(int(kid), chain)
            if found is not None:
                return found
        return None

    root = head(objects.objects.get(objects.root, b""))
    pages = _REF.search(value_of(root, b"/Pages"))
    found = walk(int(pages.group(1)), inherited) if pages else None
    return [resources for resources in (found or []) if resources]


def _paints_image(objects: PdfObjects, resources: bytes, seen: set[bytes]) -> bool:
    for ref in _REF.findall(value_of(head(resources), b"/XObject")):
        body = objects.objects.get(int(ref), b"")
        subtype = value_of(head(body), b"/Subtype")
        if subtype == b"/Image":
            return True
        if subtype == b"/Form" and ref not in seen:
            nested = objects.dictionary(body, b"/Resources")
            if _paints_image(objects, nested, seen | {ref}):
                return True
    return _pattern_paints_image(objects, resources, seen)


def _pattern_paints_image(objects: PdfObjects, resources: bytes, seen: set[bytes]) -> bool:
    for ref in _REF.findall(objects.resolve(value_of(head(resources), b"/Pattern"))):
        body = objects.objects.get(int(ref), b"")
        if value_of(head(body), b"/PatternType") != b"1" or ref in seen:
            continue
        cell = objects.dictionary(body, b"/Resources")
        if _paints_image(objects, cell, seen | {ref}):
            return True
    return False


def pattern_image_count(objects: PdfObjects, page_index: int) -> int:
    for resources in _page_resources(objects, page_index):
        if _pattern_paints_image(objects, resources, set()):
            return 1
        for ref in _REF.findall(value_of(head(resources), b"/XObject")):
            body = objects.objects.get(int(ref), b"")
            if value_of(head(body), b"/Subtype") == b"/Form":
                nested = objects.dictionary(body, b"/Resources")
                if _pattern_paints_image(objects, nested, {ref}):
                    return 1
    return 0


def page_streams(objects: PdfObjects, page_index: int) -> list[bytes]:
    from txtcaster_engine.ingest.textobjects import painted_names

    number = _page_number(objects, page_index)
    if number is None:
        return []
    body = objects.objects.get(number, b"")
    contents = value_of(body, b"/Contents")
    page = b"\n".join(objects.stream(int(ref)) for ref in _REF.findall(contents))
    resources = _page_resources(objects, page_index)
    xobjects = _xobject_refs(objects, resources[-1] if resources else b"")
    out: list[bytes] = []
    work: list[tuple[bytes, dict[bytes, int]]] = [(page, xobjects)]
    seen: set[int] = set()
    while work:
        stream, names = work.pop()
        out.append(stream)
        for name in painted_names(stream):
            ref = names.get(name)
            if ref is None or ref in seen:
                continue
            seen.add(ref)
            form = objects.objects.get(ref, b"")
            if value_of(form, b"/Subtype") == b"/Form":
                body = objects.body(ref)
                nested = objects.dictionary(body, b"/Resources")
                work.append((decoded_stream(body), _xobject_refs(objects, nested)))
    return out


def _xobject_refs(objects: PdfObjects, resources: bytes) -> dict[bytes, int]:
    xobjects = objects.resolve(value_of(head(resources), b"/XObject"))
    return {
        name: int(ref) for name, ref in re.findall(rb"/([^\s/\[\]<>()]+)\s+(\d+)\s+0\s+R", xobjects)
    }


def _page_number(objects: PdfObjects, index: int) -> int | None:
    counter = [0]

    def walk(number: int) -> int | None:
        body = head(objects.objects.get(number, b""))
        if value_of(body, b"/Type") == b"/Page" or b"/Kids" not in body:
            counter[0] += 1
            return number if counter[0] - 1 == index else None
        for kid in _REF.findall(value_of(body, b"/Kids")):
            found = walk(int(kid))
            if found is not None:
                return found
        return None

    root = head(objects.objects.get(objects.root, b""))
    pages = _REF.search(value_of(root, b"/Pages"))
    return walk(int(pages.group(1))) if pages else None


STANDARD_GLYPH_SCALE = 0.001
GLYPH_SCALE_TOLERANCE = 0.2


class FontMap:
    def __init__(
        self,
        base_name: bytes,
        subtype: bytes,
        differences: dict[int, bytes],
        matrix_scale: float = STANDARD_GLYPH_SCALE,
    ) -> None:
        self.base_name = base_name.split(b"+", 1)[-1] if len(base_name) > 7 else base_name
        self.subtype = subtype
        self.differences = differences
        self.matrix_scale = matrix_scale

    @property
    def glyph_space_size(self) -> bool:
        return (
            self.subtype == b"/Type3"
            and abs(self.matrix_scale - STANDARD_GLYPH_SCALE)
            > GLYPH_SCALE_TOLERANCE * STANDARD_GLYPH_SCALE
        )


def _matrix_scale(font: bytes) -> float:
    numbers = re.findall(rb"-?\d*\.?\d+(?:[eE][-+]?\d+)?", value_of(font, b"/FontMatrix"))
    if len(numbers) != 6:
        return STANDARD_GLYPH_SCALE
    a, b = float(numbers[0]), float(numbers[1])
    return (a * a + b * b) ** 0.5 or STANDARD_GLYPH_SCALE


def _differences(objects: PdfObjects, font: bytes) -> dict[int, bytes]:
    encoding = objects.resolve(value_of(font, b"/Encoding"))
    out: dict[int, bytes] = {}
    code = 0
    for tok in re.findall(rb"/[^\s/\[\]<>()]*|\d+", value_of(head(encoding), b"/Differences")):
        if tok[:1] == b"/":
            out[code] = re.sub(
                rb"#([0-9A-Fa-f]{2})", lambda m: bytes([int(m.group(1), 16)]), tok[1:]
            )
            code += 1
        else:
            code = int(tok)
    return out


def page_fonts(objects: PdfObjects, page_index: int) -> list[FontMap]:
    out: list[FontMap] = []
    seen: set[int] = set()
    for resources in _page_resources(objects, page_index):
        fonts = objects.resolve(value_of(head(resources), b"/Font"))
        for ref in _REF.findall(fonts):
            number = int(ref)
            if number in seen:
                continue
            seen.add(number)
            font = head(objects.objects.get(number, b""))
            base = value_of(font, b"/BaseFont")
            base = base[1:] if base[:1] == b"/" else base
            out.append(
                FontMap(
                    base,
                    value_of(font, b"/Subtype"),
                    _differences(objects, font),
                    _matrix_scale(font),
                )
            )
    return out
