import { useEffect, useId, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useI18n } from "./i18n";
import { PageIcon } from "./Icons";

export interface PagePeekData {
  pageNo: number;
  chapterTitle: string | null;
  text: string;
  clipped: boolean;
}

type PeekState =
  | { kind: "loading" }
  | { kind: "ready"; peek: PagePeekData }
  | { kind: "empty" };

const cache = new Map<string, Promise<PagePeekData | null>>();

export function loadPagePeek(
  bookId: string,
  jobId: string | null,
  pageNo: number,
): Promise<PagePeekData | null> {
  const key = `${bookId}\u0000${jobId ?? ""}\u0000${pageNo}`;
  let pending = cache.get(key);
  if (pending == null) {
    pending = invoke<PagePeekData | null>("voice_page_peek", {
      bookId,
      jobId,
      pageNo,
    }).catch(() => {
      cache.delete(key);
      return null;
    });
    cache.set(key, pending);
  }
  return pending;
}

export function forgetPagePeeks(): void {
  cache.clear();
}

function PeekBody({
  bookId,
  jobId,
  pageNo,
  id,
}: {
  bookId: string;
  jobId: string | null;
  pageNo: number;
  id: string;
}) {
  const { t } = useI18n();
  const [state, setState] = useState<PeekState>({ kind: "loading" });
  useEffect(() => {
    let live = true;
    setState({ kind: "loading" });
    void loadPagePeek(bookId, jobId, pageNo).then((peek) => {
      if (live) setState(peek == null ? { kind: "empty" } : { kind: "ready", peek });
    });
    return () => {
      live = false;
    };
  }, [bookId, jobId, pageNo]);

  return (
    <div
      id={id}
      role="region"
      aria-label={t("voice.peek.page", { page: pageNo })}
      data-testid="tutor-page-peek"
      className="ui-peek-slip"
    >
      <p className="flex items-center justify-between gap-3 font-mono text-label text-ink-500">
        <span className="min-w-0 truncate" dir="auto">
          {state.kind === "ready" ? state.peek.chapterTitle ?? "" : ""}
        </span>
        <span className="shrink-0">{t("voice.peek.page", { page: pageNo })}</span>
      </p>
      {state.kind === "loading" && (
        <p className="text-label text-ink-500">{t("voice.peek.loading")}</p>
      )}
      {state.kind === "empty" && (
        <p className="text-label text-ink-500">{t("voice.peek.empty")}</p>
      )}
      {state.kind === "ready" && (
        <p
          dir="auto"
          data-clipped={state.peek.clipped ? "true" : undefined}
          className="ui-peek-text text-start"
        >
          {state.peek.text}
          {state.peek.clipped ? "…" : ""}
        </p>
      )}
    </div>
  );
}

export default function PagePeeks({
  bookId,
  jobId,
  pages,
}: {
  bookId: string;
  jobId: string | null;
  pages: readonly number[];
}) {
  const { t } = useI18n();
  const [open, setOpen] = useState<number | null>(null);
  const base = useId();
  if (pages.length === 0) return null;
  return (
    <div
      className="mt-2 flex flex-col gap-2"
      onKeyDown={(event) => {
        if (event.key === "Escape" && open != null) {
          event.stopPropagation();
          setOpen(null);
        }
      }}
    >
      <ul
        aria-label={t("voice.peek.label")}
        data-testid="tutor-page-pills"
        className="flex flex-wrap items-center gap-1.5"
      >
        {pages.map((page) => (
          <li key={page}>
            <button
              type="button"
              className="ui-page-pill"
              data-testid="tutor-page-pill"
              aria-expanded={open === page}
              aria-controls={open === page ? `${base}-${page}` : undefined}
              aria-label={t("voice.peek.open", { page })}
              onClick={() => setOpen((current) => (current === page ? null : page))}
            >
              <PageIcon className="h-3.5 w-3.5" />
              <bdi>{t("voice.peek.pill", { page })}</bdi>
            </button>
          </li>
        ))}
      </ul>
      {open != null && (
        <PeekBody bookId={bookId} jobId={jobId} pageNo={open} id={`${base}-${open}`} />
      )}
    </div>
  );
}
