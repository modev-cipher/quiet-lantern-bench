import { useEffect, useState } from "react";

import BrandMark from "./BrandMark";

const INTRO_FALLBACK_MS = 2000;
const INTRO_REVEAL_MS = 1010;

let revealAt = 0;

export function introRemainingMs(): number {
  return Math.max(0, Math.round(revealAt - performance.now()));
}

function startIntro(): boolean {
  const allowed = motionAllowed();
  if (allowed) revealAt = performance.now() + INTRO_REVEAL_MS;
  return allowed;
}

function motionAllowed(): boolean {
  return !window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

function LaunchIntro({ landing }: { landing: "corner" | "fade" }) {
  const [playing, setPlaying] = useState(startIntro);

  useEffect(() => {
    if (!playing) return;
    const timer = window.setTimeout(() => setPlaying(false), INTRO_FALLBACK_MS);
    return () => window.clearTimeout(timer);
  }, [playing]);

  if (!playing) return null;

  return (
    <div
      data-testid="launch-intro"
      data-landing={landing}
      aria-hidden="true"
      className="launch-intro pointer-events-none fixed inset-0 z-[60]"
    >
      <div className="launch-veil absolute inset-0 bg-ink-950" />
      <div className="launch-glow absolute inset-0" />
      <div
        data-testid="launch-intro-mark"
        className={`absolute start-3.5 top-4 ${
          landing === "fade" ? "launch-mark-fade" : "launch-mark"
        }`}
        onAnimationEnd={() => setPlaying(false)}
      >
        <BrandMark />
      </div>
    </div>
  );
}

export default LaunchIntro;
