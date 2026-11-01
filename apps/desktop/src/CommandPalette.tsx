import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import {
  BooksIcon,
  ChatIcon,
  HeadphonesIcon,
  HelpCircleIcon,
  PlayIcon,
  SearchIcon,
  SettingsIcon,
  WaveIcon,
} from "./Icons";
import { buildAudioShelf, type AudioShelfItem } from "./audioShelfModel";
import { castScopeLabel, castScopeOf } from "./castScope";
import { bookAvailability } from "./libraryAvailability";
import {
  moveSelection,
  rankPalette,
  type PaletteEntry,
  type PaletteGroup,
  type PaletteTarget,
} from "./commandPaletteModel";
import {
  bookDisplayAuthor,
  bookDisplayTitle,
  type BookSummary,
} from "./libraryModel";
import type { ExportSummary } from "./audioShelfModel";
import type { ReadingIndex } from "./readingModel";
import { usePlayback } from "./usePlayback";
import { useI18n } from "./i18n";
import { useDialogFocus } from "./useDialogFocus";

type Sources = {
  books: BookSummary[];
  shelf: AudioShelfItem[];
  index: ReadingIndex | null;
};

const EMPTY: Sources = { books: [], shelf: [], index: null };

function GlyphFor({ target }: { target: PaletteTarget }) {
  const cls = "h-4 w-4";
  switch (target.kind) {
    case "book":
      return <BooksIcon className={cls} />;
    case "cast":
      return <PlayIcon className={cls} />;
    case "chapter":
      return <HeadphonesIcon className={cls} />;
    case "view":
      return target.view === "audio" ? (
        <HeadphonesIcon className={cls} />
      ) : target.view === "library" ? (
        <BooksIcon className={cls} />
      ) : (
        <SettingsIcon className={cls} />
      );
    case "action":
      return target.action === "tutor" ? (
        <ChatIcon className={cls} />
      ) : target.action === "activity" ? (
        <WaveIcon className={cls} />
      ) : (
        <HelpCircleIcon className={cls} />
      );
  }
}

function CommandPalette({
  onClose,
  onChoose,
}: {
  onClose: () => void;
  onChoose: (target: PaletteTarget) => void;
}) {
  const { t } = useI18n();
  const listId = useId();
  const inputRef = useRef<HTMLInputElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const [query, setQuery] = useState("");
  const [typed, setTyped] = useState(false);
  const [selected, setSelected] = useState(0);
  const [sources, setSources] = useState<Sources>(EMPTY);
  const [glide, setGlide] = useState<{ top: number; height: number } | null>(
    null,
  );
  const playingJobId = usePlayback((s) => s.item?.jobId ?? null);

  useDialogFocus(dialogRef, true, {
    onEscape: onClose,
    initialFocusRef: inputRef,
  });

  useEffect(() => {
    let cancelled = false;
    Promise.all([
      invoke<BookSummary[]>("list_books"),
      invoke<ExportSummary[]>("list_exports"),
      playingJobId == null
        ? Promise.resolve(null)
        : invoke<ReadingIndex>("get_reading_index", {
            jobId: playingJobId,
          }).catch(() => null),
    ])
      .then(([books, exports, index]) => {
        if (cancelled) return;
        setSources({ books, shelf: buildAudioShelf(exports, books), index });
      })
      .catch(console.error);
    return () => {
      cancelled = true;
    };
  }, [playingJobId]);

  const entries: PaletteEntry[] = [
    ...sources.books.flatMap((book): PaletteEntry[] => {
      const hasPages = book.pageCount != null && book.pageCount >= 1;
      const playable = bookAvailability(book.id, sources.shelf).playable;
      if (!hasPages && playable == null) return [];
      return [
        {
          group: "books",
          id: `book:${book.id}`,
          label: bookDisplayTitle(book),
          detail: bookDisplayAuthor(book) ?? undefined,
          target: { kind: "book", bookId: book.id, hasPages, playable },
        },
      ];
    }),
    ...sources.shelf
      .filter((item) => !item.stale)
      .map((item): PaletteEntry => {
        const scope = castScopeOf(item.path);
        return {
          group: "casts",
          id: `cast:${item.id}`,
          label: scope == null ? item.title : castScopeLabel(scope),
          detail: scope == null ? undefined : item.title,
          target: { kind: "cast", item },
        };
      }),
    ...(sources.index?.chapters ?? []).map((chapter): PaletteEntry => ({
      group: "chapters",
      id: `chapter:${chapter.chapterId}`,
      label: chapter.title,
      detail: (chapter.title.match(/[0-9]+/g) ?? ([] as string[])).includes(
        String(chapter.idx + 1),
      )
        ? undefined
        : t("cast.scope.chapter", { list: chapter.idx + 1 }),
      target: { kind: "chapter", startMs: chapter.startMs },
    })),
    {
      group: "go",
      id: "go:library",
      label: t("nav.shelf"),
      target: { kind: "view", view: "library" },
    },
    {
      group: "go",
      id: "go:audio",
      label: t("nav.listening"),
      target: { kind: "view", view: "audio" },
    },
    {
      group: "go",
      id: "go:activity",
      label: t("nav.activity"),
      target: { kind: "action", action: "activity" },
    },
    {
      group: "go",
      id: "go:settings",
      label: t("nav.settings"),
      target: { kind: "view", view: "settings" },
    },
    {
      group: "go",
      id: "go:models",
      label: t("nav.voicesAndModels"),
      target: { kind: "view", view: "models" },
    },
    {
      group: "go",
      id: "go:providers",
      label: t("nav.providers"),
      target: { kind: "view", view: "providers" },
    },
    {
      group: "actions",
      id: "action:tutor",
      label: t("nav.talkToTutor"),
      target: { kind: "action", action: "tutor" },
    },
    {
      group: "actions",
      id: "action:report",
      label: t("palette.reportIssue"),
      target: { kind: "action", action: "report" },
    },
  ];

  const ranked = rankPalette(entries, query);
  const flat = ranked.flatMap((group) => group.entries);
  const active = flat[Math.min(selected, Math.max(0, flat.length - 1))];
  const activeIndex = active == null ? -1 : flat.indexOf(active);
  const activeId = active?.id ?? null;
  const optionId = (entry: PaletteEntry) =>
    `${listId}-${entry.id.replace(/[^a-zA-Z0-9_-]/g, "_")}`;

  useLayoutEffect(() => {
    const list = listRef.current;
    const row =
      list == null || activeId == null
        ? null
        : list.querySelector<HTMLElement>(
            `[data-entry-id="${CSS.escape(activeId)}"]`,
          );
    if (row == null) {
      setGlide(null);
      return;
    }
    const top = row.offsetTop;
    const height = row.offsetHeight;
    setGlide((current) =>
      current != null && current.top === top && current.height === height
        ? current
        : { top, height },
    );
    row.scrollIntoView({ block: "nearest" });
  }, [activeId, query, sources]);

  const hintFor = (group: PaletteGroup): string =>
    group === "books"
      ? t("palette.hint.open")
      : group === "casts"
        ? t("palette.hint.play")
        : group === "chapters"
          ? t("palette.hint.playFrom")
          : group === "go"
            ? t("palette.hint.go")
            : t("palette.hint.run");

  const choose = (entry: PaletteEntry) => {
    onClose();
    onChoose(entry.target);
  };

  let rowIndex = -1;

  return (
    <div
      data-testid="command-palette"
      data-modal-layer=""
      className="palette-scrim fixed inset-0 z-[70] flex justify-center bg-black/45 px-4 pt-[12vh]"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        ref={dialogRef}
        tabIndex={-1}
        role="dialog"
        aria-modal="true"
        aria-label={t("palette.aria")}
        className="palette-panel shadow-overlay flex max-h-[70vh] w-full max-w-[600px] flex-col self-start overflow-hidden rounded-xl border border-ink-800 bg-ink-900"
        onKeyDown={(event) => {
          if (event.key === "ArrowDown" || event.key === "ArrowUp") {
            event.preventDefault();
            setSelected(
              moveSelection(
                Math.max(0, activeIndex),
                event.key === "ArrowDown" ? 1 : -1,
                flat.length,
              ),
            );
          } else if (event.key === "Enter") {
            event.preventDefault();
            if (active != null) choose(active);
          } else if (event.key === "Tab") {
            event.preventDefault();
            inputRef.current?.focus();
          }
        }}
      >
        <div className="flex items-center gap-3 border-b border-ink-800 px-4">
          <SearchIcon className="h-5 w-5 shrink-0 text-ink-400" />
          <input
            ref={inputRef}
            data-testid="command-palette-input"
            role="combobox"
            aria-expanded="true"
            aria-controls={listId}
            aria-activedescendant={
              active == null ? undefined : optionId(active)
            }
            aria-autocomplete="list"
            dir="auto"
            value={query}
            placeholder={t("palette.placeholder")}
            onChange={(event) => {
              setQuery(event.currentTarget.value);
              setTyped(true);
              setSelected(0);
            }}
            className="palette-input h-14 min-w-0 flex-1 bg-transparent text-[15px] text-ink-50 outline-none placeholder:text-ink-500"
          />
          <kbd className="rounded border border-ink-700 px-1.5 font-mono text-[11px] text-ink-400">
            Esc
          </kbd>
        </div>
        <div
          ref={listRef}
          id={listId}
          role="listbox"
          aria-label={t("palette.aria")}
          className="relative min-h-0 flex-1 overflow-y-auto p-2"
        >
          {glide != null && (
            <span
              aria-hidden="true"
              className="palette-glide pointer-events-none absolute inset-x-2 top-0 rounded-lg bg-ember-500/12"
              style={{
                transform: `translateY(${glide.top}px)`,
                height: glide.height,
              }}
            >
              <span className="absolute inset-y-2 start-0 w-[3px] rounded-e-full bg-ember-500" />
            </span>
          )}
          {ranked.length === 0 && (
            <div
              data-testid="command-palette-empty"
              className="flex flex-col items-center gap-1.5 px-6 py-10 text-center"
            >
              <p className="text-secondary font-medium text-ink-200">
                {t("palette.empty", { query })}
              </p>
              <p className="text-label text-ink-400">
                {t("palette.emptyHint")}
              </p>
            </div>
          )}
          {ranked.map((group) => (
            <div
              key={group.group}
              role="group"
              aria-labelledby={`${listId}-${group.group}`}
              className="pb-1"
            >
              <p
                id={`${listId}-${group.group}`}
                className="px-3 pt-2.5 pb-1 text-[11px] font-semibold tracking-[0.12em] text-ink-500 uppercase"
              >
                {t(`palette.group.${group.group}`)}
              </p>
              {group.entries.map((entry) => {
                rowIndex += 1;
                const index = rowIndex;
                const isActive = entry === active;
                return (
                  <div
                    key={entry.id}
                    id={optionId(entry)}
                    role="option"
                    aria-selected={isActive}
                    data-entry-id={entry.id}
                    data-testid={`palette-${entry.id}`}
                    onMouseMove={() => {
                      if (!isActive) setSelected(index);
                    }}
                    onClick={() => choose(entry)}
                    className={`relative flex h-10 cursor-pointer items-center gap-3 rounded-lg px-3 ${
                      typed ? "" : "palette-row-in"
                    } ${isActive ? "text-ink-50" : "text-ink-300"}`}
                    style={
                      typed
                        ? undefined
                        : ({ "--i": index } as React.CSSProperties)
                    }
                  >
                    <span
                      className={`shrink-0 transition-colors duration-150 ${
                        isActive ? "text-ember-400" : "text-ink-500"
                      }`}
                    >
                      <GlyphFor target={entry.target} />
                    </span>
                    <span className="min-w-0 truncate text-secondary font-medium">
                      {entry.label}
                    </span>
                    {entry.detail != null && (
                      <span className="min-w-0 truncate text-label text-ink-500">
                        {entry.detail}
                      </span>
                    )}
                    <span
                      className={`ms-auto flex shrink-0 items-center gap-1.5 text-label text-ink-400 transition-opacity duration-150 ${
                        isActive ? "opacity-100" : "opacity-0"
                      }`}
                    >
                      {hintFor(entry.group)}
                      <kbd className="rounded border border-ink-700 px-1 font-mono text-[11px]">
                        ↵
                      </kbd>
                    </span>
                  </div>
                );
              })}
            </div>
          ))}
        </div>
        <div className="flex items-center gap-4 border-t border-ink-800 px-4 py-2 text-label text-ink-500">
          <span className="flex items-center gap-1.5">
            <kbd className="rounded border border-ink-700 px-1 font-mono text-[11px]">
              ↑↓
            </kbd>
            {t("palette.move")}
          </span>
          <span className="flex items-center gap-1.5">
            <kbd className="rounded border border-ink-700 px-1 font-mono text-[11px]">
              ↵
            </kbd>
            {t("palette.choose")}
          </span>
          <span className="flex items-center gap-1.5">
            <kbd className="rounded border border-ink-700 px-1 font-mono text-[11px]">
              Esc
            </kbd>
            {t("palette.close")}
          </span>
        </div>
      </div>
    </div>
  );
}

export default CommandPalette;
