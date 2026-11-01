import { useRef } from "react";

import type { BookSummary } from "./libraryModel";
import { DockSideIcon, MicIcon, MinimizeIcon, PopOutIcon } from "./Icons";
import type { TutorDock } from "./tutorDockModel";
import { RAIL_KEYBOARD_STEP_PX, railWidthFromPointer } from "./tutorRailModel";
import { goToView } from "./views";
import { useI18n } from "./i18n";
import Button from "./ui/Button";

export type CompanionTab = "tutor" | "book";

/**
 * The drag handle on the rail's inline-end edge. Stateless on purpose
 * (companion state must live above the Focus mount boundary): the live
 * width lives in ReadingView and arrives through callbacks. Pointer capture keeps the
 * drag on the element — no window listeners to leak.
 */
function RailResizeHandle({
  width,
  minWidth,
  maxWidth,
  onResize,
  onCommit,
  onReset,
}: {
  width: number;
  minWidth: number;
  maxWidth: number;
  onResize: (px: number) => void;
  onCommit: () => void;
  onReset: () => void;
}) {
  const { t } = useI18n();
  const railRef = useRef<HTMLDivElement>(null);
  const widthFromPointer = (clientX: number): number | null => {
    const rail = railRef.current?.parentElement;
    if (rail == null) return null;
    const direction =
      getComputedStyle(rail).direction === "rtl" ? "rtl" : "ltr";
    return railWidthFromPointer(
      clientX,
      rail.getBoundingClientRect(),
      direction,
    );
  };
  return (
    <div
      ref={railRef}
      role="separator"
      aria-orientation="vertical"
      aria-label={t("companion.resize")}
      aria-valuemin={minWidth}
      aria-valuemax={maxWidth}
      aria-valuenow={Math.round(width)}
      tabIndex={0}
      data-testid="companion-resize-handle"
      className="w-1 shrink-0 cursor-col-resize touch-none select-none self-stretch bg-transparent transition-colors duration-200 hover:bg-ink-700 focus-visible:bg-ink-700"
      onPointerDown={(event) => {
        if (event.button !== 0) return;
        event.currentTarget.setPointerCapture(event.pointerId);
      }}
      onPointerMove={(event) => {
        if (!event.currentTarget.hasPointerCapture(event.pointerId)) return;
        const next = widthFromPointer(event.clientX);
        if (next != null) onResize(next);
      }}
      onPointerUp={(event) => {
        if (event.currentTarget.hasPointerCapture(event.pointerId)) {
          event.currentTarget.releasePointerCapture(event.pointerId);
          onCommit();
        }
      }}
      // A cancelled drag (touch interruption, capture theft) fires no
      // pointerup — the applied width must still persist.
      onPointerCancel={onCommit}
      onDoubleClick={onReset}
      onKeyDown={(event) => {
        const grow = event.key === "ArrowRight" || event.key === "ArrowUp";
        const shrink = event.key === "ArrowLeft" || event.key === "ArrowDown";
        if (!grow && !shrink) return;
        event.preventDefault();
        const rtl = getComputedStyle(event.currentTarget).direction === "rtl";
        // Horizontal arrows follow the screen: the edge moves the way the
        // key points, so their meaning flips with direction. Vertical
        // arrows are direction-free (Up grows, Down shrinks).
        const delta =
          event.key === "ArrowUp" || event.key === "ArrowDown"
            ? grow
            : grow === rtl;
        onResize(width + (delta ? 1 : -1) * RAIL_KEYBOARD_STEP_PX);
        onCommit();
      }}
    />
  );
}

function TabButton({
  id,
  label,
  active,
  onSelect,
}: {
  /** Stable machine id — the testid must never derive from translated copy (I18N-2). */
  id: "tutor" | "book";
  label: string;
  active: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      role="tab"
      aria-selected={active}
      data-testid={`companion-tab-${id}`}
      onClick={onSelect}
      className={`rounded-lg px-3 py-1 text-secondary transition-colors duration-200 active:scale-[0.98] ${
        active
          ? "bg-ink-800 font-semibold text-ink-50"
          : "text-ink-400 hover:text-ink-100"
      }`}
    >
      {label}
    </button>
  );
}

export function BookPanel({
  title,
  book,
  castChapters,
}: {
  title: string;
  book: BookSummary | null;
  castChapters: number | null;
}) {
  const { t } = useI18n();
  return (
    <div
      data-testid="companion-book"
      className="flex h-full w-full shrink-0 flex-col gap-3 border-s border-ink-800 bg-ink-900 px-4 py-4"
    >
      <p className="font-serif text-heading leading-snug font-semibold text-ink-50">
        {title}
      </p>
      {book?.author != null && (
        <p className="font-serif text-secondary italic text-ink-300">
          {book.author}
        </p>
      )}
      <dl className="flex flex-col gap-1.5 text-secondary text-ink-300">
        {book?.pageCount != null && (
          <div className="flex justify-between gap-3">
            <dt className="text-ink-500">{t("companion.pages")}</dt>
            <dd className="font-mono text-label">{book.pageCount}</dd>
          </div>
        )}
        {book != null && (
          <div className="flex justify-between gap-3">
            <dt className="text-ink-500">
              {book.hasPseudoChapters
                ? t("companion.sections")
                : t("companion.chapters")}
            </dt>
            <dd className="font-mono text-label">{book.chapterCount}</dd>
          </div>
        )}
        {castChapters != null && (
          <div className="flex justify-between gap-3">
            <dt className="text-ink-500">{t("companion.inThisCast")}</dt>
            <dd className="font-mono text-label">{castChapters}</dd>
          </div>
        )}
        {book?.language != null && (
          <div className="flex justify-between gap-3">
            <dt className="text-ink-500">{t("companion.language")}</dt>
            <dd className="font-mono text-label uppercase">{book.language}</dd>
          </div>
        )}
      </dl>
      <span className="flex-1" />
      <Button
        variant="soft"
        size="sm"
        className="self-start"
        data-testid="companion-open-library"
        onClick={() => goToView("library")}
      >
        {t("companion.openInLibrary")}
      </Button>
    </div>
  );
}

function ChromeButton({
  buttonRef,
  testId,
  label,
  onClick,
  children,
}: {
  buttonRef?: (el: HTMLButtonElement | null) => void;
  testId: string;
  label: string;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <Button
      ref={buttonRef}
      icon
      size="sm"
      label={label}
      data-testid={testId}
      onClick={onClick}
    >
      {children}
    </Button>
  );
}

function StudyCompanion({
  title,
  book,
  castChapters,
  tutor,
  bookAgent,
  tab,
  onSelectTab,
  width,
  minWidth,
  maxWidth,
  onResize,
  onResizeCommit,
  onResizeReset,
  dock,
  floatSize,
  live,
  onFloat,
  onDock,
  onFold,
  onMove,
  onMoveCommit,
}: {
  title: string;
  book: BookSummary | null;
  castChapters: number | null;
  tutor: React.ReactNode;
  /** The Book Agent composer (charter B-4); metadata panel when absent. */
  bookAgent?: React.ReactNode;
  tab: CompanionTab;
  onSelectTab: (tab: CompanionTab) => void;
  width: number;
  minWidth: number;
  maxWidth: number;
  onResize: (px: number) => void;
  onResizeCommit: () => void;
  onResizeReset: () => void;
  dock: TutorDock;
  floatSize: { width: number; height: number };
  live: boolean;
  onFloat: () => void;
  onDock: () => void;
  onFold: (folded: boolean) => void;
  onMove: (x: number, y: number) => void;
  onMoveCommit: () => void;
}) {
  const { t } = useI18n();
  const grabRef = useRef<{ dx: number; dy: number } | null>(null);
  const focusAfterFoldRef = useRef<"pill" | "window" | null>(null);
  const floating = dock.mode === "floating";
  const folded = floating && dock.folded;
  return (
    <>
      <aside
        data-testid="study-companion"
        aria-label={t("companion.aria")}
        hidden={folded}
        className={
          floating
            ? "shadow-overlay fixed z-40 flex flex-row overflow-hidden rounded-xl border border-ink-800 bg-ink-900"
            : "flex h-full shrink-0 flex-row"
        }
        style={
          floating
            ? {
                left: dock.x,
                top: dock.y,
                inlineSize: floatSize.width,
                blockSize: floatSize.height,
              }
            : { inlineSize: width }
        }
      >
        {!floating && (
          <RailResizeHandle
            width={width}
            minWidth={minWidth}
            maxWidth={maxWidth}
            onResize={onResize}
            onCommit={onResizeCommit}
            onReset={onResizeReset}
          />
        )}
        <div className="flex h-full min-w-0 flex-1 flex-col">
          <div
            data-testid="companion-drag"
            className={`flex shrink-0 touch-none items-center gap-1 border-b border-ink-800 bg-ink-900 px-2.5 py-1.5 select-none ${
              floating ? "cursor-move" : "border-s"
            }`}
            onPointerDown={(event) => {
              if (!floating || event.button !== 0) return;
              if ((event.target as HTMLElement).closest("button") != null) {
                return;
              }
              grabRef.current = {
                dx: event.clientX - dock.x,
                dy: event.clientY - dock.y,
              };
              event.currentTarget.setPointerCapture(event.pointerId);
            }}
            onPointerMove={(event) => {
              const grab = grabRef.current;
              if (grab == null) return;
              onMove(event.clientX - grab.dx, event.clientY - grab.dy);
            }}
            onPointerUp={(event) => {
              if (grabRef.current == null) return;
              grabRef.current = null;
              event.currentTarget.releasePointerCapture(event.pointerId);
              onMoveCommit();
            }}
            onPointerCancel={() => {
              if (grabRef.current == null) return;
              grabRef.current = null;
              onMoveCommit();
            }}
          >
            <div
              role="tablist"
              aria-label={t("companion.panels")}
              className="flex items-center gap-1"
            >
              <TabButton
                id="tutor"
                label={t("companion.tabTutor")}
                active={tab === "tutor"}
                onSelect={() => onSelectTab("tutor")}
              />
              <TabButton
                id="book"
                label={t("companion.tabBook")}
                active={tab === "book"}
                onSelect={() => onSelectTab("book")}
              />
            </div>
            <div className="ms-auto flex items-center gap-0.5">
              {floating ? (
                <>
                  <ChromeButton
                    buttonRef={(el) => {
                      if (
                        el != null &&
                        focusAfterFoldRef.current === "window"
                      ) {
                        focusAfterFoldRef.current = null;
                        el.focus();
                      }
                    }}
                    testId="companion-fold"
                    label={t("companion.fold")}
                    onClick={() => {
                      focusAfterFoldRef.current = "pill";
                      onFold(true);
                    }}
                  >
                    <MinimizeIcon className="h-4 w-4" />
                  </ChromeButton>
                  <ChromeButton
                    testId="companion-dock"
                    label={t("companion.dock")}
                    onClick={onDock}
                  >
                    <DockSideIcon className="h-4 w-4" />
                  </ChromeButton>
                </>
              ) : (
                <ChromeButton
                  testId="companion-float"
                  label={t("companion.float")}
                  onClick={onFloat}
                >
                  <PopOutIcon className="h-4 w-4" />
                </ChromeButton>
              )}
            </div>
          </div>
          <div className="min-h-0 flex-1">
            {tab === "tutor"
              ? tutor
              : (bookAgent ?? (
                  <BookPanel
                    title={title}
                    book={book}
                    castChapters={castChapters}
                  />
                ))}
          </div>
        </div>
      </aside>
      {folded && (
        <div
          className="pointer-events-none fixed z-40 flex justify-end"
          style={{
            left: dock.x,
            top: dock.y,
            inlineSize: floatSize.width,
          }}
        >
          <button
            type="button"
            data-testid="companion-pill"
            title={t("companion.unfold")}
            ref={(el) => {
              if (el != null && focusAfterFoldRef.current === "pill") {
                focusAfterFoldRef.current = null;
                el.focus();
              }
            }}
            onClick={() => {
              focusAfterFoldRef.current = "window";
              onFold(false);
            }}
            className="shadow-overlay pointer-events-auto flex items-center gap-2 rounded-lg border border-ink-800 bg-ink-900 px-3.5 py-2 text-secondary font-semibold text-ink-100 transition-colors duration-200 hover:bg-ink-800 active:scale-[0.98]"
          >
            <MicIcon className="h-4 w-4 text-ember-400" />
            {t("companion.tabTutor")}
            {live && (
              <span
                data-testid="companion-pill-live"
                className="flex items-center gap-1.5 text-label font-medium text-ember-400"
              >
                <span className="h-1.5 w-1.5 rounded-full bg-ember-400" />
                {t("companion.live")}
              </span>
            )}
          </button>
        </div>
      )}
    </>
  );
}

export default StudyCompanion;
