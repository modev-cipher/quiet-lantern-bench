import json
from dataclasses import dataclass

from txtcaster_engine.ingest.blocks import TextBlock
from txtcaster_engine.ingest.errors import IngestError


@dataclass(frozen=True)
class CanonicalPage:
    page_no: int
    width_pt: float
    height_pt: float
    blocks: list[TextBlock]


def _as_float(value: object) -> float:
    if isinstance(value, (int, float)):
        return float(value)
    if isinstance(value, str):
        try:
            return float(value)
        except ValueError:
            return 0.0
    return 0.0


def _as_int(value: object) -> int:
    if isinstance(value, bool):
        return 0
    if isinstance(value, (int, float)):
        return int(value)
    if isinstance(value, str):
        try:
            return int(value)
        except ValueError:
            return 0
    return 0


def _block_from_json(raw: dict[str, object]) -> TextBlock:
    bbox = raw.get("bbox")
    if not isinstance(bbox, list) or len(bbox) != 4:
        raise IngestError("ERR_BLOCKS_INVALID", "block bbox must be four numbers")
    return TextBlock(
        bbox=(_as_float(bbox[0]), _as_float(bbox[1]), _as_float(bbox[2]), _as_float(bbox[3])),
        text=str(raw.get("text", "")),
        font_size=_as_float(raw.get("font_size")),
        role="body",
        reading_order=_as_int(raw.get("reading_order")),
        column=_as_int(raw.get("column")),
        label=str(raw["label"]) if raw.get("label") is not None else None,
    )


def parse_pages(payloads: list[dict[str, object]]) -> list[CanonicalPage]:
    pages: list[CanonicalPage] = []
    for payload in payloads:
        raw_text = payload.get("text_json")
        if not isinstance(raw_text, str) or not raw_text.strip():
            raise IngestError(
                "ERR_BLOCKS_MISSING",
                f"page {payload.get('page_no')} has no stored blocks",
            )
        try:
            envelope = json.loads(raw_text)
        except json.JSONDecodeError as exc:
            raise IngestError("ERR_BLOCKS_INVALID", f"unreadable block envelope: {exc}") from exc
        raw_blocks = envelope.get("blocks")
        if not isinstance(raw_blocks, list):
            raise IngestError("ERR_BLOCKS_INVALID", "block envelope has no blocks array")
        pages.append(
            CanonicalPage(
                page_no=_as_int(payload.get("page_no")),
                width_pt=_as_float(payload.get("width_pt")),
                height_pt=_as_float(payload.get("height_pt")),
                blocks=[_block_from_json(block) for block in raw_blocks],
            )
        )
    return pages
