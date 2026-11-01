import { useRef, useSyncExternalStore } from "react";
import {
  castDraftSnapshot,
  clearCastDraft,
  subscribeCastDrafts,
} from "./castDraftStore";
import { goToCastBook } from "./views";
import { useI18n } from "./i18n";

export default function CastDraftReturn() {
  const drafts = useSyncExternalStore(subscribeCastDrafts, castDraftSnapshot);
  const { t, tx } = useI18n();
  const containerRef = useRef<HTMLElement>(null);
  const discard = (bookId: string) => {
    clearCastDraft(bookId);
    requestAnimationFrame(() => {
      const target =
        containerRef.current?.querySelector<HTMLElement>(
          "[data-testid='cast-draft-return']",
        ) ?? document.getElementById("main-content");
      target?.focus({ preventScroll: true });
    });
  };
  if (drafts.length === 0) return null;
  return (
    <aside
      ref={containerRef}
      className="mx-5 mt-4 flex flex-col gap-3 rounded-xl border border-ember-500/30 bg-ink-900 p-4"
      aria-label={t("cast.draft.saved")}
    >
      {drafts.map((draft) => (
        <div key={draft.bookId} className="flex flex-wrap items-center gap-3">
          <p className="min-w-0 flex-1 break-words [overflow-wrap:anywhere] text-body text-ink-300">
            {tx("cast.draft.forBook", { title: draft.title })}
          </p>
          <button
            type="button"
            data-testid="cast-draft-return"
            aria-label={t("cast.draft.returnFor", { title: draft.title })}
            onClick={() => goToCastBook(draft.bookId)}
            className="rounded-lg bg-ink-50 px-3 py-2 text-secondary font-semibold text-ink-950"
          >
            {t("cast.draft.return")}
          </button>
          <button
            type="button"
            data-testid={`cast-draft-discard-${draft.bookId}`}
            aria-label={t("cast.draft.discardFor", { title: draft.title })}
            onClick={() => discard(draft.bookId)}
            className="rounded-lg px-3 py-2 text-secondary text-ink-400"
          >
            {t("cast.draft.discard")}
          </button>
        </div>
      ))}
    </aside>
  );
}
