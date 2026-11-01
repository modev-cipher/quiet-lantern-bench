// The Book Agent's visible citation (charter §4.5, B-4): a distinct visual
// channel from the tutor spotlight and from narration highlighting — outline
// plus a light surrounding dim over the cited passage, never text-highlight
// vocabulary, never pointer interception. Structure pins mirror
// readingSpotlight.test.ts.

import type React from "react";

import type { AgentCitationPlan } from "./agentCitationApplier";
import { useI18n } from "./i18n";

function pct(value: number): string {
  return `${value}%`;
}

function AgentCitationOverlay({
  plan,
  onDismiss,
  boxRef,
}: {
  plan: AgentCitationPlan | null;
  onDismiss: () => void;
  boxRef?: React.Ref<HTMLDivElement>;
}) {
  const { t } = useI18n();
  if (plan == null) return null;
  const { box } = plan;
  const right = box.leftPct + box.widthPct;
  const bottom = box.topPct + box.heightPct;
  return (
    <div
      data-testid="agent-citation"
      className="pointer-events-none absolute inset-0"
      role="status"
      aria-live="polite"
    >
      {/* Text content (not just a label) so screen readers announce the
          citation (§5.2: cited chapter/page exposed to AT). */}
      <span className="sr-only">
        {t("reader.citedPassage", {
          page: plan.pageNo,
          chapter: plan.chapter + 1,
        })}
      </span>
      {/* Four-rect surrounding dim, the page's own colour at low alpha. */}
      {[
        { left: 0, top: 0, width: 100, height: box.topPct },
        { left: 0, top: bottom, width: 100, height: 100 - bottom },
        { left: 0, top: box.topPct, width: box.leftPct, height: box.heightPct },
        {
          left: right,
          top: box.topPct,
          width: 100 - right,
          height: box.heightPct,
        },
      ].map((rect, i) => (
        <div
          key={i}
          data-testid="agent-citation-dim"
          className="absolute bg-ink-950/10"
          style={{
            left: pct(rect.left),
            top: pct(rect.top),
            width: pct(rect.width),
            height: pct(rect.height),
          }}
        />
      ))}
      <div
        ref={boxRef}
        data-testid="agent-citation-outline"
        className="absolute rounded-md border-2 border-accent-500/80"
        style={{
          left: pct(box.leftPct),
          top: pct(box.topPct),
          width: pct(box.widthPct),
          height: pct(box.heightPct),
        }}
      >
        <button
          type="button"
          data-testid="agent-citation-dismiss"
          aria-label={t("reader.dismissCitation")}
          onClick={onDismiss}
          className={
            /* i18n-physical-ok: page-coordinate overlay */ "pointer-events-auto absolute top-0.5 right-0.5 flex h-5 w-5 items-center justify-center rounded-full border border-ink-700 bg-ink-900/90 text-label leading-none text-ink-200 transition-colors duration-200 hover:text-ink-50"
          }
        >
          ×
        </button>
      </div>
    </div>
  );
}

export default AgentCitationOverlay;
