export const WORD_ATTRIBUTE = "data-word";
export const FILL_PROPERTY = "--reading-fill";
export const HIGHLIGHT_CLASSES = [
  "bg-reading-highlight",
  "text-reading-highlight-fg",
] as const;

export interface HighlightTargets {
  container: HTMLElement | null;
  underline: HTMLElement | null;
}

export interface HighlightApplier {
  apply: (
    targets: HighlightTargets,
    wordIdx: number | null,
    fill: number,
  ) => void;
  reset: () => void;
}

export function createHighlightApplier(): HighlightApplier {
  let previous: HTMLElement | null = null;

  function clear(): void {
    previous?.classList.remove(...HIGHLIGHT_CLASSES);
    previous = null;
  }

  return {
    apply(targets, wordIdx, fill) {
      targets.underline?.style.setProperty(FILL_PROPERTY, String(fill));
      const next =
        targets.container == null || wordIdx == null
          ? null
          : targets.container.querySelector<HTMLElement>(
              `[${WORD_ATTRIBUTE}="${wordIdx}"]`,
            );
      if (next === previous) return;
      previous?.classList.remove(...HIGHLIGHT_CLASSES);
      next?.classList.add(...HIGHLIGHT_CLASSES);
      previous = next;
    },
    reset: clear,
  };
}
