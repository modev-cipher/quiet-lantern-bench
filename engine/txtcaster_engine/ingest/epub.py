"""EPUB container, package (OPF), NCX and nav reading on the standard library plus selectolax.

No copyleft EPUB library: the package document and NCX are XML and read with ElementTree; the
nav document and content documents are XHTML and read with selectolax. TOC precedence mirrors
the reader this replaced (NCX when the spine names one, else the nav document), including
its href handling: NCX `src` values are kept raw, nav hrefs are joined onto the nav document's
directory, and both are resolved against the spine through the same candidate bases."""

import re
import xml.etree.ElementTree as ET
import zipfile
from dataclasses import dataclass, field
from pathlib import PurePosixPath
from urllib.parse import unquote

from selectolax.parser import HTMLParser, Node

from txtcaster_engine.ingest.errors import IngestError

ENCRYPTION_ENTRY = "META-INF/encryption.xml"
CONTAINER_ENTRY = "META-INF/container.xml"
FONT_OBFUSCATION_ALGORITHMS = frozenset(
    {
        "http://www.idpf.org/2008/embedding",
        "http://ns.adobe.com/pdf/enc#RC",
    }
)
FONT_MEDIA_TYPES = frozenset(
    {
        "application/vnd.ms-opentype",
        "application/font-woff",
        "application/font-sfnt",
    }
)


@dataclass
class TocEntry:
    title: str
    spine_index: int


@dataclass
class EpubStructure:
    title: str | None
    author: str | None
    language: str | None
    spine_hrefs: list[str]
    toc: list[TocEntry]


def is_epub(book_path: str) -> bool:
    return book_path.lower().endswith(".epub")


def _collapse_posix(path: str) -> str:
    parts: list[str] = []
    for segment in path.split("/"):
        if segment in ("", "."):
            continue
        if segment == ".." and parts and parts[-1] != "..":
            parts.pop()
            continue
        parts.append(segment)
    return "/".join(parts)


def _local_name(tag: str) -> str:
    return tag.rsplit(":", 1)[-1]


def _css_local(tree: HTMLParser | Node, name: str) -> list[Node]:
    root = tree.root if isinstance(tree, HTMLParser) else tree
    if root is None:
        return []
    return [node for node in root.traverse() if _local_name(node.tag) == name]


def _attr(node: object, name: str) -> str | None:
    attributes = getattr(node, "attributes", {})
    lowered = name.lower()
    for key, value in attributes.items():
        if value and (key.lower() == lowered or key.lower().endswith(f":{lowered}")):
            return str(value).strip()
    return None


def _is_font_media_type(media_type: str) -> bool:
    lowered = media_type.lower()
    return (
        lowered.startswith("font/")
        or lowered.startswith("application/x-font-")
        or lowered in FONT_MEDIA_TYPES
    )


EPUB_MEMBER_CAP_BYTES = 64 * 1024 * 1024


def read_member(archive: zipfile.ZipFile, name: str) -> bytes:
    cap = EPUB_MEMBER_CAP_BYTES
    with archive.open(name, "r") as stream:
        data = stream.read(cap + 1)
    if len(data) > cap:
        raise IngestError(
            "ERR_OPEN",
            f"EPUB member {name!r} decompresses beyond the {cap} byte budget",
        )
    return data


def _opf_path(archive: zipfile.ZipFile) -> str | None:
    if CONTAINER_ENTRY not in set(archive.namelist()):
        return None
    tree = HTMLParser(read_member(archive, CONTAINER_ENTRY))
    for node in _css_local(tree, "rootfile"):
        full_path = _attr(node, "full-path")
        if full_path:
            return _collapse_posix(full_path)
    return None


def _manifest_media_types(archive: zipfile.ZipFile) -> dict[str, str]:
    opf_path = _opf_path(archive)
    if opf_path is None or opf_path not in set(archive.namelist()):
        return {}
    base = PurePosixPath(opf_path).parent
    tree = HTMLParser(read_member(archive, opf_path))
    media_types: dict[str, str] = {}
    manifest_items = [
        item for manifest in _css_local(tree, "manifest") for item in _css_local(manifest, "item")
    ]
    for node in manifest_items:
        href = _attr(node, "href")
        media_type = _attr(node, "media-type")
        if not href or not media_type:
            continue
        media_types[_normalize_href(href, base)] = media_type
    return media_types


def _obfuscation_targets_are_fonts(encryption: HTMLParser, media_types: dict[str, str]) -> bool:
    for data_node in _css_local(encryption, "encrypteddata"):
        methods = _css_local(data_node, "encryptionmethod")
        algorithm = _attr(methods[0], "algorithm") if methods else None
        if algorithm not in FONT_OBFUSCATION_ALGORITHMS:
            return False
        references = _css_local(data_node, "cipherreference")
        uri = _attr(references[0], "uri") if references else None
        if not uri:
            return False
        media_type = media_types.get(_normalize_href(uri))
        if media_type is None or not _is_font_media_type(media_type):
            return False
    return True


def check_epub_drm(book_path: str) -> None:
    try:
        with zipfile.ZipFile(book_path) as archive:
            if ENCRYPTION_ENTRY not in set(archive.namelist()):
                return
            encryption = HTMLParser(read_member(archive, ENCRYPTION_ENTRY))
            algorithms: set[str] = set()
            for node in _css_local(encryption, "encryptionmethod"):
                algorithm = _attr(node, "algorithm")
                if algorithm:
                    algorithms.add(algorithm)
            if not algorithms or algorithms - FONT_OBFUSCATION_ALGORITHMS:
                raise IngestError("ERR_DRM", "document is encrypted (DRM) and cannot be ingested")
            media_types = _manifest_media_types(archive)
            if not _obfuscation_targets_are_fonts(encryption, media_types):
                raise IngestError("ERR_DRM", "document is encrypted (DRM) and cannot be ingested")
    except (zipfile.BadZipFile, OSError, KeyError) as exc:
        raise IngestError("ERR_OPEN", f"cannot open file: {exc}") from exc


def _normalize_href(href: str, base: PurePosixPath | None = None) -> str:
    without_fragment = unquote(href.split("#", 1)[0])
    if base is not None and base.parts:
        without_fragment = str(base / without_fragment)
    return _collapse_posix(without_fragment)


VOID_TAGS = frozenset(
    {
        "area",
        "base",
        "br",
        "col",
        "embed",
        "hr",
        "img",
        "input",
        "link",
        "meta",
        "param",
        "source",
        "track",
        "wbr",
    }
)
_SELF_CLOSING = re.compile(rb"<([A-Za-z][\w:.-]*)((?:\s+[^<>]*?)?)\s*/>")


def expand_self_closing(raw: bytes) -> bytes:
    """XHTML content documents self-close non-void elements (`<a id="p12"/>`); an HTML5 parser
    treats those as open tags and swallows the following siblings, which moves text under the
    wrong ancestor (a hidden page-number span in one retained book). Expand them first."""

    def fix(match: re.Match[bytes]) -> bytes:
        name = match.group(1)
        if name.lower().decode("ascii", "replace") in VOID_TAGS:
            return match.group(0)
        return b"<" + name + match.group(2) + b"></" + name + b">"

    return _SELF_CLOSING.sub(fix, raw)


OPF_NS = "http://www.idpf.org/2007/opf"
DC_NS = "http://purl.org/dc/elements/1.1/"


@dataclass(frozen=True)
class ManifestItem:
    id: str
    href: str
    """Unquoted, relative to the package document's directory."""
    media_type: str
    properties: frozenset[str] = frozenset()


@dataclass
class EpubPackage:
    opf_path: str
    title: str | None
    author: str | None
    language: str | None
    items: dict[str, ManifestItem] = field(default_factory=dict)
    spine: list[str] = field(default_factory=list)
    """Every itemref's idref in order, linear or not, as the previous reader exposed them."""
    toc_idref: str | None = None

    @property
    def opf_dir(self) -> PurePosixPath:
        return PurePosixPath(self.opf_path).parent

    def archive_path(self, href: str) -> str:
        """Archive member for a package-relative href (fragment and query dropped)."""
        return _normalize_href(href, self.opf_dir)

    def nav_item(self) -> ManifestItem | None:
        return next((item for item in self.items.values() if "nav" in item.properties), None)

    def toc_item(self) -> ManifestItem | None:
        return self.items.get(self.toc_idref) if self.toc_idref else None


def _xml(archive: zipfile.ZipFile, name: str) -> ET.Element:
    try:
        return ET.fromstring(read_member(archive, name))
    except (KeyError, ET.ParseError, ValueError) as exc:
        raise IngestError("ERR_OPEN", f"cannot parse epub: {exc}") from exc


def _tag(element: ET.Element) -> str:
    return str(element.tag).rsplit("}", 1)[-1].lower()


def _first_dc_text(metadata: ET.Element, name: str) -> str | None:
    for element in metadata:
        if element.tag == f"{{{DC_NS}}}{name}" and element.text and element.text.strip():
            return element.text.strip()
    return None


def read_package(archive: zipfile.ZipFile) -> EpubPackage:
    opf_path = _opf_path(archive)
    if opf_path is None:
        raise IngestError("ERR_OPEN", "cannot parse epub: no package document")
    root = _xml(archive, opf_path)
    package = EpubPackage(opf_path=opf_path, title=None, author=None, language=None)
    for child in root:
        kind = _tag(child)
        if kind == "metadata":
            package.title = _first_dc_text(child, "title")
            package.author = _first_dc_text(child, "creator")
            package.language = _first_dc_text(child, "language")
        elif kind == "manifest":
            for item in child:
                item_id, href = item.get("id"), item.get("href")
                if _tag(item) != "item" or not item_id or not href:
                    continue
                package.items[item_id] = ManifestItem(
                    id=item_id,
                    href=unquote(href),
                    media_type=item.get("media-type") or "",
                    properties=frozenset((item.get("properties") or "").split()),
                )
        elif kind == "spine":
            package.toc_idref = child.get("toc") or None
            package.spine = [
                str(ref.get("idref"))
                for ref in child
                if _tag(ref) == "itemref" and ref.get("idref")
            ]
    return package


def _ncx_toc(archive: zipfile.ZipFile, package: EpubPackage) -> list[tuple[str | None, str]]:
    """Flattened navMap in document order, parents before children; `src` kept raw."""
    item = package.toc_item()
    if item is None:
        return []
    root = _xml(archive, package.archive_path(item.href))
    flat: list[tuple[str | None, str]] = []

    def walk(parent: ET.Element) -> None:
        for point in parent:
            if _tag(point) != "navpoint":
                continue
            label: str | None = None
            src = ""
            for part in point:
                if _tag(part) == "navlabel":
                    text = next(iter(part), None)
                    label = text.text if text is not None else None
                elif _tag(part) == "content":
                    src = part.get("src", "")
            flat.append((label, src))
            walk(point)

    for element in root.iter():
        if _tag(element) == "navmap":
            walk(element)
            break
    return flat


NON_TOC_NAV_TYPES = frozenset({"landmarks", "page-list", "lot", "loi", "loa", "lov"})


def _toc_nav_node(tree: HTMLParser) -> Node | None:
    anchored = [nav for nav in tree.css("nav") if nav.css_first("a[href]") is not None]
    for nav in anchored:
        epub_type = _attr(nav, "type") or ""
        if "toc" in epub_type.split():
            return nav
    for nav in anchored:
        nav_types = set((_attr(nav, "type") or "").split())
        if nav_types & NON_TOC_NAV_TYPES:
            continue
        return nav
    return None


def _nav_toc(archive: zipfile.ZipFile, package: EpubPackage) -> list[tuple[str | None, str]]:
    """The nav document's `toc` list, hrefs joined onto the nav document's directory."""
    item = package.nav_item()
    if item is None:
        return []
    base = PurePosixPath(_collapse_posix(item.href)).parent
    try:
        tree = HTMLParser(
            expand_self_closing(read_member(archive, package.archive_path(item.href)))
        )
    except KeyError:
        return []
    nav = next((n for n in tree.css("nav") if "toc" in (n.attributes or {}).values()), None)
    if nav is None:
        return []
    flat: list[tuple[str | None, str]] = []

    def walk(list_node: Node) -> None:
        for li in list_node.iter():
            if li.tag != "li":
                continue
            sub = li.css_first("ol")
            link = li.css_first("a")
            href = link.attributes.get("href") if link is not None else None
            if sub is not None:
                first = next(iter(li.iter()), None)
                title = first.text() if first is not None else ""
                if href:
                    flat.append((title, _normalize_href(href, base)))
                walk(sub)
            elif href:
                flat.append((link.text() if link is not None else "", _normalize_href(href, base)))

    root_list = nav.css_first("ol")
    if root_list is not None:
        walk(root_list)
    return flat


def _toc_from_nav_document(
    archive: zipfile.ZipFile, package: EpubPackage
) -> tuple[list[tuple[str, str]], PurePosixPath]:
    """Lenient fallback: any anchored nav in the nav document, toc-typed first."""
    item = package.nav_item()
    if item is None:
        return [], PurePosixPath()
    base = PurePosixPath(_collapse_posix(item.href)).parent
    try:
        tree = HTMLParser(
            expand_self_closing(read_member(archive, package.archive_path(item.href)))
        )
    except KeyError:
        return [], PurePosixPath()
    nav = _toc_nav_node(tree)
    if nav is None:
        return [], PurePosixPath()
    entries = [
        (a.text(strip=True), str(a.attributes.get("href")))
        for a in nav.css("a[href]")
        if a.attributes.get("href") and a.text(strip=True)
    ]
    return entries, base


def _toc_bases(package: EpubPackage) -> list[PurePosixPath]:
    bases = [PurePosixPath()]
    for item in (package.nav_item(), package.toc_item()):
        if item is None:
            continue
        parent = PurePosixPath(_collapse_posix(item.href)).parent
        if parent not in bases:
            bases.append(parent)
    return bases


def _resolve_spine_index(
    href: str, bases: list[PurePosixPath], spine_index: dict[str, int]
) -> int | None:
    direct = spine_index.get(_normalize_href(href))
    if direct is not None:
        return direct
    for base in bases:
        resolved = spine_index.get(_normalize_href(href, base))
        if resolved is not None:
            return resolved
    return None


def parse_epub_structure(book_path: str) -> EpubStructure:
    check_epub_drm(book_path)
    try:
        with zipfile.ZipFile(book_path) as archive:
            package = read_package(archive)
            spine_hrefs = [
                _normalize_href(package.items[idref].href)
                for idref in package.spine
                if idref in package.items
            ]
            if not spine_hrefs:
                raise IngestError("ERR_EMPTY", "epub has no spine documents")
            flat = [(t, h) for t, h in _ncx_toc(archive, package) if isinstance(t, str) and h]
            if not flat:
                flat = [(t, h) for t, h in _nav_toc(archive, package) if isinstance(t, str) and h]
            bases = _toc_bases(package)
            if not flat:
                flat, nav_base = _toc_from_nav_document(archive, package)
                bases = [nav_base, *bases]
    except (zipfile.BadZipFile, OSError) as exc:
        raise IngestError("ERR_OPEN", f"cannot open file: {exc}") from exc
    spine_index: dict[str, int] = {}
    for index, href in enumerate(spine_hrefs):
        spine_index.setdefault(href, index)
    toc: list[TocEntry] = []
    for title, href in flat:
        resolved = _resolve_spine_index(href, bases, spine_index)
        if resolved is None:
            continue
        toc.append(TocEntry(title=title.strip() or f"Chapter {len(toc) + 1}", spine_index=resolved))
    return EpubStructure(
        title=package.title,
        author=package.author,
        language=package.language,
        spine_hrefs=spine_hrefs,
        toc=toc,
    )
