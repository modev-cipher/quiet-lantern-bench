import hashlib
import math
from dataclasses import dataclass, field

Bbox = tuple[float, float, float, float]

CHART_CLASSES = frozenset({"bar_chart", "line_chart", "pie_chart", "flow_chart"})
DECORATIVE_CLASSES = frozenset({"logo", "icon", "signature", "stamp", "qr_code", "bar_code"})

_TYPE_BY_LABEL = {
    "picture": "figure",
    "formula": "equation",
    "table": "table",
}

DECORATIVE_AREA_RATIO = 0.005
DECORATIVE_BAND_RATIO = 0.05
MERGE_IOU_THRESHOLD = 0.6
CLASSIFIER_MATCH_IOU = 0.5


@dataclass
class Region:
    page: int
    bbox: Bbox
    type: str
    confidence: float
    crop_png_path: str | None = None
    meta: dict[str, int] | None = None
    id: str = field(default="")

    def to_json(self) -> dict[str, object]:
        payload: dict[str, object] = {
            "id": self.id,
            "page": self.page,
            "bbox": list(self.bbox),
            "type": self.type,
            "confidence": self.confidence,
            "crop_png_path": self.crop_png_path,
        }
        if self.meta is not None:
            payload["meta"] = dict(self.meta)
        return payload


def region_type_for_label(label: str) -> str | None:
    return _TYPE_BY_LABEL.get(label)


def refine_with_classifier(class_name: str) -> str:
    if class_name in CHART_CLASSES:
        return "chart"
    if class_name in DECORATIVE_CLASSES:
        return "decorative"
    return "figure"


def is_decorative_bbox(bbox: Bbox, page_width: float, page_height: float) -> bool:
    assert math.isfinite(page_width) and math.isfinite(page_height), (
        "page dimensions must be finite"
    )
    assert page_width > 0 and page_height > 0, "page dimensions must be positive"
    x0, y0, x1, y1 = bbox
    assert all(math.isfinite(v) for v in (x0, y0, x1, y1)), (
        f"bbox values must be finite, got {bbox}"
    )
    assert x0 <= x1 and y0 <= y1, f"bbox coordinates must satisfy x0 <= x1 and y0 <= y1, got {bbox}"
    area = max(0.0, x1 - x0) * max(0.0, y1 - y0)
    if area < DECORATIVE_AREA_RATIO * page_width * page_height:
        return True
    band = DECORATIVE_BAND_RATIO * page_height
    return y1 <= band or y0 >= page_height - band


def iou(a: Bbox, b: Bbox) -> float:
    ix0 = max(a[0], b[0])
    iy0 = max(a[1], b[1])
    ix1 = min(a[2], b[2])
    iy1 = min(a[3], b[3])
    if ix1 <= ix0 or iy1 <= iy0:
        return 0.0
    inter = (ix1 - ix0) * (iy1 - iy0)
    area_a = (a[2] - a[0]) * (a[3] - a[1])
    area_b = (b[2] - b[0]) * (b[3] - b[1])
    union = area_a + area_b - inter
    if union <= 0:
        return 0.0
    return inter / union


def _union(a: Bbox, b: Bbox) -> Bbox:
    return (min(a[0], b[0]), min(a[1], b[1]), max(a[2], b[2]), max(a[3], b[3]))


def merge_overlapping(regions: list[Region]) -> list[Region]:
    merged = [r for r in regions]
    changed = True
    while changed:
        changed = False
        for i in range(len(merged)):
            for j in range(i + 1, len(merged)):
                a, b = merged[i], merged[j]
                if a.page != b.page or a.type != b.type:
                    continue
                if iou(a.bbox, b.bbox) <= MERGE_IOU_THRESHOLD:
                    continue
                keep, drop = (a, b) if a.confidence >= b.confidence else (b, a)
                keep.bbox = _union(a.bbox, b.bbox)
                if keep.meta is None and drop.meta is not None:
                    keep.meta = drop.meta
                merged.pop(j if keep is a else i)
                changed = True
                break
            if changed:
                break
    return merged


def best_match(bbox: Bbox, candidates: list[tuple[Bbox, object]]) -> object | None:
    best: object | None = None
    best_iou = CLASSIFIER_MATCH_IOU
    for cand_bbox, value in candidates:
        score = iou(bbox, cand_bbox)
        if score >= best_iou:
            best_iou = score
            best = value
    return best


def bbox_canonical(bbox: Bbox) -> str:
    return ",".join(f"{v:.2f}" for v in bbox)


def region_id(content_hash: str, page: int, bbox: Bbox, region_type: str) -> str:
    raw = f"{content_hash}:{page}:{bbox_canonical(bbox)}:{region_type}"
    return hashlib.sha256(raw.encode("utf-8")).hexdigest()[:16]
