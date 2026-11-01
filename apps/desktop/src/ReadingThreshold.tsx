import { useEffect, useState } from "react";

import TypographicCover from "./TypographicCover";
import {
  THRESHOLD_MS,
  enterTransform,
  peekThreshold,
  resetThreshold,
  type ThresholdSource,
} from "./thresholdModel";

function calm(): boolean {
  if (typeof window === "undefined" || window.matchMedia == null) return false;
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

function ReadingThreshold() {
  const [source] = useState<ThresholdSource | null>(() =>
    calm() ? null : peekThreshold(),
  );

  useEffect(resetThreshold, []);
  const [start, setStart] = useState(() =>
    source == null
      ? null
      : enterTransform(source.rect, {
          width: window.innerWidth,
          height: window.innerHeight,
        }),
  );

  useEffect(() => {
    if (start == null) return;
    const frame = requestAnimationFrame(() => setStart(null));
    return () => cancelAnimationFrame(frame);
  }, [start]);

  const [gone, setGone] = useState(false);
  useEffect(() => {
    if (source == null) return;
    const timer = setTimeout(() => setGone(true), THRESHOLD_MS + 60);
    return () => clearTimeout(timer);
  }, [source]);

  if (source == null || gone) return null;

  return (
    <div
      aria-hidden="true"
      data-testid="reading-threshold"
      data-arrived={start == null ? "yes" : "no"}
      className="pointer-events-none fixed inset-0 z-10 origin-top-left will-change-transform"
      style={{
        transform: start ?? "none",
        opacity: start == null ? 0 : 1,
        transition: `transform ${THRESHOLD_MS}ms cubic-bezier(0.32, 0.72, 0, 1), opacity ${THRESHOLD_MS}ms cubic-bezier(0.32, 0.72, 0, 1)`,
      }}
    >
      <TypographicCover
        seed={source.seed}
        title={source.title}
        size="lg"
      />
    </div>
  );
}

export default ReadingThreshold;
