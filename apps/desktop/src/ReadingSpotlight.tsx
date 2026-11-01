import type { SpotlightPlan } from "./teacherAnchorModel";

// The teacher-mode spotlight (VOICE_AGENT §11.1, owner-approved style): a
// subtle outline plus a very light dim of the surrounding content. Region
// anchors (figure/equation/table — the layout stage's region boxes) get
// outline + dim; section anchors outline the heading block only, with no
// surrounding dim. Purely token-coloured: the outline is reading-frame (the
// figure-frame pairing already pinned ≥3:1 against reading-page in
// contrast.test.ts — 3.73:1 light, 5.26:1 dark), and the dim is the page's
// own colour at 35% alpha, a veil that washes surroundings toward the paper
// in both themes while even maximal-contrast content stays ≥6.8:1 through
// it. Never any text highlighting: a live tutor session explains, it does
// not read.

interface ReadingSpotlightProps {
  plan: SpotlightPlan | null;
  /** The outline element, for scrolling the anchor into view on sync. */
  outlineRef?: React.RefObject<HTMLDivElement | null>;
}

interface Edges {
  leftPct: number;
  topPct: number;
  rightPct: number;
  bottomPct: number;
}

function edgesOf(plan: SpotlightPlan): Edges {
  return {
    leftPct: plan.box.leftPct,
    topPct: plan.box.topPct,
    rightPct: plan.box.leftPct + plan.box.widthPct,
    bottomPct: plan.box.topPct + plan.box.heightPct,
  };
}

const DIM_CLASS = "absolute bg-reading-page/35";

function ReadingSpotlight({ plan, outlineRef }: ReadingSpotlightProps) {
  if (plan == null) return null;
  const edges = edgesOf(plan);
  return (
    <div
      data-testid="reading-spotlight"
      data-shape={plan.shape}
      className="animate-fade-in pointer-events-none absolute inset-0"
    >
      {plan.shape === "region" && (
        <>
          <div
            data-testid="reading-spotlight-dim"
            className={DIM_CLASS}
            style={{ left: 0, top: 0, right: 0, height: `${edges.topPct}%` }}
          />
          <div
            data-testid="reading-spotlight-dim"
            className={DIM_CLASS}
            style={{ left: 0, top: `${edges.bottomPct}%`, right: 0, bottom: 0 }}
          />
          <div
            data-testid="reading-spotlight-dim"
            className={DIM_CLASS}
            style={{
              left: 0,
              top: `${edges.topPct}%`,
              width: `${edges.leftPct}%`,
              height: `${plan.box.heightPct}%`,
            }}
          />
          <div
            data-testid="reading-spotlight-dim"
            className={DIM_CLASS}
            style={{
              left: `${edges.rightPct}%`,
              top: `${edges.topPct}%`,
              right: 0,
              height: `${plan.box.heightPct}%`,
            }}
          />
        </>
      )}
      <div
        ref={outlineRef}
        data-testid="reading-spotlight-outline"
        className="absolute rounded-xs border-2 border-reading-frame"
        style={{
          left: `${plan.box.leftPct}%`,
          top: `${plan.box.topPct}%`,
          width: `${plan.box.widthPct}%`,
          height: `${plan.box.heightPct}%`,
        }}
      />
    </div>
  );
}

export default ReadingSpotlight;
