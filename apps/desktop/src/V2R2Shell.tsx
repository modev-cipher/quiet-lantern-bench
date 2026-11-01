import {
  AudioIcon,
  ChevronIcon,
  PauseIcon,
  PlayIcon,
  SettingsIcon,
  SkipBackIcon,
  SkipForwardIcon,
  SparkIcon,
} from "./Icons";
import type { StudioBook } from "./V2R2Data";
import { PRIMARY_BOOK } from "./V2R2Data";

export function Cover({
  book,
  className,
}: {
  book: StudioBook;
  className?: string;
}) {
  return (
    <div
      className={`flex flex-col justify-end rounded-lg ${book.tone} p-4 ${className ?? ""}`}
    >
      <p className="font-serif text-heading leading-snug font-semibold text-paper-fg">
        {book.title}
      </p>
      <p className="mt-1 font-serif text-label italic text-paper-fg-muted">
        {book.author}
      </p>
      <span className="mt-2 h-0.5 w-8 bg-ember-500" />
    </div>
  );
}

export type PillActivity =
  | { kind: "casting"; label: string; percent: number }
  | { kind: "idle" }
  | { kind: "engine"; label: string; severity: "wait" | "bad" | "unknown" };

const RING = 2 * Math.PI * 8;

export function ActivityPill({
  expanded,
  activity = { kind: "casting", label: "Casting Chapters 6–7", percent: 43 },
}: {
  expanded?: boolean;
  activity?: PillActivity;
}) {
  return (
    <button
      type="button"
      data-testid="v2r2-activity-pill"
      data-activity={activity.kind}
      aria-controls="v2r2-activity-tray"
      aria-expanded={expanded === true}
      className={`flex items-center gap-2.5 rounded-full py-1.5 transition-colors duration-200 active:scale-[0.98] ${
        activity.kind === "casting" ? "ps-2 pe-3.5" : "px-3.5"
      } ${expanded ? "bg-ink-800" : "bg-ink-850 hover:bg-ink-800"}`}
    >
      {activity.kind === "casting" && (
        <>
          <span className="relative flex h-5 w-5 items-center justify-center">
            <svg viewBox="0 0 20 20" className="h-5 w-5 -rotate-90">
              <circle
                cx="10"
                cy="10"
                r="8"
                fill="none"
                stroke="var(--color-ink-700)"
                strokeWidth="2.5"
              />
              <circle
                cx="10"
                cy="10"
                r="8"
                fill="none"
                stroke="var(--color-ember-500)"
                strokeWidth="2.5"
                strokeDasharray={RING.toFixed(2)}
                strokeDashoffset={(RING * (1 - activity.percent / 100)).toFixed(
                  2,
                )}
                strokeLinecap="round"
              />
            </svg>
          </span>
          <span className="text-label font-medium text-ink-200">
            {activity.label}
          </span>
        </>
      )}
      {activity.kind === "idle" && (
        <span className="text-label text-ink-400">Activity</span>
      )}
      {activity.kind === "engine" && (
        <>
          <span
            className={`h-2 w-2 rounded-full ${
              activity.severity === "bad"
                ? "bg-wash-red-ink"
                : activity.severity === "wait"
                  ? "bg-wash-yellow-ink"
                  : "bg-ink-500"
            }`}
          />
          <span className="text-label font-medium text-ink-200">
            {activity.label}
          </span>
        </>
      )}
    </button>
  );
}

const TRAY_ROW = "flex items-start gap-3.5 px-5 py-3.5";

export function ActivityTray() {
  return (
    <section
      id="v2r2-activity-tray"
      data-testid="v2r2-activity-tray"
      aria-label="Studio activity"
      className="animate-modal-in shadow-overlay absolute end-4 top-14 z-40 w-[400px] rounded-2xl border border-ink-800 bg-ink-900"
    >
      <header className="flex items-center justify-between border-b border-ink-800 px-5 py-3">
        <h2 className="text-body font-semibold text-ink-50">Studio activity</h2>
        <span className="flex items-center gap-2 text-label text-ink-400">
          <span className="h-2 w-2 rounded-full bg-wash-green-ink" />
          Engine ready
        </span>
      </header>
      <div className={`${TRAY_ROW} border-b border-ink-800/70`}>
        <span className="mt-1 h-2 w-2 shrink-0 rounded-full bg-ember-500" />
        <div className="min-w-0 flex-1">
          <p className="text-body font-medium text-ink-100">
            Arabic Grammar Primer
          </p>
          <p className="mt-0.5 text-secondary text-ink-300">
            Chapters 6–7 · Synthesizing · about 12 min left
          </p>
          <span className="mt-2.5 block h-1 rounded-full bg-ink-800">
            <span className="block h-1 w-[43%] rounded-full bg-ember-500" />
          </span>
          <div className="mt-2.5 flex gap-2">
            <button
              type="button"
              className="rounded-lg border border-ink-800 px-2.5 py-1 text-label font-medium text-ink-300 transition-colors duration-200 hover:text-ink-100 active:scale-[0.98]"
            >
              Pause
            </button>
            <button
              type="button"
              className="rounded-lg border border-ink-800 px-2.5 py-1 text-label font-medium text-ink-300 transition-colors duration-200 hover:text-ink-100 active:scale-[0.98]"
            >
              Cancel
            </button>
          </div>
        </div>
      </div>
      <div className={`${TRAY_ROW} border-b border-ink-800/70`}>
        <span className="mt-1 h-2 w-2 shrink-0 rounded-full bg-ink-600" />
        <div className="min-w-0 flex-1">
          <p className="text-body font-medium text-ink-200">
            The Physics of Sound
          </p>
          <p className="mt-0.5 text-secondary text-ink-400">
            Chapters 3–9 · waiting for the current cast
          </p>
        </div>
      </div>
      <div className={TRAY_ROW}>
        <span className="mt-1 h-2 w-2 shrink-0 rounded-full bg-wash-green-ink" />
        <div className="min-w-0 flex-1">
          <p className="text-body font-medium text-ink-100">
            Letters on Rhetoric
          </p>
          <p className="mt-0.5 text-secondary text-ink-300">
            New audio ready · all 12 chapters
          </p>
        </div>
        <button
          type="button"
          className="flex items-center gap-1.5 rounded-lg bg-ink-800 px-2.5 py-1.5 text-label font-semibold text-ink-100 transition-colors duration-200 hover:bg-ink-700 active:scale-[0.98]"
        >
          <AudioIcon className="h-4 w-4" /> Listen
        </button>
      </div>
      <footer className="flex items-center gap-3 border-t border-ink-800 px-5 py-3">
        <p className="min-w-0 flex-1 text-label text-ink-400">
          One cast runs at a time.
        </p>
        <button
          type="button"
          data-testid="v2r2-open-production"
          className="flex items-center gap-1 rounded-lg px-2 py-1 text-label font-medium text-ink-300 transition-colors duration-200 hover:text-ink-100 active:scale-[0.98]"
        >
          All activity & history <ChevronIcon className="h-3.5 w-3.5" />
        </button>
      </footer>
    </section>
  );
}

export function TopBar({
  destination,
  trayOpen,
}: {
  destination: "study" | "library";
  trayOpen?: boolean;
}) {
  const tab = (key: "study" | "library", label: string) => (
    <button
      type="button"
      data-testid={`v2r2-nav-${key}`}
      aria-current={destination === key ? "page" : undefined}
      className={`relative rounded-lg px-3 py-1.5 text-body transition-colors duration-200 ${
        destination === key
          ? "font-semibold text-ink-50"
          : "text-ink-400 hover:text-ink-100"
      }`}
    >
      {label}
      {destination === key && (
        <span className="absolute inset-x-3 -bottom-[9px] h-0.5 rounded-full bg-ember-500" />
      )}
    </button>
  );
  return (
    <header
      data-testid="v2r2-topbar"
      className="relative z-30 flex h-13 shrink-0 items-center gap-5 border-b border-ink-800 px-4"
    >
      <span className="flex h-8 w-8 items-center justify-center rounded-lg bg-ember-500/15 ring-1 ring-ember-500/25">
        <span className="font-serif text-heading font-semibold text-ember-300">
          t
        </span>
      </span>
      <nav aria-label="Destinations" className="flex items-center gap-1">
        {tab("study", "Study")}
        {tab("library", "Library")}
      </nav>
      <span className="flex-1" />
      <button
        type="button"
        data-testid="v2r2-settings"
        title="Appearance, models, voices, keys, storage"
        className="flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-secondary text-ink-400 transition-colors duration-200 hover:bg-ink-850 hover:text-ink-100 active:scale-[0.98]"
      >
        <SettingsIcon className="h-4 w-4" /> Settings
      </button>
      <ActivityPill expanded={trayOpen} />
    </header>
  );
}

function timelineCaption(): string {
  return PRIMARY_BOOK.availability;
}

export function Transport({
  playing,
  withTutorEntry,
}: {
  playing?: boolean;
  withTutorEntry?: boolean;
}) {
  return (
    <footer
      data-testid="v2r2-transport"
      className="paper-surface flex shrink-0 items-center gap-5 border-t border-ink-800 bg-paper-base-e px-5 py-2.5"
    >
      <div className="flex min-w-0 items-center gap-3">
        <span className="flex h-11 w-8 shrink-0 items-end rounded-sm bg-paper-tone-a p-1">
          <span className="h-0.5 w-3.5 bg-ember-500" />
        </span>
        <div className="min-w-0">
          <p className="truncate text-secondary font-semibold text-paper-fg">
            {PRIMARY_BOOK.title}
          </p>
          <p className="truncate font-mono text-label text-paper-fg-muted">
            Chapter 3 · Verb Forms
          </p>
        </div>
      </div>
      <div className="flex items-center gap-1.5">
        <button
          type="button"
          aria-label="Back 15 seconds"
          className="rounded-full p-2 text-paper-fg-muted transition-colors duration-200 hover:text-paper-fg active:scale-[0.98]"
        >
          <SkipBackIcon className="h-5 w-5" />
        </button>
        <button
          type="button"
          aria-label={playing ? "Pause" : "Play"}
          className="flex h-10 w-10 items-center justify-center rounded-full bg-ember-500 text-ink-950 transition-transform duration-200 active:scale-[0.98]"
        >
          {playing ? (
            <PauseIcon className="h-5 w-5" />
          ) : (
            <PlayIcon className="h-5 w-5" />
          )}
        </button>
        <button
          type="button"
          aria-label="Forward 30 seconds"
          className="rounded-full p-2 text-paper-fg-muted transition-colors duration-200 hover:text-paper-fg active:scale-[0.98]"
        >
          <SkipForwardIcon className="h-5 w-5" />
        </button>
      </div>
      <div className="flex min-w-0 flex-1 items-center gap-3">
        <span className="font-mono text-label text-paper-fg-muted">12:41</span>
        <div
          role="slider"
          aria-label="Listening position"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={22}
          tabIndex={0}
          data-testid="v2r2-timeline"
          className="relative h-4 min-w-0 flex-1 cursor-pointer"
        >
          <span className="absolute inset-x-0 top-1/2 h-1 -translate-y-1/2 rounded-full bg-paper-tone-e" />
          <span className="absolute inset-y-0 start-0 top-1/2 h-1 w-[71%] -translate-y-1/2 rounded-full bg-paper-fg-muted" />
          <span className="absolute start-0 top-1/2 h-1 w-[22%] -translate-y-1/2 rounded-s-full bg-ember-500" />
          <span className="absolute start-[71%] top-1/2 h-2.5 w-0.5 -translate-y-1/2 rounded-full bg-paper-fg" />
        </div>
        <span
          data-testid="v2r2-availability"
          className="shrink-0 font-mono text-label text-paper-fg-muted"
        >
          {timelineCaption()}
        </span>
      </div>
      <div className="flex shrink-0 items-center gap-2">
        <button
          type="button"
          className={`items-center gap-1.5 rounded-lg border border-ember-500/40 px-3 py-1.5 text-secondary font-medium text-ember-300 transition-colors duration-200 hover:bg-ember-500/10 active:scale-[0.98] ${
            withTutorEntry ? "flex" : "flex lg:hidden"
          }`}
        >
          <SparkIcon className="h-4 w-4" /> Ask the tutor
        </button>
        <button
          type="button"
          className="rounded-lg border border-paper-tone-e px-2.5 py-1.5 font-mono text-label text-paper-fg-muted transition-colors duration-200 hover:text-paper-fg active:scale-[0.98]"
        >
          1.0×
        </button>
        <button
          type="button"
          aria-label="Chapter list"
          className="rounded-lg p-2 text-paper-fg-muted transition-colors duration-200 hover:text-paper-fg active:scale-[0.98]"
        >
          <ChevronIcon className="h-4 w-4 rotate-90" />
        </button>
      </div>
    </footer>
  );
}
