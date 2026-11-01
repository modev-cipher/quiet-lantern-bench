import type { OverlayBox, OverlayPlan } from "./readingPageModel";

export interface PageOverlayTargets {
  word: HTMLElement | null;
  fallback: HTMLElement | null;
}

export interface PageOverlayApplier {
  apply: (targets: PageOverlayTargets, plan: OverlayPlan) => void;
  reset: () => void;
}

export function boxSignature(plan: OverlayPlan): string {
  if (plan.kind !== "word") return plan.kind;
  const box = plan.boxes[0];
  if (box == null) return "none";
  return `word:${box.leftPct}:${box.topPct}:${box.widthPct}:${box.heightPct}`;
}

function place(element: HTMLElement, box: OverlayBox): void {
  element.style.left = `${box.leftPct}%`;
  element.style.top = `${box.topPct}%`;
  element.style.width = `${box.widthPct}%`;
  element.style.height = `${box.heightPct}%`;
}

export function createPageOverlayApplier(): PageOverlayApplier {
  let previous: string | null = null;
  let appliedTo: PageOverlayTargets = { word: null, fallback: null };

  return {
    apply(targets, plan) {
      const signature = boxSignature(plan);
      const sameNodes =
        targets.word === appliedTo.word &&
        targets.fallback === appliedTo.fallback;
      if (sameNodes && signature === previous) return;
      previous = signature;
      appliedTo = { word: targets.word, fallback: targets.fallback };
      const box = plan.kind === "word" ? plan.boxes[0] : undefined;
      if (targets.word != null) {
        if (box == null) {
          targets.word.hidden = true;
        } else {
          place(targets.word, box);
          targets.word.hidden = false;
        }
      }
      if (targets.fallback != null) {
        targets.fallback.hidden = plan.kind !== "segment";
      }
    },
    reset() {
      previous = null;
      appliedTo = { word: null, fallback: null };
    },
  };
}
