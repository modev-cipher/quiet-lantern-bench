import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import { CloseIcon, DockSideIcon, MinimizeIcon, PopOutIcon } from "./Icons";
import { TUTOR_LAUNCHER_ID } from "./TutorLauncher";
import VoiceSessionPanel from "./VoiceSessionPanel";
import {
  buildAudioShelf,
  type AudioShelfItem,
  type ExportSummary,
} from "./audioShelfModel";
import { bookDisplayTitle, type BookSummary } from "./libraryModel";
import { bookAvailability } from "./libraryAvailability";
import {
  FLOAT_MAX_WIDTH_PX,
  clampFloat,
  defaultFloat,
  floatHeight,
  type TutorDock,
} from "./tutorDockModel";
import { useDialogFocus } from "./useDialogFocus";
import { consumePendingTutorBook } from "./views";
import { useI18n } from "./i18n";
import Button from "./ui/Button";

interface TalkTarget {
  book: BookSummary;
  shelf: AudioShelfItem[];
}

function useViewport() {
  const [size, setSize] = useState(() => ({
    width: window.innerWidth,
    height: window.innerHeight,
  }));
  useEffect(() => {
    const onResize = () =>
      setSize({ width: window.innerWidth, height: window.innerHeight });
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);
  return size;
}

function HeaderButton({
  testId,
  label,
  onClick,
  children,
}: {
  testId: string;
  label: string;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <Button icon size="sm" label={label} data-testid={testId} onClick={onClick}>
      {children}
    </Button>
  );
}

function TutorPanel({
  dock,
  onDock,
  onMoveCommit,
  onClose,
}: {
  dock: TutorDock;
  onDock: (next: TutorDock) => void;
  onMoveCommit: (next: TutorDock) => void;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const panelRef = useRef<HTMLElement>(null);
  const grabRef = useRef<{ dx: number; dy: number } | null>(null);
  const viewport = useViewport();
  const floatSize = {
    width: Math.min(FLOAT_MAX_WIDTH_PX, viewport.width - 32),
    height: floatHeight(viewport.height),
  };
  const floating = dock.mode === "floating";
  const folded = floating && dock.folded;
  const place = floating
    ? clampFloat(dock, floatSize.width, floatSize.height, viewport)
    : null;
  const [talkBookId] = useState(consumePendingTutorBook);
  const [target, setTarget] = useState<TalkTarget | null>(null);
  const [resolved, setResolved] = useState(talkBookId == null);
  useDialogFocus(panelRef);
  const shownFolded = useRef(folded);

  useEffect(() => {
    if (shownFolded.current === folded) return;
    shownFolded.current = folded;
    if (folded) document.getElementById(TUTOR_LAUNCHER_ID)?.focus();
    else panelRef.current?.focus();
  }, [folded]);

  useEffect(() => {
    if (talkBookId == null) return;
    let active = true;
    Promise.all([
      invoke<ExportSummary[]>("list_exports"),
      invoke<BookSummary[]>("list_books"),
    ])
      .then(([exports, books]) => {
        if (!active) return;
        const book = books.find((entry) => entry.id === talkBookId) ?? null;
        if (book != null) {
          setTarget({ book, shelf: buildAudioShelf(exports, books) });
        }
        setResolved(true);
      })
      .catch((error: unknown) => {
        console.error(error);
        if (active) setResolved(true);
      });
    return () => {
      active = false;
    };
  }, [talkBookId]);

  const talkBook = target?.book ?? null;
  const talkJobId =
    talkBook != null && target != null
      ? (bookAvailability(talkBook.id, target.shelf).playable?.jobId ?? null)
      : null;

  return (
    <aside
      ref={panelRef}
      tabIndex={-1}
      id="tutor-panel"
      data-testid="tutor-panel"
      data-dock={floating ? "floating" : "docked"}
      aria-label={t("voice.title")}
      hidden={folded}
      onKeyDown={(event) => {
        if (event.key !== "Escape" || event.defaultPrevented) return;
        if (floating) onDock({ ...dock, folded: true });
        else onClose();
      }}
      className={
        floating
          ? "tutor-float shadow-overlay fixed z-40 flex flex-col overflow-y-auto rounded-xl border border-ink-800 bg-ink-900 outline-none"
          : "animate-fade-in flex w-[min(24rem,45%)] shrink-0 flex-col overflow-y-auto border-s border-ink-800 bg-ink-900 outline-none"
      }
      style={
        place != null
          ? {
              left: place.x,
              top: place.y,
              inlineSize: floatSize.width,
              blockSize: floatSize.height,
            }
          : undefined
      }
    >
      <div
        data-testid="tutor-panel-drag"
        className={`sticky top-0 z-10 flex shrink-0 touch-none items-center justify-between gap-3 bg-ink-900 select-none ${
          floating
            ? "cursor-move border-b border-ink-800 px-4 py-2"
            : "px-5 pt-4"
        }`}
        onPointerDown={(event) => {
          if (place == null || event.button !== 0) return;
          if ((event.target as HTMLElement).closest("button") != null) return;
          grabRef.current = {
            dx: event.clientX - place.x,
            dy: event.clientY - place.y,
          };
          event.currentTarget.setPointerCapture(event.pointerId);
        }}
        onPointerMove={(event) => {
          const grab = grabRef.current;
          if (grab == null || dock.mode !== "floating") return;
          onDock({
            ...dock,
            ...clampFloat(
              { x: event.clientX - grab.dx, y: event.clientY - grab.dy },
              floatSize.width,
              floatSize.height,
              viewport,
            ),
          });
        }}
        onPointerUp={(event) => {
          if (grabRef.current == null) return;
          grabRef.current = null;
          event.currentTarget.releasePointerCapture(event.pointerId);
          onMoveCommit(dock);
        }}
        onPointerCancel={() => {
          if (grabRef.current == null) return;
          grabRef.current = null;
          onMoveCommit(dock);
        }}
      >
        <p className="text-label font-semibold tracking-[0.14em] text-ink-500 uppercase">
          {t("voice.title")}
        </p>
        <div className="flex items-center gap-0.5">
          {floating ? (
            <>
              <HeaderButton
                testId="tutor-panel-fold"
                label={t("companion.fold")}
                onClick={() => onDock({ ...dock, folded: true })}
              >
                <MinimizeIcon className="h-4 w-4" />
              </HeaderButton>
              <HeaderButton
                testId="tutor-panel-dock"
                label={t("companion.dock")}
                onClick={() => onDock({ mode: "docked" })}
              >
                <DockSideIcon className="h-4 w-4" />
              </HeaderButton>
            </>
          ) : (
            <HeaderButton
              testId="tutor-panel-float"
              label={t("companion.float")}
              onClick={() =>
                onDock(
                  defaultFloat(
                    viewport,
                    floatSize.width,
                    getComputedStyle(document.documentElement).direction ===
                      "rtl"
                      ? "rtl"
                      : "ltr",
                  ),
                )
              }
            >
              <PopOutIcon className="h-4 w-4" />
            </HeaderButton>
          )}
          <HeaderButton
            testId="tutor-panel-close"
            label={t("nav.closeTutor")}
            onClick={onClose}
          >
            <CloseIcon className="h-5 w-5" />
          </HeaderButton>
        </div>
      </div>
      {resolved && talkBook != null && (
        <section
          data-testid="study-book-tutor"
          className={floating ? "px-4 pt-3 pb-4" : "px-5 pb-5"}
        >
          <h2 className="mt-1 font-serif text-title font-semibold text-ink-100">
            {bookDisplayTitle(talkBook)}
          </h2>
          {talkJobId == null && (
            <p
              data-testid="study-book-tutor-no-cast"
              className="mt-1 text-secondary text-ink-400"
            >
              {t("study.noCastYet")}
            </p>
          )}
          <VoiceSessionPanel
            bookId={talkBook.id}
            jobId={talkJobId}
            variant="side"
          />
        </section>
      )}
      {resolved && talkBook == null && (
        <section
          data-testid="study-standalone-tutor"
          className={floating ? "px-4 pt-3 pb-4" : "px-5 pb-5"}
        >
          <p className="mt-1 text-secondary text-ink-400">
            {t("study.standaloneBody")}
          </p>
          <VoiceSessionPanel bookId={null} jobId={null} variant="side" />
        </section>
      )}
    </aside>
  );
}

export default TutorPanel;
