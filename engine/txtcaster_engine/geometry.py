Bbox = tuple[float, float, float, float]

PAGE_CONTAINMENT_TOLERANCE_PT = 4.0


def bbox_area(bbox: Bbox) -> float:
    x0, y0, x1, y1 = bbox
    return max(0.0, x1 - x0) * max(0.0, y1 - y0)


def intersection_area(first: Bbox, second: Bbox) -> float:
    ax0, ay0, ax1, ay1 = first
    bx0, by0, bx1, by1 = second
    width = min(ax1, bx1) - max(ax0, bx0)
    height = min(ay1, by1) - max(ay0, by0)
    return max(0.0, width) * max(0.0, height)


def block_coverage(inner: Bbox, outer: Bbox) -> float:
    area = bbox_area(inner)
    if area <= 0.0:
        return 0.0
    return intersection_area(inner, outer) / area


def bbox_within_page(
    bbox: Bbox,
    page_width: float,
    page_height: float,
    tolerance: float = PAGE_CONTAINMENT_TOLERANCE_PT,
) -> bool:
    if page_width <= 0.0 or page_height <= 0.0:
        return False
    x0, y0, x1, y1 = bbox
    return (
        x0 >= -tolerance
        and y0 >= -tolerance
        and x1 <= page_width + tolerance
        and y1 <= page_height + tolerance
    )
