import type { OverlayBox } from "./readingPageModel";

interface ReadingPageOverlayProps {
  boxes: OverlayBox[];
  wordRef: React.RefObject<HTMLDivElement | null>;
  fallbackRef: React.RefObject<HTMLDivElement | null>;
}

function style(box: OverlayBox): React.CSSProperties {
  return {
    left: `${box.leftPct}%`,
    top: `${box.topPct}%`,
    width: `${box.widthPct}%`,
    height: `${box.heightPct}%`,
  };
}

function ReadingPageOverlay({
  boxes,
  wordRef,
  fallbackRef,
}: ReadingPageOverlayProps) {
  return (
    <div
      data-testid="reading-page-overlay"
      data-boxes={boxes.length}
      className="pointer-events-none absolute inset-0"
    >
      <div
        ref={fallbackRef}
        data-testid="reading-page-fallback"
        className="absolute inset-0"
        hidden
      >
        {boxes.map((box, position) => (
          <div
            key={position}
            data-testid="reading-page-fallback-box"
            className="absolute rounded-xs bg-reading-highlight/25"
            style={style(box)}
          />
        ))}
      </div>
      <div
        ref={wordRef}
        data-testid="reading-page-word"
        hidden
        className="absolute rounded-xs bg-reading-highlight/60"
      />
    </div>
  );
}

export default ReadingPageOverlay;
