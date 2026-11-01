import type { ReadingPage, ReadingSegment } from "./readingModel";
import { tapAtFraction, type TapOutcome } from "./readingPageModel";

interface ReadingTapLayerProps {
  segments: ReadingSegment[];
  pageNo: number | null;
  page: ReadingPage | null;
  onTap: (outcome: TapOutcome) => void;
}

function ReadingTapLayer({ segments, pageNo, page, onTap }: ReadingTapLayerProps) {
  const onClick = (event: React.MouseEvent<HTMLDivElement>) => {
    const box = event.currentTarget.getBoundingClientRect();
    const fx = (event.clientX - box.left) / box.width;
    const fy = (event.clientY - box.top) / box.height;
    onTap(tapAtFraction(segments, pageNo, page, fx, fy));
  };

  return (
    <div
      data-testid="reading-tap-layer"
      aria-hidden="true"
      className="absolute inset-0"
      onClick={onClick}
    />
  );
}

export default ReadingTapLayer;
