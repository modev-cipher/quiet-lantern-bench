import { canFollowAlong } from "./readingOverlayModel";
import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ensureEngine } from "./engineSetupStore";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

import {
  bookDisplayAuthor,
  bookMeta,
  bookDisplayTitle,
  canImproveExtraction,
  deleteBookErrorMessage,
  extractionErrorMessage,
  extractionOutcomeNotice,
  extractionProgressLabel,
  importProgressLabel,
  layoutPrompt,
  presentImportError,
  presentImportWarnings,
  type ActiveExtraction,
  type ActiveImport,
  type ExtractionOutcome,
  type ExtractionPlan,
  type ExtractionProgress,
  type ImportPlan,
  type BookSummary,
  type ImportProgress,
  type ImportResult,
} from "./libraryModel";
import {
  buildAudioShelf,
  durationParts,
  formatAudioSize,
  formatAudioDate,
  canPlay,
  modeLabel,
  type AudioShelfItem,
  type ExportSummary,
} from "./audioShelfModel";
import type { JobStateUpdate } from "./jobModel";
import { dispatch } from "./playbackStore";
import { cardListened, cardPlayback, usePlayback } from "./usePlayback";
import { listenedOf, loadListened } from "./listeningProgress";
import {
  bookAvailability,
  partitionOrphans,
  type BookAvailability,
} from "./libraryAvailability";
import {
  AlertIcon,
  AudioIcon,
  CastIcon,
  CheckIcon,
  CircleIcon,
  CloseIcon,
  CopyIcon,
  MicIcon,
  PauseIcon,
  PlayIcon,
  SparkIcon,
  TrashIcon,
  UploadIcon,
} from "./Icons";
import {
  CAST_BOOK_EVENT,
  consumePendingCastBook,
  consumePendingImport,
  consumePendingSendToPhone,
  goToView,
  notifySetupChanged,
  requestBookOpen,
  requestBookTutor,
} from "./views";
import BookMenu, { type BookMenuItem } from "./BookMenu";
import DeleteAction from "./DeleteAction";
import { useI18n, type MessageKey } from "./i18n";
import { forgetMediaUrl } from "./mediaUrl";
import TypographicCover from "./TypographicCover";
import ShelfBook from "./ShelfBook";
import { introRemainingMs } from "./LaunchIntro";
import { partitionShelf, shelfStatus, type ShelfStatus } from "./shelfModel";
import { formatDuration } from "./sessionHistoryStore";
import { castScopeLabel, castScopeOf } from "./castScope";
import JobModal from "./JobModal";
import { clearCastDraft, pruneCastDrafts } from "./castDraftStore";
import SendToPhoneModal from "./SendToPhoneModal";
import ShelfContinue from "./ShelfContinue";
import type { SetupChecklist } from "./setupChecklist";
import { GlideGroup } from "./GlideThumb";
import { commandErrorText } from "./commandError";

type Segment = "books" | "audios";

export type ShelfFilter = "all" | "casting" | "audio" | "unattached";

export function filterSegment(filter: ShelfFilter): Segment {
  return filter === "audio" || filter === "unattached" ? "audios" : "books";
}

function copyToClipboard(text: string): void {
  if (
    typeof navigator !== "undefined" &&
    navigator.clipboard?.writeText != null
  ) {
    navigator.clipboard.writeText(text).catch(() => {
      copyViaHiddenTextarea(text);
    });
    return;
  }
  copyViaHiddenTextarea(text);
}

function copyViaHiddenTextarea(text: string): boolean {
  if (typeof document === "undefined") return false;
  const el = document.createElement("textarea");
  el.value = text;
  el.setAttribute("readonly", "");
  el.style.position = "fixed";
  el.style.opacity = "0";
  el.style.pointerEvents = "none";
  document.body.appendChild(el);
  el.select();
  try {
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    document.body.removeChild(el);
  }
}

const SHELF_PRIMARY_CLASS =
  "ui-btn inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap";
const SHELF_ICON_CLASS =
  "flex h-[26px] w-[26px] shrink-0 items-center justify-center rounded-lg border border-ink-800 bg-ink-900 text-ink-300 transition-colors duration-300 hover:border-ink-700 hover:text-ink-100 active:scale-[0.97]";

function ShelfStatusLine({
  book,
  status,
}: {
  book: BookSummary;
  status: ShelfStatus;
}) {
  const { t } = useI18n();
  switch (status.kind) {
    case "improving":
      return (
        <span
          role="status"
          data-testid={`improving-${book.id}`}
          className="flex items-center gap-1.5 text-label text-ember-400"
        >
          <SparkIcon className="h-3.5 w-3.5" />
          {t("library.rereading")}
        </span>
      );
    case "casting":
      return (
        <span className="flex items-center gap-1.5 text-label text-ink-300">
          <span className="shelf-status-ring" aria-hidden="true" />
          {t("shelf.status.casting")}
        </span>
      );
    case "listen":
      return (
        <span className="flex items-center gap-1.5 text-label text-ink-300">
          <PlayIcon className="h-3 w-3 text-ember-400 rtl:-scale-x-100" />
          {status.leftMs != null
            ? t("shelf.status.left", { time: formatDuration(status.leftMs) })
            : t("shelf.status.audio")}
        </span>
      );
    case "finished":
      return (
        <span className="flex items-center gap-1.5 text-label text-ink-300">
          <CheckIcon className="h-3.5 w-3.5 text-ember-400" />
          {t("shelf.status.finished")}
        </span>
      );
    case "audio":
      return (
        <span className="flex items-center gap-1.5 text-label text-ink-300">
          <PlayIcon className="h-3 w-3 text-ember-400 rtl:-scale-x-100" />
          {t("shelf.status.audio")}
        </span>
      );
    case "stale":
      return (
        <span className="flex items-center gap-1.5 text-label text-reading-stale">
          <AlertIcon className="h-3.5 w-3.5" />
          {t("shelf.status.stale")}
        </span>
      );
    case "ready":
      return (
        <span className="flex items-center gap-1.5 text-label text-ink-400">
          <CircleIcon className="h-3 w-3" />
          {t("shelf.status.ready")}
        </span>
      );
  }
}

function ShelfSlot({
  book,
  index,
  availability,
  status,
  onDelete,
  onCreateAudio,
  onImproveExtraction,
  onStudy,
  improving,
}: {
  book: BookSummary;
  index: number;
  availability: BookAvailability;
  status: ShelfStatus;
  onDelete: (id: string) => void;
  onCreateAudio: (book: BookSummary) => void;
  onImproveExtraction: (book: BookSummary) => void;
  onStudy: (item: AudioShelfItem) => void;
  improving: boolean;
}) {
  const { t } = useI18n();
  const [menuOpen, setMenuOpen] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const confirmRef = useRef<HTMLButtonElement | null>(null);
  const actionsRef = useRef<HTMLDivElement | null>(null);
  const cancelRemoval = () => {
    setConfirming(false);
    requestAnimationFrame(() =>
      actionsRef.current
        ?.querySelector<HTMLButtonElement>('[aria-haspopup="menu"]')
        ?.focus(),
    );
  };
  const menuItems: BookMenuItem[] = [
    ...(canImproveExtraction(book) && !improving
      ? [
          {
            id: `improve-${book.id}`,
            label: t("library.improveExtraction"),
            icon: <SparkIcon className="h-4 w-4 text-ember-400" />,
            onSelect: () => onImproveExtraction(book),
          },
        ]
      : []),
    {
      id: `copy-id-${book.id}`,
      label: t("library.copyBookId"),
      icon: <CopyIcon className="h-4 w-4 text-ink-400" />,
      onSelect: () => copyToClipboard(book.id),
    },
    {
      id: `delete-${book.id}`,
      testId: `delete-book-${book.id}`,
      label: t("library.deleteBookTitle"),
      icon: <TrashIcon className="h-4 w-4" />,
      tone: "danger",
      onSelect: () => {
        setConfirming(true);
        requestAnimationFrame(() => confirmRef.current?.focus());
      },
    },
  ];

  const title = bookDisplayTitle(book);
  const author = bookDisplayAuthor(book);
  const hasPages = book.pageCount != null && book.pageCount >= 1;
  const followable =
    availability.playable != null && canFollowAlong(availability.playable)
      ? availability.playable
      : null;
  const canOpen = availability.playable != null || hasPages;
  const active = menuOpen || confirming;

  return (
    <article
      data-testid="book-card"
      data-active={active ? "true" : undefined}
      data-status={status.kind}
      style={{ "--stagger": index } as React.CSSProperties}
      className="shelf-slot"
    >
      <span aria-hidden="true" className="shelf-contact" />
      {canOpen ? (
        <button
          type="button"
          data-testid={`open-book-${book.id}`}
          data-shelf-book=""
          aria-label={t("library.openBook", { title })}
          onClick={() => {
            if (followable != null) onStudy(followable);
            else if (hasPages) requestBookOpen(book.id);
            else if (availability.playable != null)
              onStudy(availability.playable);
          }}
          className="shelf-open"
        >
          <ShelfBook book={book} status={status} />
        </button>
      ) : (
        <div className="shelf-open">
          <ShelfBook book={book} status={status} />
        </div>
      )}
      <div className="shelf-label">
        <h2
          data-testid="book-card-title"
          title={title}
          className="line-clamp-2 text-secondary leading-snug font-semibold text-ink-100"
        >
          {title}
        </h2>
        {author && (
          <p className="mt-0.5 truncate text-label text-ink-400">{author}</p>
        )}
        <div className="shelf-line" data-line="status">
          <ShelfStatusLine book={book} status={status} />
        </div>
        <div
          ref={actionsRef}
          className="shelf-line flex items-center gap-1.5"
          data-line="actions"
        >
          {confirming ? (
            <div
              className="flex items-center gap-1.5"
              onKeyDown={(event) => {
                if (event.key === "Escape") {
                  event.stopPropagation();
                  cancelRemoval();
                }
              }}
            >
              <button
                ref={confirmRef}
                type="button"
                data-testid={`confirm-delete-book-${book.id}`}
                onClick={() => onDelete(book.id)}
                className="flex h-7 items-center rounded-md bg-wash-red-ink px-2.5 text-label font-medium whitespace-nowrap text-ink-950 transition-colors hover:bg-wash-red-ink/90"
              >
                {t("library.confirmDelete")}
              </button>
              <button
                type="button"
                data-testid={`cancel-delete-book-${book.id}`}
                onClick={cancelRemoval}
                className="flex h-7 items-center rounded-md px-2 text-label whitespace-nowrap text-ink-400 transition-colors hover:text-ink-200"
              >
                {t("library.keep")}
              </button>
            </div>
          ) : (
            <>
              <div data-testid="book-card-primary">
                {availability.playable != null ? (
                  <button
                    type="button"
                    data-testid={`study-book-${book.id}`}
                    data-variant="solid"
                    data-size="sm"
                    onClick={() =>
                      onStudy(availability.playable as AudioShelfItem)
                    }
                    className={SHELF_PRIMARY_CLASS}
                  >
                    <PlayIcon className="h-3.5 w-3.5 rtl:-scale-x-100" />
                    {t("nav.study")}
                  </button>
                ) : (
                  <button
                    type="button"
                    data-testid={`create-audio-${book.id}`}
                    data-variant="solid"
                    data-size="sm"
                    onClick={() => onCreateAudio(book)}
                    className={SHELF_PRIMARY_CLASS}
                  >
                    <CastIcon className="h-3.5 w-3.5" />
                    {t("job.cast")}
                  </button>
                )}
              </div>
              <div
                data-testid="book-card-secondary"
                className="flex items-center gap-1"
              >
                <button
                  type="button"
                  data-testid={`talk-book-${book.id}`}
                  onClick={() => requestBookTutor(book.id)}
                  aria-label={t("library.talk")}
                  title={t("library.talkHint")}
                  className={SHELF_ICON_CLASS}
                >
                  <MicIcon className="h-4 w-4" />
                </button>
                {availability.playable != null && (
                  <button
                    type="button"
                    data-testid={`create-audio-${book.id}`}
                    onClick={() => onCreateAudio(book)}
                    aria-label={t("shelf.castAgain")}
                    title={t("shelf.castAgain")}
                    className={SHELF_ICON_CLASS}
                  >
                    <CastIcon className="h-4 w-4" />
                  </button>
                )}
                <BookMenu
                  testId={book.id}
                  items={menuItems}
                  details={bookMeta(book)}
                  onOpenChange={setMenuOpen}
                  triggerClassName={SHELF_ICON_CLASS}
                />
              </div>
            </>
          )}
        </div>
      </div>
    </article>
  );
}

function ShelfSection({
  label,
  books,
  startIndex,
  testId,
  renderSlot,
}: {
  label: string;
  books: BookSummary[];
  startIndex: number;
  testId: string;
  renderSlot: (book: BookSummary, index: number) => React.ReactNode;
}) {
  const { t } = useI18n();
  if (books.length === 0) return null;
  return (
    <section
      data-testid={testId}
      aria-label={label}
      className="flex flex-col gap-3"
    >
      <h2 className="flex items-baseline gap-2.5 px-1">
        <span className="font-serif text-body font-semibold text-ink-200">
          {label}
        </span>
        <span className="text-label text-ink-500">
          {t("shelf.count", { count: books.length })}
        </span>
      </h2>
      <div className="shelf-grid" onKeyDown={moveBetweenBooks}>
        {books.map((book, offset) => renderSlot(book, startIndex + offset))}
      </div>
    </section>
  );
}

function moveBetweenBooks(event: React.KeyboardEvent<HTMLDivElement>) {
  const target = event.target as HTMLElement;
  if (!target.matches("[data-shelf-book]")) return;
  const books = Array.from(
    event.currentTarget.querySelectorAll<HTMLElement>("[data-shelf-book]"),
  );
  const at = books.indexOf(target);
  if (at < 0) return;
  const rtl = getComputedStyle(target).direction === "rtl";
  const box = target.getBoundingClientRect();
  let next: HTMLElement | undefined;
  if (event.key === "ArrowRight" || event.key === "ArrowLeft") {
    const forward = (event.key === "ArrowRight") !== rtl;
    next = books[at + (forward ? 1 : -1)];
  } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    const down = event.key === "ArrowDown";
    const centre = box.left + box.width / 2;
    let best = Number.POSITIVE_INFINITY;
    for (const candidate of books) {
      const rect = candidate.getBoundingClientRect();
      const rowStep = down ? rect.top - box.bottom : box.top - rect.bottom;
      if (rowStep < 0) continue;
      const score = rowStep * 4 + Math.abs(rect.left + rect.width / 2 - centre);
      if (score < best) {
        best = score;
        next = candidate;
      }
    }
  } else if (event.key === "Home") {
    next = books[0];
  } else if (event.key === "End") {
    next = books[books.length - 1];
  } else {
    return;
  }
  if (next == null) return;
  event.preventDefault();
  next.focus();
}

const CHIPS: Record<
  "books" | "audios",
  { id: ShelfFilter; testId: string; label: MessageKey }[]
> = {
  books: [
    { id: "all", testId: "filter-all-books", label: "library.filter.all" },
    { id: "casting", testId: "filter-casting", label: "nav.casting" },
  ],
  audios: [
    { id: "audio", testId: "filter-all-audio", label: "library.filter.all" },
    {
      id: "unattached",
      testId: "filter-unattached",
      label: "library.unattached",
    },
  ],
};

function LibraryTabs({
  segment,
  bookCount,
  audioCount,
  onFilter,
}: {
  segment: "books" | "audios";
  bookCount: number | null;
  audioCount: number;
  onFilter: (next: ShelfFilter) => void;
}) {
  const { t } = useI18n();
  const tab = (
    id: "books" | "audios",
    testId: string,
    label: MessageKey,
    count: number | null,
    target: ShelfFilter,
  ) => (
    <button
      type="button"
      role="tab"
      data-testid={testId}
      aria-selected={segment === id}
      onClick={() => onFilter(target)}
      className={`relative flex items-baseline gap-1.5 px-0.5 pb-2 text-body transition-colors duration-200 ${
        segment === id
          ? "font-semibold text-ink-50"
          : "text-ink-400 hover:text-ink-100"
      }`}
    >
      {t(label)}
      {count != null && (
        <span className="font-mono text-label text-ink-500 tabular-nums">
          {count}
        </span>
      )}
    </button>
  );
  return (
    <GlideGroup
      thumbClassName="border-b-2 border-ember-500"
      role="tablist"
      aria-label={t("library.title")}
      className="flex items-end gap-6"
    >
      {tab("books", "zone-books", "library.tab.books", bookCount, "all")}
      {tab("audios", "nav-audio", "library.tab.audio", audioCount, "audio")}
    </GlideGroup>
  );
}

function ShelfChips({
  segment,
  filter,
  onFilter,
}: {
  segment: "books" | "audios";
  filter: ShelfFilter;
  onFilter: (next: ShelfFilter) => void;
}) {
  const { t } = useI18n();
  return (
    <div
      role="group"
      aria-label={t("library.filters")}
      className="flex flex-wrap items-center gap-1.5"
    >
      {CHIPS[segment].map((option) => (
        <button
          key={option.id}
          type="button"
          data-testid={option.testId}
          aria-pressed={filter === option.id}
          onClick={() => onFilter(option.id)}
          className={`rounded-md border px-2.5 py-1 text-label transition-colors duration-200 active:scale-[0.98] ${
            filter === option.id
              ? "border-ink-600 bg-ink-850 font-semibold text-ink-50"
              : "border-ink-800 text-ink-400 hover:border-ink-700 hover:text-ink-100"
          }`}
        >
          {t(option.label)}
        </button>
      ))}
    </div>
  );
}

function ShelfSkeleton() {
  return (
    <div
      aria-hidden="true"
      data-testid="library-skeleton"
      className="grid gap-6 grid-cols-[repeat(auto-fill,minmax(min(180px,100%),1fr))]"
    >
      {[0, 1, 2, 3, 4].map((card) => (
        <div
          key={card}
          className="flex flex-col overflow-hidden rounded-2xl border border-ink-800 bg-ink-900"
        >
          <div className="shimmer-bar aspect-[4/5] w-full bg-ink-800/60" />
          <div className="flex flex-col gap-2 p-4">
            <div className="shimmer-bar h-4 w-3/4 rounded bg-ink-800/70" />
            <div className="shimmer-bar h-3 w-1/2 rounded bg-ink-800/50" />
          </div>
        </div>
      ))}
    </div>
  );
}

function EqualizerGlyph() {
  return (
    <span
      aria-hidden
      className="flex h-3.5 items-end gap-0.5"
      data-testid="now-playing-glyph"
    >
      {[0, 1, 2].map((bar) => (
        <span
          key={bar}
          className="eq-bar w-0.5 rounded-full bg-ember-400"
          style={{ height: "100%", animationDelay: `${bar * 0.18}s` }}
        />
      ))}
    </span>
  );
}

function AudioCard({
  item,
  index,
  listened,
  onOpen,
  onDelete,
  onRecast,
}: {
  item: AudioShelfItem;
  index: number;
  listened: number | null;
  onOpen: (item: AudioShelfItem) => void;
  onDelete: (exportId: string) => void;
  onRecast: (bookId: string) => void;
}) {
  const { t, locale } = useI18n();
  const playback = usePlayback((s) => cardPlayback(s, item.id, item.path));
  const live = usePlayback((s) => cardListened(s, item.id, item.path));
  const playable = canPlay(item);
  const date = formatAudioDate(item.createdAt, locale);
  const size = formatAudioSize(item.bytes);
  const parts = durationParts(item.durationMs);
  const length =
    parts == null
      ? ""
      : parts.h === 0
        ? t("library.durationM", { m: parts.m })
        : parts.m === 0
          ? t("library.durationH", { h: parts.h })
          : t("library.durationHm", { h: parts.h, m: parts.m });
  const isActive = playback !== "idle";
  const isPlaying = playback === "playing";

  const quickToggle = () => {
    if (!playable) return;
    if (isActive) dispatch({ type: "toggle" });
    else dispatch({ type: "load", item });
  };

  const scope = castScopeOf(item.path);
  const progress = live ?? listened;
  const pct = progress == null ? 0 : Math.round(progress * 100);

  return (
    <article
      data-testid={`audio-card-${item.id}`}
      data-playing={isPlaying}
      style={{ "--stagger": index } as React.CSSProperties}
      onClick={() => playable && onOpen(item)}
      className={`lift-card group animate-rise-in flex cursor-pointer flex-col rounded-xl border bg-ink-900 ${
        isActive
          ? "border-ember-500/50 ring-1 ring-ember-500/25"
          : "border-ink-800 hover:border-ink-700"
      }`}
    >
      <div className="relative h-28 w-full">
        <div className="cover-zoom absolute inset-0 overflow-hidden rounded-t-xl">
          <TypographicCover
            seed={item.bookId || item.title}
            title={item.title}
            badge={
              isPlaying ? (
                <span className="flex h-6 min-w-6 items-center justify-center rounded-full bg-ink-950/50 px-1.5 text-ember-300 ring-1 ring-white/10">
                  <EqualizerGlyph />
                </span>
              ) : null
            }
          />
        </div>
        {playable && (
          <button
            type="button"
            data-testid={`cover-quick-toggle-${item.id}`}
            aria-label={isPlaying ? t("player.pause") : t("player.play")}
            onClick={(event) => {
              event.stopPropagation();
              quickToggle();
            }}
            className="press-pop absolute end-3 -bottom-5 flex h-10 w-10 items-center justify-center rounded-full bg-ember-500 text-on-accent shadow-overlay hover:bg-ember-600"
          >
            {isPlaying ? (
              <PauseIcon className="h-4 w-4" />
            ) : (
              <PlayIcon className="ml-0.5 h-4 w-4" />
            )}
          </button>
        )}
      </div>
      <div className="flex flex-1 flex-col gap-2 p-4 pt-5">
        <div className="min-w-0 pe-10">
          <h2
            className="truncate font-serif text-heading leading-snug font-semibold text-ink-50"
            title={scope ? castScopeLabel(scope) : item.title}
          >
            {scope ? castScopeLabel(scope) : item.title}
          </h2>
          {scope && (
            <p
              className="truncate text-secondary text-ink-500"
              title={item.title}
            >
              {item.title}
            </p>
          )}
        </div>
        {item.stale && (
          <p
            data-testid={`audio-stale-${item.id}`}
            className="rounded-md bg-wash-yellow px-2 py-1 text-label leading-snug text-wash-yellow-ink"
          >
            {t("library.rerenderNeeded")}
          </p>
        )}
        <div className="flex flex-wrap items-center gap-x-1.5 gap-y-1">
          <span className="rounded bg-ink-800 px-2 py-0.5 font-mono text-label tracking-wide text-ink-400 uppercase">
            {modeLabel(item.mode)}
          </span>
          <span className="rounded bg-ink-800 px-2 py-0.5 font-mono text-label tracking-wide text-ink-400 uppercase">
            {item.format}
          </span>
          {length && (
            <span
              data-testid={`audio-length-${item.id}`}
              className="font-mono text-label whitespace-nowrap text-ink-300"
            >
              {length}
            </span>
          )}
          {(size || date) && (
            <span className="font-mono text-label whitespace-nowrap text-ink-500">
              {[size, date].filter(Boolean).join(" · ")}
            </span>
          )}
        </div>
        {!item.stale && item.seekExact === false && (
          <div
            data-testid={`audio-seek-drift-${item.id}`}
            title={t("library.seekDriftHelp")}
            className="flex flex-wrap items-center justify-between gap-x-2 gap-y-0.5 rounded-md border border-ink-800 px-2 py-1 text-label leading-snug text-ink-400"
            onClick={(event) => event.stopPropagation()}
          >
            <span className="min-w-0">{t("library.seekDrift")}</span>
            {item.bookId !== "" && (
              <button
                type="button"
                data-testid={`audio-rerender-${item.id}`}
                onClick={() => onRecast(item.bookId)}
                className="shrink-0 rounded px-1.5 py-0.5 font-medium text-ember-400 transition-colors duration-200 hover:bg-ink-800 hover:text-ember-300"
              >
                {t("library.rerender")}
              </button>
            )}
          </div>
        )}
        <div className="mt-auto flex flex-col gap-1.5">
          <div
            className="h-1 overflow-hidden rounded-full bg-ink-800"
            role="progressbar"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={pct}
            aria-label={t("library.castProgressLabel")}
          >
            <span
              className="fill-glide block h-full rounded-full bg-ember-500"
              style={{ width: `${pct}%` }}
            />
          </div>
          <div
            className="flex items-center justify-between gap-2 text-label text-ink-500"
            onClick={(event) => event.stopPropagation()}
          >
            <span className="truncate">
              {pct > 0
                ? t("library.castProgress", { pct })
                : t("library.castNotStarted")}
            </span>
            <DeleteAction
              testId={`audio-${item.id}`}
              label={null}
              confirmLabel={t("library.deleteAudioConfirm")}
              cancelLabel={t("library.keep")}
              title={t("library.deleteAudioTitle")}
              onConfirm={() => onDelete(item.id)}
            />
          </div>
        </div>
      </div>
    </article>
  );
}

function Library({
  filter,
  castingBookIds,
  onFilter,
  checklist,
  onOpenAudio,
  onOpenSession,
  onPickModels,
  onOpenProviders,
}: {
  filter: ShelfFilter;
  castingBookIds: ReadonlySet<string>;
  onFilter: (next: ShelfFilter) => void;
  checklist: SetupChecklist | null;
  onOpenAudio: (item: AudioShelfItem) => void;
  onOpenSession: (
    item: AudioShelfItem,
    resumed: boolean,
    fraction: number | null,
  ) => void;
  onPickModels: () => void;
  onOpenProviders?: () => void;
}) {
  const { t } = useI18n();
  const segment = filterSegment(filter);
  const [books, setBooks] = useState<BookSummary[] | null>(null);
  const [exports, setExports] = useState<ExportSummary[]>([]);
  const [importing, setImporting] = useState(false);
  const [importFilename, setImportFilename] = useState<string | null>(null);
  const [cancellingImport, setCancellingImport] = useState(false);
  const [progress, setProgress] = useState<ImportProgress | null>(null);
  const [pending, setPending] = useState<{
    path: string;
    plan: ImportPlan;
    prompt: NonNullable<ReturnType<typeof layoutPrompt>>;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notices, setNotices] = useState<string[]>([]);
  const [jobBook, setJobBook] = useState<BookSummary | null>(null);
  const [improveBook, setImproveBook] = useState<{
    book: BookSummary;
    prompt: NonNullable<ReturnType<typeof layoutPrompt>>;
  } | null>(null);
  const [improvingId, setImprovingId] = useState<string | null>(null);
  const improvingIdRef = useRef<string | null>(null);
  const [improveProgress, setImproveProgress] =
    useState<ExtractionProgress | null>(null);

  const refresh = useCallback(() => {
    invoke<BookSummary[]>("list_books")
      .then(setBooks)
      .catch((err) => setError(commandErrorText(err)));
  }, []);

  const refreshExports = useCallback(() => {
    invoke<ExportSummary[]>("list_exports")
      .then((rows) => {
        for (const row of rows) {
          if (row.stale) forgetMediaUrl(row.path);
        }
        setExports(rows);
      })
      .catch((err) => setError(commandErrorText(err)));
  }, []);

  useEffect(() => {
    refresh();
    refreshExports();
  }, [refresh, refreshExports]);

  useEffect(() => {
    if (segment === "audios" && consumePendingSendToPhone()) {
      setSendToPhoneOpen(true);
    }
  }, [segment]);

  const recastBook = (bookId: string) => {
    clearCastDraft(bookId);
    const book = books?.find((entry) => entry.id === bookId) ?? null;
    if (book != null) setJobBook(book);
  };

  useEffect(() => {
    if (books == null) return;
    pruneCastDrafts(new Set(books.map((book) => book.id)));
    const openPendingCast = () => {
      const bookId = consumePendingCastBook();
      if (bookId == null) return;
      const book = books.find((entry) => entry.id === bookId) ?? null;
      if (book != null) setJobBook(book);
    };
    openPendingCast();
    window.addEventListener(CAST_BOOK_EVENT, openPendingCast);
    return () => window.removeEventListener(CAST_BOOK_EVENT, openPendingCast);
  }, [books]);

  useEffect(() => {
    let active = true;
    const unlisten = listen<ImportProgress>("import-progress", (event) => {
      if (active) {
        setProgress(event.payload);
        if (event.payload.type === "done") {
          setImporting(false);
          setImportFilename(null);
        }
      }
    });

    invoke<ActiveImport | null>("active_import")
      .then((running) => {
        if (!active || !running) return;
        setImporting(true);
        setImportFilename(running.filename);
        setProgress({
          type: "progress",
          page: running.page,
          total: running.total,
          stage: running.stage,
        });
      })
      .catch(console.error);

    return () => {
      active = false;
      unlisten.then((fn) => fn()).catch(console.error);
    };
  }, []);

  useEffect(() => {
    const onEdited = () => refreshExports();
    window.addEventListener("txtcaster:job-created", onEdited);
    return () => window.removeEventListener("txtcaster:job-created", onEdited);
  }, [refreshExports]);

  useEffect(() => {
    let active = true;
    let finishedFor: string | null = null;
    const settle = () => {
      if (finishedFor !== null && finishedFor === improvingIdRef.current) {
        improvingIdRef.current = null;
        setImprovingId(null);
        setImproveProgress(null);
        refresh();
      }
    };
    const progress = listen<ExtractionProgress>(
      "improve-extraction-progress",
      (event) => {
        if (active && event.payload.bookId === improvingIdRef.current) {
          setImproveProgress(event.payload);
        }
      },
    );
    const finished = listen<{ bookId: string }>(
      "improve-extraction-finished",
      (event) => {
        if (!active) return;
        finishedFor = event.payload.bookId;
        settle();
      },
    );

    Promise.all([progress, finished])
      .then(() => invoke<ActiveExtraction | null>("active_extraction"))
      .then((running) => {
        if (!active || !running) return;
        improvingIdRef.current = running.bookId;
        setImprovingId(running.bookId);
        setImproveProgress({
          bookId: running.bookId,
          page: running.page,
          pages: running.pages,
        });
        settle();
      })
      .catch(console.error);

    return () => {
      active = false;
      progress.then((fn) => fn()).catch(console.error);
      finished.then((fn) => fn()).catch(console.error);
    };
  }, [refresh]);

  useEffect(() => {
    let active = true;
    const unlistenState = listen<JobStateUpdate>("job-state", () => {
      if (!active) return;
      refreshExports();
    });
    return () => {
      active = false;
      unlistenState.then((fn) => fn()).catch(console.error);
    };
  }, [refreshExports]);

  const cancelImport = useCallback(async () => {
    setCancellingImport(true);
    try {
      await invoke<boolean>("cancel_import");
    } catch (err) {
      console.error(err);
    } finally {
      setCancellingImport(false);
    }
  }, []);

  const runImport = useCallback(
    async (path: string, layout: boolean) => {
      const filename = path.split(/[/\\]/).pop() || path;
      setPending(null);
      setImporting(true);
      setImportFilename(filename);
      setProgress(null);
      try {
        const result = await invoke<ImportResult>("import_book", {
          path,
          layout,
        });
        setNotices(presentImportWarnings(result.warnings));
        refresh();
        notifySetupChanged();
      } catch (raw) {
        const msg = String(raw);
        if (!msg.includes("import cancelled")) {
          setError(presentImportError(msg));
        }
      } finally {
        setImporting(false);
        setImportFilename(null);
        setProgress(null);
      }
    },
    [refresh],
  );

  const runImproveExtraction = useCallback(
    async (book: BookSummary, layout: boolean = true) => {
      if (improvingIdRef.current !== null) return;
      improvingIdRef.current = book.id;
      setImproveBook(null);
      setImprovingId(book.id);
      setImproveProgress(null);
      try {
        const outcome = await invoke<ExtractionOutcome>("improve_extraction", {
          bookId: book.id,
          layout,
        });
        const notice = extractionOutcomeNotice(outcome, layout);
        if (notice) setNotices([notice]);
        refresh();
      } catch (raw) {
        setError(extractionErrorMessage(String(raw)));
      } finally {
        improvingIdRef.current = null;
        setImprovingId(null);
        setImproveProgress(null);
      }
    },
    [refresh],
  );

  const improveExtraction = useCallback(
    async (book: BookSummary) => {
      if (improvingIdRef.current !== null) {
        setError(t("library.rereadBusy"));
        return;
      }
      setError(null);
      setNotices([]);
      setImproveBook(null);
      let plan: ExtractionPlan | null = null;
      try {
        plan = await invoke<ExtractionPlan>("plan_extraction", {
          bookId: book.id,
        });
      } catch (raw) {
        setError(extractionErrorMessage(String(raw)));
        return;
      }
      if (plan.blocked) {
        setError(extractionErrorMessage(plan.blocked));
        return;
      }
      const prompt = layoutPrompt(
        plan,
        book.blocksStale ? "stale" : "unanalysed",
      );
      if (prompt) {
        setImproveBook({ book, prompt });
        return;
      }
      await runImproveExtraction(book, plan.layoutAvailable && !plan.reflowed);
    },
    [runImproveExtraction, t],
  );

  const importFromPath = useCallback(
    async function importBookAt(path: string): Promise<void> {
      setError(null);
      setNotices([]);
      setPending(null);
      if (!(await ensureEngine("documents", () => void importBookAt(path)))) return;
      try {
        let plan: ImportPlan | null = null;
        try {
          plan = await invoke<ImportPlan>("plan_import", { path });
        } catch {
          // A failed estimate must never stand between someone and their book.
          plan = null;
        }
        const prompt = plan ? layoutPrompt(plan) : null;
        if (plan && prompt) {
          setPending({ path, plan, prompt });
          return;
        }
        await runImport(
          path,
          plan ? plan.layoutAvailable && !plan.reflowed && plan.silent : true,
        );
      } catch (raw) {
        setError(presentImportError(String(raw)));
      }
    },
    [runImport],
  );

  useEffect(() => {
    const path = consumePendingImport();
    if (path != null) void importFromPath(path);
  }, [importFromPath]);

  const importBook = useCallback(async () => {
    setError(null);
    setNotices([]);
    setPending(null);
    try {
      const path = await open({
        multiple: false,
        filters: [{ name: "Books", extensions: ["pdf", "epub"] }],
      });
      if (typeof path !== "string") return;
      await importFromPath(path);
    } catch (raw) {
      setError(presentImportError(String(raw)));
    }
  }, [importFromPath]);

  const deleteBook = useCallback(
    (bookId: string) => {
      invoke("delete_book", { bookId })
        .then(() => {
          refresh();
          refreshExports();
        })
        .catch((raw) => {
          setError(deleteBookErrorMessage(String(raw)));
          refresh();
          refreshExports();
        });
    },
    [refresh, refreshExports],
  );

  const deleteExport = useCallback(
    (exportId: string) => {
      invoke("delete_export", { exportId })
        .then(refreshExports)
        .catch((raw) =>
          setError(t("library.audioDeleteFailed", { reason: commandErrorText(raw) })),
        );
    },
    [refreshExports, t],
  );

  const deleteAllExports = useCallback(() => {
    invoke<{ deleted: number; failed: number }>("delete_all_exports")
      .then((outcome) => {
        if (outcome.failed > 0) {
          setError(t("library.deleteAllPartial", { count: outcome.failed }));
        }
        refreshExports();
      })
      .catch((raw) =>
        setError(t("library.deleteAllFailed", { reason: commandErrorText(raw) })),
      );
  }, [refreshExports, t]);

  const [sendToPhoneOpen, setSendToPhoneOpen] = useState(false);
  const hasBooks = books != null && books.length > 0;
  const audios = buildAudioShelf(exports, books ?? []);
  const { owned: ownedAudio, orphans: orphanAudio } = partitionOrphans(audios);
  const visibleBooks =
    filter === "casting"
      ? (books ?? []).filter((book) => castingBookIds.has(book.id))
      : (books ?? []);
  const anyPlaying = usePlayback((s) => s.playing);
  const [listenedMap, setListenedMap] = useState(loadListened);
  useEffect(() => {
    if (!anyPlaying) setListenedMap(loadListened());
  }, [anyPlaying]);
  const [introDelayMs] = useState(introRemainingMs);

  const availabilityOf = (book: BookSummary) =>
    bookAvailability(book.id, audios);
  const statusOf = (book: BookSummary): ShelfStatus => {
    const availability = availabilityOf(book);
    const playable = availability.playable;
    return shelfStatus({
      improving: improvingId === book.id,
      casting: castingBookIds.has(book.id),
      playable: playable != null,
      stale: availability.tone === "stale",
      listened: playable != null ? listenedOf(listenedMap, playable.id) : null,
      durationMs: playable?.durationMs ?? null,
    });
  };
  const shelves = partitionShelf(visibleBooks, statusOf);
  const renderShelfSlot = (book: BookSummary, index: number) => (
    <ShelfSlot
      key={book.id}
      book={book}
      index={index}
      availability={availabilityOf(book)}
      status={statusOf(book)}
      onDelete={deleteBook}
      onCreateAudio={setJobBook}
      onImproveExtraction={(target) => void improveExtraction(target)}
      onStudy={(item) =>
        onOpenSession(item, false, listenedOf(listenedMap, item.id))
      }
      improving={improvingId === book.id}
    />
  );

  const subtitle =
    segment === "books"
      ? books == null
        ? t("library.loadingShelf")
        : hasBooks
          ? t("library.booksReady", { count: books.length })
          : t("library.importToBegin")
      : audios.length > 0
        ? t("library.finishedCasts", { count: audios.length })
        : t("library.finishedAudioHere");

  return (
    <section
      data-testid="library"
      className="mx-auto flex w-full max-w-shelf flex-col gap-6 px-10 py-8"
    >
      {filter === "all" && (
        <ShelfContinue
          shelf={audios}
          listened={listenedMap}
          checklist={checklist}
          onNavigate={goToView}
          onOpenSession={onOpenSession}
        />
      )}
      <header className="flex flex-col gap-4">
        <div className="flex flex-wrap items-end justify-between gap-4">
          <div className="flex flex-wrap items-end gap-x-8 gap-y-2">
            <h1 className="font-serif text-display font-semibold tracking-tight text-ink-50">
              {t("library.title")}
            </h1>
            <LibraryTabs
              segment={segment}
              bookCount={books == null ? null : books.length}
              audioCount={audios.length}
              onFilter={onFilter}
            />
          </div>
          {segment === "books" ? (
            <div className="flex flex-wrap items-center gap-2">
              <button
                type="button"
                onClick={importBook}
                data-testid="library-import-book"
                disabled={importing}
                data-variant="solid"
                data-size="md"
                className="ui-btn inline-flex items-center gap-2 whitespace-nowrap disabled:cursor-not-allowed disabled:opacity-60"
              >
                <UploadIcon className="h-4 w-4" />
                {importing
                  ? importProgressLabel(progress)
                  : t("library.importBook")}
              </button>
            </div>
          ) : (
            <div className="flex flex-wrap items-center gap-2">
              <button
                type="button"
                data-testid="send-to-phone"
                onClick={() => setSendToPhoneOpen(true)}
                disabled={audios.length === 0}
                title={audios.length === 0 ? t("library.sendNone") : undefined}
                className="flex items-center gap-2 rounded-xl border border-ink-700 px-5 py-2.5 text-body font-semibold text-ink-200 transition-colors duration-200 hover:bg-ink-900 active:scale-[0.98] disabled:cursor-not-allowed disabled:opacity-50"
              >
                <CastIcon className="h-4 w-4" />
                {t("job.sendToPhone")}
              </button>
              {audios.length > 0 && (
                <DeleteAction
                  key={audios.length}
                  testId="all-audio"
                  label={t("library.deleteAll")}
                  question={t("library.deleteAllQuestion")}
                  confirmLabel={t("library.deleteAllConfirm", {
                    count: audios.length,
                  })}
                  cancelLabel={t("library.cancel")}
                  title={t("library.deleteAllTitle")}
                  onConfirm={deleteAllExports}
                />
              )}
            </div>
          )}
        </div>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <ShelfChips segment={segment} filter={filter} onFilter={onFilter} />
          <p className="text-secondary text-ink-400">{subtitle}</p>
        </div>
      </header>

      {pending && (
        <div
          role="group"
          aria-labelledby="layout-choice-heading"
          data-testid="layout-choice"
          className="flex flex-col gap-3 rounded-xl border border-ember-500/30 bg-ember-500/[0.07] px-5 py-4 text-body text-ember-100"
        >
          <div className="flex flex-col gap-1">
            <span id="layout-choice-heading" className="font-semibold">
              {t("library.layoutQuestion")}
            </span>
            <span aria-live="polite" className="text-ember-200/90">
              {pending.prompt.offerBoth
                ? t("library.importTradeoffEta", {
                    cpuEta: pending.prompt.cpuEta,
                    layoutEta: pending.prompt.eta,
                  })
                : t("library.layoutEta", { eta: pending.prompt.eta })}
            </span>
            <span className="text-ember-200/70">
              {pending.prompt.consequence}
            </span>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <button
              type="button"
              data-testid="layout-choice-with"
              autoFocus={!pending.prompt.heavy}
              onClick={() => void runImport(pending.path, true)}
              className={`rounded-lg px-3.5 py-2 text-secondary font-semibold transition-colors duration-200 ${
                pending.prompt.heavy
                  ? "border border-ember-500/40 text-ember-200 hover:bg-ember-500/10"
                  : "bg-ink-50 text-ink-950 hover:bg-ink-150"
              }`}
            >
              {t("library.analyseLayout", { eta: pending.prompt.eta })}
            </button>
            <button
              type="button"
              data-testid="layout-choice-without"
              autoFocus={pending.prompt.heavy}
              onClick={() => void runImport(pending.path, false)}
              className={`rounded-lg px-3.5 py-2 text-secondary font-semibold transition-colors duration-200 ${
                pending.prompt.heavy
                  ? "bg-ink-50 text-ink-950 hover:bg-ink-150"
                  : "border border-ember-500/40 text-ember-200 hover:bg-ember-500/10"
              }`}
            >
              {pending.prompt.offerBoth
                ? t("library.importWithCpu", { eta: pending.prompt.cpuEta })
                : t("library.importWithout")}
            </button>
            <button
              type="button"
              onClick={() => setPending(null)}
              className="rounded-lg px-3.5 py-2 text-secondary text-ember-300/80 transition-colors hover:text-ember-200"
            >
              {t("library.cancel")}
            </button>
          </div>
          <span className="text-secondary text-ember-200/60">
            {t("library.laterFromMenu")}
          </span>
        </div>
      )}
      {improveBook && (
        <div
          role="group"
          aria-labelledby="improve-choice-heading"
          data-testid="improve-choice"
          className="flex flex-col gap-3 rounded-xl border border-ember-500/30 bg-ember-500/[0.07] px-4 py-3 text-sm text-ember-100"
        >
          <div className="flex flex-col gap-1">
            <span id="improve-choice-heading" className="font-semibold">
              {improveBook.prompt.offerBoth
                ? t("library.rereadQuestionBoth", {
                    title: improveBook.book.title,
                  })
                : t("library.rereadQuestion", {
                    title: improveBook.book.title,
                  })}
            </span>
            <span aria-live="polite" className="text-ember-200/90">
              {improveBook.prompt.offerBoth
                ? t("library.rereadTradeoffEta", {
                    cpuEta: improveBook.prompt.cpuEta,
                    layoutEta: improveBook.prompt.eta,
                  })
                : t("library.rereadEta", { eta: improveBook.prompt.eta })}
            </span>
            <span className="text-ember-200/70">
              {improveBook.prompt.consequence}
            </span>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            {improveBook.prompt.offerBoth ? (
              <>
                <button
                  type="button"
                  data-testid="improve-choice-cpu"
                  autoFocus
                  onClick={() =>
                    void runImproveExtraction(improveBook.book, false)
                  }
                  className="rounded-lg bg-ink-50 px-3.5 py-2 text-secondary font-semibold text-ink-950 transition-colors duration-200 hover:bg-ink-150"
                >
                  {t("library.rereadCpu", { eta: improveBook.prompt.cpuEta })}
                </button>
                <button
                  type="button"
                  data-testid="improve-choice-layout"
                  onClick={() =>
                    void runImproveExtraction(improveBook.book, true)
                  }
                  className="rounded-lg border border-ember-500/40 px-3.5 py-2 text-secondary font-semibold text-ember-200 transition-colors duration-200 hover:bg-ember-500/10"
                >
                  {t("library.rereadLayout", { eta: improveBook.prompt.eta })}
                </button>
                <button
                  type="button"
                  data-testid="improve-choice-cancel"
                  onClick={() => setImproveBook(null)}
                  className="rounded-lg border border-ember-500/40 px-3.5 py-2 text-secondary font-semibold text-ember-200 transition-colors duration-200 hover:bg-ember-500/10"
                >
                  {t("library.keepAsIs")}
                </button>
              </>
            ) : (
              <>
                <button
                  type="button"
                  data-testid="improve-choice-confirm"
                  autoFocus={!improveBook.prompt.heavy}
                  onClick={() =>
                    void runImproveExtraction(improveBook.book, true)
                  }
                  className={`rounded-lg px-3.5 py-2 text-secondary font-semibold transition-colors duration-200 ${
                    improveBook.prompt.heavy
                      ? "border border-ember-500/40 text-ember-200 hover:bg-ember-500/10"
                      : "bg-ink-50 text-ink-950 hover:bg-ink-150"
                  }`}
                >
                  {t("library.rereadIt", { eta: improveBook.prompt.eta })}
                </button>
                <button
                  type="button"
                  data-testid="improve-choice-cancel"
                  autoFocus={improveBook.prompt.heavy}
                  onClick={() => setImproveBook(null)}
                  className={`rounded-lg px-3.5 py-2 text-secondary font-semibold transition-colors duration-200 ${
                    improveBook.prompt.heavy
                      ? "bg-ink-50 text-ink-950 hover:bg-ink-150"
                      : "border border-ember-500/40 text-ember-200 hover:bg-ember-500/10"
                  }`}
                >
                  {t("library.keepAsIs")}
                </button>
              </>
            )}
          </div>
        </div>
      )}
      {importing && (
        <div
          role="status"
          data-testid="import-progress"
          className="flex items-center justify-between gap-4 rounded-xl border border-sky-500/20 bg-sky-500/[0.05] px-5 py-3 text-body text-sky-200"
        >
          <div className="flex min-w-0 flex-1 flex-col gap-0.5">
            {importFilename && (
              <span className="truncate text-secondary font-medium text-sky-300">
                {importFilename}
              </span>
            )}
            <span className="text-body text-sky-200">
              {importProgressLabel(progress)}
            </span>
          </div>
          <button
            type="button"
            data-testid="import-cancel"
            disabled={cancellingImport}
            onClick={() => void cancelImport()}
            className="shrink-0 rounded-lg border border-sky-500/40 px-3.5 py-2 text-secondary font-semibold text-sky-200 transition-colors duration-200 hover:bg-sky-500/10 disabled:opacity-50"
          >
            {cancellingImport
              ? t("library.cancellingImport")
              : t("library.cancelImport")}
          </button>
        </div>
      )}
      {improvingId && (
        <div
          role="status"
          data-testid="improve-progress"
          className="flex items-center justify-between gap-4 rounded-xl border border-ember-500/20 bg-ember-500/[0.05] px-5 py-3 text-body text-ember-200"
        >
          <span>{extractionProgressLabel(improveProgress)}</span>
          <button
            type="button"
            data-testid="improve-stop"
            onClick={() => {
              void invoke<boolean>("cancel_extraction", {
                bookId: improvingId,
              })
                .then((stopping) => {
                  if (!stopping) {
                    setError(t("library.stopNotYet"));
                  }
                })
                .catch((raw) => setError(commandErrorText(raw)));
            }}
            className="shrink-0 rounded-lg border border-ember-500/40 px-3.5 py-2 text-secondary font-semibold text-ember-200 transition-colors duration-200 hover:bg-ember-500/10"
          >
            {t("voice.preview.stop")}
          </button>
        </div>
      )}
      {error && (
        <div
          role="alert"
          className="flex items-start justify-between gap-4 rounded-xl border border-wash-red-ink/20 bg-wash-red px-5 py-4 text-body text-wash-red-ink"
        >
          <div className="flex flex-wrap items-center gap-3">
            <span>{error}</span>
            <button
              type="button"
              onClick={() => {
                setError(null);
                refresh();
                refreshExports();
              }}
              className="rounded border border-wash-red-ink/30 px-2 py-0.5 text-secondary font-medium hover:bg-wash-red-ink/10"
            >
              {t("session.retry")}
            </button>
          </div>
          <button
            type="button"
            onClick={() => setError(null)}
            className="shrink-0 text-wash-red-ink/70 transition-colors hover:text-wash-red-ink"
            aria-label={t("library.dismissError")}
          >
            <CloseIcon className="h-4 w-4" />
          </button>
        </div>
      )}
      {notices.length > 0 && (
        <div
          role="status"
          className="flex items-start justify-between gap-4 rounded-xl border border-wash-blue-ink/20 bg-wash-blue px-5 py-4 text-body text-wash-blue-ink"
        >
          <div className="flex flex-col gap-1">
            {notices.map((notice) => (
              <span key={notice}>{notice}</span>
            ))}
          </div>
          <button
            type="button"
            onClick={() => setNotices([])}
            className="shrink-0 text-wash-blue-ink/70 transition-colors hover:text-wash-blue-ink"
            aria-label={t("library.dismissNotice")}
          >
            <CloseIcon className="h-4 w-4" />
          </button>
        </div>
      )}

      {segment === "books" && (
        <>
          {books == null && <ShelfSkeleton />}
          {books && books.length === 0 && (
            <button
              type="button"
              onClick={importBook}
              data-testid="empty-shelf-import"
              aria-label={t("library.importBook")}
              disabled={importing}
              className="group flex min-h-[360px] flex-1 flex-col items-center justify-center gap-4 rounded-xl border border-dashed border-ink-700 bg-ink-900 p-10 text-center transition-colors duration-300 hover:border-ember-500/50 hover:bg-ink-850"
            >
              <span className="flex h-16 w-16 items-center justify-center rounded-2xl bg-ink-800/70 text-ink-400 transition-colors duration-300 group-hover:bg-ember-500/15 group-hover:text-ember-300">
                <UploadIcon className="h-7 w-7" />
              </span>
              <div className="flex flex-col gap-2">
                <p className="font-serif text-title font-semibold text-ink-100">
                  {t("library.empty")}
                </p>
                <p className="text-body text-ink-500">
                  {t("library.emptyHint")}
                </p>
              </div>
              <span className="rounded-lg bg-ink-50 px-4 py-2 text-body font-semibold text-ink-950">{t("library.importBook")}</span>
              <div className="mt-4 flex flex-wrap items-center justify-center gap-3 text-secondary text-ink-400">
                <span className="flex items-center gap-2 rounded-md border border-ink-800 px-4 py-2">
                  <span className="font-mono text-label text-ink-500">1</span>
                  {t("library.step1")}
                </span>
                <span aria-hidden className="text-ink-500">
                  →
                </span>
                <span className="flex items-center gap-2 rounded-md border border-ink-800 px-4 py-2">
                  <span className="font-mono text-label text-ink-500">2</span>
                  {t("library.step2")}
                </span>
                <span aria-hidden className="text-ink-500">
                  →
                </span>
                <span className="flex items-center gap-2 rounded-md border border-ink-800 px-4 py-2">
                  <span className="font-mono text-label text-ink-500">3</span>
                  {t("library.step3")}
                </span>
              </div>
            </button>
          )}

          {hasBooks && filter === "casting" && visibleBooks.length === 0 && (
            <p
              data-testid="casting-empty"
              className="rounded-xl border border-dashed border-ink-700 bg-ink-900 px-6 py-10 text-center text-body text-ink-400"
            >
              {t("library.castingEmpty")}
            </p>
          )}

          {hasBooks && visibleBooks.length > 0 && (
            <div
              data-testid="book-grid"
              className="shelf flex flex-col gap-8"
              style={
                { "--intro-delay": `${introDelayMs}ms` } as React.CSSProperties
              }
            >
              <ShelfSection
                label={t("shelf.inProgress")}
                testId="shelf-in-progress"
                books={shelves.inProgress}
                startIndex={0}
                renderSlot={renderShelfSlot}
              />
              <ShelfSection
                label={t("shelf.onShelf")}
                testId="shelf-on-shelf"
                books={shelves.onShelf}
                startIndex={shelves.inProgress.length}
                renderSlot={renderShelfSlot}
              />
            </div>
          )}
        </>
      )}

      {segment === "audios" && (
        <>
          {audios.length === 0 ? (
            <div className="flex min-h-[360px] flex-1 flex-col items-center justify-center gap-4 rounded-xl border border-dashed border-ink-700 bg-ink-900 p-10 text-center">
              <span className="flex h-16 w-16 items-center justify-center rounded-2xl bg-ink-800/70 text-ink-400">
                <AudioIcon className="h-7 w-7" />
              </span>
              <div className="flex flex-col gap-2">
                <p className="font-serif text-title font-semibold text-ink-100">
                  {t("library.noAudio")}
                </p>
                <p className="text-body text-ink-500">
                  {t("library.noAudioHint")}
                </p>
                <p className="text-secondary text-ink-500">
                  {t("library.noAudioDetail")}
                </p>
              </div>
              <button
                type="button"
                data-testid="audios-empty-to-books"
                onClick={() => goToView("library")}
                className="flex items-center gap-2 rounded-xl bg-ink-50 px-5 py-2.5 text-body font-semibold text-ink-950 transition-colors duration-200 hover:bg-ink-150 active:scale-[0.98]"
              >
                <CastIcon className="h-4 w-4" />
                {t("library.browseToCast")}
              </button>
            </div>
          ) : (
            <>
              {filter === "audio" &&
                (ownedAudio.length > 0 ? (
                  <div
                    data-testid="audio-grid"
                    className="grid gap-6 grid-cols-[repeat(auto-fill,minmax(min(180px,100%),1fr))]"
                  >
                    {ownedAudio.map((item, index) => (
                      <AudioCard
                        key={item.id}
                        item={item}
                        index={index}
                        listened={listenedOf(listenedMap, item.id)}
                        onOpen={onOpenAudio}
                        onDelete={deleteExport}
                        onRecast={recastBook}
                      />
                    ))}
                  </div>
                ) : (
                  <p
                    data-testid="audio-filter-empty"
                    className="rounded-xl border border-dashed border-ink-700 bg-ink-900 px-6 py-10 text-center text-body text-ink-400"
                  >
                    {t("library.noAudio")}
                  </p>
                ))}
              {filter === "unattached" && (
                <>
                  <p className="text-label text-ink-500">
                    {t("library.unattachedHint")}
                  </p>
                  {orphanAudio.length > 0 ? (
                    <div
                      data-testid="orphan-audio-grid"
                      className="grid gap-6 grid-cols-[repeat(auto-fill,minmax(min(180px,100%),1fr))]"
                    >
                      {orphanAudio.map((item, index) => (
                        <AudioCard
                          key={item.id}
                          item={item}
                          index={index}
                          listened={listenedOf(listenedMap, item.id)}
                          onOpen={onOpenAudio}
                          onDelete={deleteExport}
                          onRecast={recastBook}
                        />
                      ))}
                    </div>
                  ) : (
                    <p
                      data-testid="unattached-empty"
                      className="rounded-xl border border-dashed border-ink-700 bg-ink-900 px-6 py-10 text-center text-body text-ink-400"
                    >
                      {t("library.unattachedEmpty")}
                    </p>
                  )}
                </>
              )}
            </>
          )}
        </>
      )}

      {jobBook && (
        <JobModal
          key={jobBook.id}
          book={jobBook}
          onClose={() => setJobBook(null)}
          onCreated={() =>
            window.dispatchEvent(new Event("txtcaster:job-created"))
          }
          onPickModels={() => {
            setJobBook(null);
            onPickModels();
          }}
          onOpenProviders={() => {
            setJobBook(null);
            onOpenProviders?.();
          }}
        />
      )}

      <SendToPhoneModal
        open={sendToPhoneOpen}
        audios={audios}
        onClose={() => setSendToPhoneOpen(false)}
      />
    </section>
  );
}

export default Library;
