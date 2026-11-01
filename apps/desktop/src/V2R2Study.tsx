import { ChevronIcon, CloseIcon, MicIcon } from "./Icons";
import {
  PRIMARY_BOOK,
  PRIMER_CHAPTERS,
  READER_PARAGRAPHS,
  READER_PARAGRAPHS_AR,
  TUTOR_TURNS,
} from "./V2R2Data";
import { ActivityTray, TopBar, Transport } from "./V2R2Shell";

const SEGMENT_ACTIVE =
  "rounded-full bg-ember-500/15 px-3 py-1 font-mono text-label tracking-wide text-ember-200 uppercase";
const SEGMENT_QUIET =
  "rounded-full px-3 py-1 font-mono text-label tracking-wide text-ink-300 uppercase transition-colors hover:text-ember-300 active:scale-[0.98]";

function chapterStateLine(state: "ready" | "casting" | "none"): string | null {
  if (state === "casting") return "casting";
  if (state === "none") return "no audio yet";
  return null;
}

export function OutlineRail() {
  return (
    <nav
      data-testid="v2r2-outline"
      aria-label="Chapters"
      className="hidden w-60 shrink-0 flex-col overflow-y-auto border-e border-ink-800 lg:flex"
    >
      <div className="border-b border-ink-800/70 px-5 pt-5 pb-4">
        <p className="font-serif text-heading leading-snug font-semibold text-ink-50">
          {PRIMARY_BOOK.title}
        </p>
        <p className="mt-1 font-mono text-label text-ink-400">
          {PRIMARY_BOOK.meta}
        </p>
      </div>
      <ol className="flex flex-col gap-0.5 p-2.5">
        {PRIMER_CHAPTERS.map((chapter) => {
          const state = chapterStateLine(chapter.state);
          return (
            <li key={chapter.no}>
              <button
                type="button"
                aria-current={chapter.current ? "true" : undefined}
                className={`relative flex w-full items-baseline gap-2.5 rounded-lg px-3 py-2 text-start transition-colors duration-200 ${
                  chapter.current
                    ? "bg-ink-850 text-ink-50"
                    : "text-ink-300 hover:bg-ink-850 hover:text-ink-100"
                }`}
              >
                {chapter.current && (
                  <span className="absolute inset-y-2 start-0 w-0.5 rounded-full bg-ember-500" />
                )}
                <span className="w-4 shrink-0 font-mono text-label text-ink-500">
                  {chapter.no}
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-secondary font-medium">
                    {chapter.title}
                  </span>
                  {state != null && (
                    <span
                      className={`block text-label ${
                        chapter.state === "casting"
                          ? "text-ember-400"
                          : "text-ink-500"
                      }`}
                    >
                      {state}
                    </span>
                  )}
                </span>
              </button>
            </li>
          );
        })}
      </ol>
      <p className="mt-auto border-t border-ink-800/70 px-5 py-3.5 text-label leading-relaxed text-ink-500">
        {PRIMARY_BOOK.availability}. Chapters 6–7 will appear here when their
        cast finishes.
      </p>
    </nav>
  );
}

function CompanionTab({ label, active }: { label: string; active?: boolean }) {
  return (
    <button
      type="button"
      role="tab"
      aria-selected={active === true}
      className={`rounded-lg px-3 py-1.5 text-secondary transition-colors duration-200 ${
        active
          ? "bg-ink-800 font-semibold text-ink-50"
          : "text-ink-400 hover:text-ink-100"
      }`}
    >
      {label}
    </button>
  );
}

export function Companion() {
  return (
    <aside
      data-testid="v2r2-companion"
      aria-label="Companion"
      className="hidden w-[21.5rem] shrink-0 flex-col border-e border-ink-800 bg-ink-900 lg:flex"
    >
      <div className="flex items-center gap-1 border-b border-ink-800/70 px-3 py-2.5">
        <div
          role="tablist"
          aria-label="Companion panels"
          className="flex gap-1"
        >
          <CompanionTab label="Tutor" active />
          <CompanionTab label="Book" />
        </div>
        <span className="flex-1" />
        <button
          type="button"
          aria-label="Collapse companion"
          className="rounded-lg p-1.5 text-ink-400 transition-colors duration-200 hover:text-ink-100 active:scale-[0.98]"
        >
          <ChevronIcon className="h-4 w-4 rotate-180" />
        </button>
      </div>
      <div className="flex items-center gap-2 px-4 pt-3.5">
        <span className="rounded-full border border-ink-700 bg-ink-800 px-2.5 py-0.5 text-label font-medium text-ink-100">
          Live
        </span>
        <span className="rounded-full border border-ink-800 px-2.5 py-0.5 text-label text-ink-400">
          Audio on
        </span>
        <span className="rounded-full border border-ink-800 px-2.5 py-0.5 text-label text-ink-400">
          Listening
        </span>
        <span className="flex-1" />
        <button
          type="button"
          className="rounded-lg border border-ink-800 px-2.5 py-1 text-label text-ink-400 transition-colors duration-200 hover:text-ink-100 active:scale-[0.98]"
        >
          End
        </button>
      </div>
      <p className="px-4 pt-2 text-label text-ink-500">
        Paused at Chapter 3 · 12:41 — this conversation stays with the book.
      </p>
      <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-4 py-4">
        {TUTOR_TURNS.map((turn, index) => (
          <div
            key={index}
            className={`max-w-[90%] rounded-xl px-3.5 py-2.5 ${
              turn.from === "you"
                ? "self-end bg-ink-850 text-ink-100"
                : "self-start bg-ink-800 text-ink-50"
            }`}
          >
            {turn.excerpt != null && (
              <p className="mb-2 border-s-2 border-ember-500 ps-2.5 font-serif text-label italic text-ink-300">
                “{turn.excerpt}”
              </p>
            )}
            <p className="text-secondary leading-relaxed">{turn.text}</p>
          </div>
        ))}
      </div>
      <div className="flex items-center gap-2.5 border-t border-ink-800/70 px-4 py-3">
        <button
          type="button"
          aria-label="Hold to talk"
          className="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-ember-500 text-ink-950 transition-transform duration-200 active:scale-[0.98]"
        >
          <MicIcon className="h-5 w-5" />
        </button>
        <label className="min-w-0 flex-1">
          <span className="sr-only">Type a question</span>
          <input
            type="text"
            placeholder="Type instead…"
            className="w-full rounded-xl border border-ink-800 bg-ink-950 px-3.5 py-2 text-secondary text-ink-100 placeholder:text-ink-500"
          />
        </label>
      </div>
    </aside>
  );
}

function FacsimileThumb() {
  return (
    <aside
      aria-label="Printed page preview"
      className="hidden w-64 shrink-0 xl:block"
    >
      <div className="shadow-overlay aspect-[3/4] rounded-md border border-ink-800 bg-reading-page p-5">
        <span className="block h-2.5 w-2/3 rounded-sm bg-ink-600" />
        <div className="mt-5 flex flex-col gap-2">
          {Array.from({ length: 16 }, (_v, line) => (
            <span
              key={line}
              className={`block h-1.5 rounded-sm bg-ink-700 ${
                line % 5 === 4 ? "w-3/5" : "w-full"
              }`}
            />
          ))}
        </div>
      </div>
      <p className="mt-2 text-center font-mono text-label text-ink-500">
        Page 41
      </p>
    </aside>
  );
}

export function Reader({
  arabic,
  focus,
  withThumb,
  tutorLive,
}: {
  arabic?: boolean;
  focus?: boolean;
  withThumb?: boolean;
  tutorLive?: boolean;
}) {
  const paragraphs = arabic ? READER_PARAGRAPHS_AR : READER_PARAGRAPHS;
  return (
    <section data-testid="v2r2-reader" className="flex min-w-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center gap-4 px-6 py-2.5">
        <p className="min-w-0 truncate font-mono text-label text-ink-400">
          Chapter 3 · Verb Forms
        </p>
        <span className="flex-1" />
        <div
          role="radiogroup"
          aria-label="Reading mode"
          className="flex items-center gap-1"
        >
          <button
            type="button"
            role="radio"
            aria-checked="true"
            className={SEGMENT_ACTIVE}
          >
            Script
          </button>
          <button
            type="button"
            role="radio"
            aria-checked="false"
            className={SEGMENT_QUIET}
          >
            Page
          </button>
        </div>
        {focus ? (
          <button
            type="button"
            className="flex items-center gap-1.5 rounded-lg border border-ink-800 px-3 py-1.5 text-secondary font-medium text-ink-200 transition-colors duration-200 hover:text-ink-50 active:scale-[0.98]"
          >
            <CloseIcon className="h-4 w-4" /> Exit focus
          </button>
        ) : (
          <button
            type="button"
            className="rounded-lg border border-ink-800 px-3 py-1.5 text-secondary font-medium text-ink-300 transition-colors duration-200 hover:text-ink-100 active:scale-[0.98]"
          >
            Focus
          </button>
        )}
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto flex max-w-[1500px] justify-center gap-12 px-8 py-8">
          <div
            lang={arabic ? "ar" : undefined}
            className="min-w-0 max-w-[38rem] flex-1 font-serif text-2xl leading-relaxed tracking-tight text-ink-100"
          >
            {paragraphs.map((paragraph, index) => (
              <p key={index} className={index > 0 ? "mt-6" : undefined}>
                {paragraph.highlight && !tutorLive ? (
                  <span className="rounded-sm bg-reading-highlight px-1 text-reading-highlight-fg">
                    {paragraph.text}
                  </span>
                ) : (
                  paragraph.text
                )}
              </p>
            ))}
          </div>
          {withThumb && <FacsimileThumb />}
        </div>
      </div>
    </section>
  );
}

function EdgeHandle({ label }: { label: string }) {
  return (
    <button
      type="button"
      aria-label={label}
      className="flex w-6 shrink-0 items-center justify-center border-e border-ink-800 text-ink-500 transition-colors duration-200 last:border-e-0 last:border-s hover:text-ink-200"
    >
      <ChevronIcon className="h-4 w-4" />
    </button>
  );
}

export function StudyScreen({
  companionOpen,
  focus,
  resumeChip,
  arabic,
  tray,
}: {
  companionOpen?: boolean;
  focus?: boolean;
  resumeChip?: boolean;
  arabic?: boolean;
  tray?: boolean;
}) {
  return (
    <div className="relative flex h-screen flex-col bg-ink-950 text-ink-100">
      {!focus && <TopBar destination="study" trayOpen={tray} />}
      {tray && <ActivityTray />}
      <div className="flex min-h-0 flex-1">
        {focus ? (
          <EdgeHandle label="Show chapters and companion" />
        ) : (
          <>
            <OutlineRail />
            {companionOpen && <Companion />}
          </>
        )}
        <Reader
          arabic={arabic}
          focus={focus}
          withThumb={focus || !companionOpen}
          tutorLive={companionOpen}
        />
      </div>
      {resumeChip && (
        <div className="pointer-events-none absolute inset-x-0 bottom-20 flex justify-center">
          <p
            data-testid="v2r2-resume-chip"
            className="animate-rise-in pointer-events-auto flex items-center gap-2.5 rounded-full border border-ink-800 bg-ink-900 py-1.5 ps-4 pe-2 text-secondary text-ink-200 shadow-overlay"
          >
            Resumed where you left off · Chapter 3 · 12:41
            <button
              type="button"
              aria-label="Dismiss"
              className="rounded-full p-1 text-ink-400 transition-colors duration-200 hover:text-ink-100"
            >
              <CloseIcon className="h-3.5 w-3.5" />
            </button>
          </p>
        </div>
      )}
      <Transport
        playing={!resumeChip && !companionOpen}
        withTutorEntry={!companionOpen}
      />
    </div>
  );
}
