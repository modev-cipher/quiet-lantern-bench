import { useRef, useState } from "react";

import {
  ChevronIcon,
  MoonIcon,
  MuteIcon,
  SkipBackIcon,
  SkipForwardIcon,
  VolumeIcon,
} from "./Icons";
import { PlayToggle } from "./AudioPlayer";
import { formatRate, formatTime, progressPercent } from "./audioPlayerModel";
import {
  CAPSULE_RATES,
  chapterAt,
  chapterLengths,
  chapterSpan,
  chapterTicks,
  loadPlayerScope,
  nextChapterTarget,
  prevChapterTarget,
  storePlayerScope,
  type PlayerScope,
} from "./playerCapsuleModel";
import { dispatch, getSnapshot } from "./playbackStore";
import { startSleep, type SleepChoice } from "./sleepTimerModel";
import { setSleep, useSleepTimer } from "./sleepTimerStore";
import { usePlayback } from "./usePlayback";
import { usePopupMenu } from "./usePopupMenu";
import { useI18n } from "./i18n";

const SKIP_BACK = 15;
const SKIP_FWD = 30;
const KEY_SEEK = 5;

export type CapsuleChapter = {
  chapterId: string;
  idx: number;
  title: string;
  startMs: number;
};

const CONTROL =
  "flex h-9 items-center justify-center gap-0.5 rounded-md text-ink-400 transition-colors duration-200 hover:bg-ink-800 hover:text-ink-100 active:scale-[0.96] disabled:pointer-events-none disabled:opacity-40";

function titleCarriesNumber(chapter: CapsuleChapter): boolean {
  const number = String(chapter.idx + 1);
  return (chapter.title.match(/[0-9]+/g) ?? ([] as string[])).includes(number);
}

function isRtl(el: Element): boolean {
  return getComputedStyle(el).direction === "rtl";
}

type Span = { startS: number; endS: number };

function Scrubber({
  chapters,
  span: liveSpan,
}: {
  chapters: readonly CapsuleChapter[];
  span: Span | null;
}) {
  const { t } = useI18n();
  const railRef = useRef<HTMLDivElement>(null);
  const current = usePlayback((s) => s.current);
  const duration = usePlayback((s) => s.duration);
  const buffering = usePlayback((s) => s.buffering);
  const [hover, setHover] = useState<number | null>(null);
  const [gesture, setGesture] = useState<{ span: Span | null } | null>(null);
  const span = gesture != null ? gesture.span : liveSpan;
  const knownDuration = Number.isFinite(duration) && duration > 0;
  const base = span?.startS ?? 0;
  const length = span != null ? span.endS - span.startS : duration;
  const percent =
    span != null
      ? progressPercent(current - base, length)
      : progressPercent(current, duration);
  const ticks = span != null ? [] : chapterTicks(chapters, duration);

  const fractionAt = (clientX: number): number | null => {
    const rail = railRef.current;
    if (rail == null) return null;
    const rect = rail.getBoundingClientRect();
    if (rect.width <= 0) return null;
    const raw = isRtl(rail)
      ? (rect.right - clientX) / rect.width
      : (clientX - rect.left) / rect.width;
    return Math.min(1, Math.max(0, raw));
  };

  const seekTo = (clientX: number) => {
    const fraction = fractionAt(clientX);
    if (fraction == null) return;
    if (span != null) {
      dispatch({
        type: "seekMs",
        ms: Math.round((base + fraction * length) * 1000),
      });
    } else {
      dispatch({ type: "seekFraction", fraction });
    }
  };

  const hoverChapter =
    hover != null && knownDuration
      ? chapterAt(chapters, base + hover * length)
      : null;

  return (
    <div
      ref={railRef}
      data-testid="player-seek"
      role="slider"
      tabIndex={0}
      aria-label={t("player.seek")}
      aria-valuemin={Math.floor(base)}
      aria-valuemax={knownDuration ? Math.floor(base + length) : 0}
      aria-valuenow={Math.floor(current)}
      aria-valuetext={formatTime(current - base)}
      onPointerDown={(event) => {
        event.currentTarget.setPointerCapture(event.pointerId);
        setGesture({ span: liveSpan });
        seekTo(event.clientX);
      }}
      onPointerUp={() => setGesture(null)}
      onPointerCancel={() => setGesture(null)}
      onLostPointerCapture={() => setGesture(null)}
      onPointerMove={(event) => {
        setHover(fractionAt(event.clientX));
        if (event.currentTarget.hasPointerCapture(event.pointerId)) {
          seekTo(event.clientX);
        }
      }}
      onPointerLeave={() => setHover(null)}
      onKeyDown={(event) => {
        const forward = isRtl(event.currentTarget) ? "ArrowLeft" : "ArrowRight";
        const back = forward === "ArrowLeft" ? "ArrowRight" : "ArrowLeft";
        if (event.key === forward) {
          event.preventDefault();
          dispatch({ type: "skip", delta: KEY_SEEK });
        } else if (event.key === back) {
          event.preventDefault();
          dispatch({ type: "skip", delta: -KEY_SEEK });
        }
      }}
      className={`group relative h-5 cursor-pointer touch-none select-none ${
        buffering ? "shimmer-bar rounded-full" : ""
      }`}
    >
      <div className="absolute start-0 top-1/2 h-1 w-full -translate-y-1/2 rounded-full bg-ink-700 transition-[height] duration-150 group-hover:h-1.5" />
      <div
        className="absolute start-0 top-1/2 h-1 -translate-y-1/2 rounded-full bg-ember-500 transition-[height] duration-150 group-hover:h-1.5"
        style={{ inlineSize: `${percent}%` }}
      />
      {ticks.map((tick) => (
        <span
          key={tick.chapterId}
          data-testid="player-chapter-tick"
          className="absolute top-1/2 h-2.5 w-px -translate-y-1/2 bg-ink-400/70"
          style={{ insetInlineStart: `${tick.fraction * 100}%` }}
        />
      ))}
      <div
        className="absolute top-1/2 h-3.5 w-3.5 -translate-y-1/2 rounded-full bg-ember-500 opacity-0 ring-4 ring-ember-500/15 transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100 ltr:-translate-x-1/2 rtl:translate-x-1/2"
        style={{ insetInlineStart: `${percent}%` }}
      />
      {hover != null && knownDuration && (
        <div
          data-testid="player-hover-time"
          className="shadow-overlay pointer-events-none absolute bottom-full mb-2 flex max-w-56 flex-col items-center rounded-md border border-ink-800 bg-ink-900 px-2 py-1 whitespace-nowrap ltr:-translate-x-1/2 rtl:translate-x-1/2"
          style={{ insetInlineStart: `${hover * 100}%` }}
        >
          <span className="font-mono text-label text-ink-100 tabular-nums">
            {formatTime(hover * length)}
          </span>
          {hoverChapter != null && span == null && (
            <span className="max-w-52 truncate text-label text-ink-400">
              {hoverChapter.title}
            </span>
          )}
        </div>
      )}
    </div>
  );
}

function SpeedMenu() {
  const { t } = useI18n();
  const rate = usePlayback((s) => s.rate);
  const { open, close, rootProps, triggerProps, menuProps } = usePopupMenu();

  return (
    <div {...rootProps} className="relative">
      <button
        type="button"
        data-testid="player-rate"
        aria-label={t("player.speed", { rate: formatRate(rate) })}
        {...triggerProps}
        className={`flex h-9 min-w-12 items-center justify-center rounded-md px-2 font-mono text-label transition-colors duration-200 hover:bg-ink-800 active:scale-[0.96] ${
          rate === 1 ? "text-ink-400 hover:text-ink-100" : "text-ember-400"
        }`}
      >
        <span dir="ltr">{formatRate(rate)}</span>
      </button>
      {open && (
        <div
          {...menuProps}
          aria-label={t("player.speedMenu")}
          data-testid="player-rate-menu"
          className="shadow-overlay absolute start-0 bottom-full z-30 mb-2 flex w-28 flex-col rounded-lg border border-ink-800 bg-ink-900 p-1"
        >
          {CAPSULE_RATES.map((option) => (
            <button
              key={option}
              type="button"
              role="menuitemradio"
              aria-checked={option === rate}
              data-testid={`player-rate-${option}`}
              onClick={() => {
                dispatch({ type: "setRate", rate: option });
                close();
              }}
              className={`rounded-md px-3 py-1.5 text-start font-mono text-label transition-colors duration-200 hover:bg-ink-800 ${
                option === rate
                  ? "font-semibold text-ember-400"
                  : "text-ink-300 hover:text-ink-100"
              }`}
            >
              <span dir="ltr">{formatRate(option)}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

const SLEEP_CHOICES: readonly SleepChoice[] = ["off", 15, 30, 45, "chapter"];

function SleepMenu({ chapters }: { chapters: readonly CapsuleChapter[] }) {
  const { t } = useI18n();
  const timer = useSleepTimer();
  const { open, close, rootProps, triggerProps, menuProps } = usePopupMenu();

  const active = timer.kind !== "off";
  const remaining =
    timer.kind === "minutes" ? formatTime(timer.remainingMs / 1000) : null;
  const label =
    timer.kind === "minutes"
      ? t("player.sleepActive", { remaining: remaining ?? "" })
      : timer.kind === "chapterEnd"
        ? t("player.sleepAtChapterEnd")
        : t("player.sleep");
  const choiceLabel = (choice: SleepChoice): string =>
    choice === "off"
      ? t("player.sleepOff")
      : choice === "chapter"
        ? t("player.sleepChapter")
        : t("player.sleepMinutes", { count: choice });
  const selected = (choice: SleepChoice): boolean =>
    choice === "off"
      ? timer.kind === "off"
      : choice === "chapter"
        ? timer.kind === "chapterEnd"
        : timer.kind === "minutes" && timer.minutes === choice;

  return (
    <div {...rootProps} className="relative">
      <button
        type="button"
        data-testid="player-sleep"
        data-state={timer.kind}
        aria-label={label}
        title={label}
        {...triggerProps}
        className={`flex h-9 items-center justify-center gap-1.5 rounded-md px-2 transition-colors duration-200 hover:bg-ink-800 active:scale-[0.96] ${
          active ? "text-ember-400" : "text-ink-400 hover:text-ink-100"
        }`}
      >
        <MoonIcon className="h-4 w-4" />
        {remaining != null && (
          <span
            data-testid="player-sleep-remaining"
            dir="ltr"
            className="font-mono text-label tabular-nums"
          >
            {remaining}
          </span>
        )}
        {timer.kind === "chapterEnd" && (
          <span className="text-label font-medium">
            {t("player.sleepChapterShort")}
          </span>
        )}
      </button>
      {open && (
        <div
          {...menuProps}
          aria-label={t("player.sleep")}
          data-testid="player-sleep-menu"
          className="shadow-overlay absolute start-0 bottom-full z-30 mb-2 flex w-44 flex-col rounded-lg border border-ink-800 bg-ink-900 p-1"
        >
          {SLEEP_CHOICES.map((choice) => (
            <button
              key={String(choice)}
              type="button"
              role="menuitemradio"
              aria-checked={selected(choice)}
              data-testid={`player-sleep-${choice}`}
              disabled={choice === "chapter" && chapters.length === 0}
              onClick={() => {
                const playback = getSnapshot();
                setSleep(
                  startSleep(choice, {
                    currentMs: playback.current * 1000,
                    chapters,
                    durationMs: Number.isFinite(playback.duration)
                      ? playback.duration * 1000
                      : 0,
                  }),
                );
                close();
              }}
              className={`rounded-md px-3 py-1.5 text-start text-secondary transition-colors duration-200 hover:bg-ink-800 disabled:opacity-40 ${
                selected(choice)
                  ? "font-semibold text-ember-400"
                  : "text-ink-300 hover:text-ink-100"
              }`}
            >
              {choiceLabel(choice)}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function ChapterMenu({
  chapters,
  chapter,
}: {
  chapters: readonly CapsuleChapter[];
  chapter: CapsuleChapter | null;
}) {
  const { t } = useI18n();
  const duration = usePlayback((s) => s.duration);
  const { open, close, rootProps, triggerProps, menuProps } = usePopupMenu();
  const lengths = chapterLengths(chapters, duration);

  return (
    <div {...rootProps} className="relative min-w-0 flex-1">
      <button
        type="button"
        data-testid="player-chapter"
        data-chapter-idx={chapter?.idx}
        {...triggerProps}
        title={t("player.chapters")}
        disabled={chapters.length < 2}
        className="-mx-1.5 flex max-w-full min-w-0 items-center gap-1 rounded-md px-1.5 py-0.5 text-start text-label text-ink-400 transition-colors duration-200 hover:bg-ink-800 hover:text-ink-200 disabled:pointer-events-none"
      >
        {chapter != null && (
          <span className="min-w-0 truncate">
            {!titleCarriesNumber(chapter) && (
              <>
                <span className="font-mono text-ink-500">
                  {t("cast.scope.chapter", { list: chapter.idx + 1 })}
                </span>
                <span className="mx-1.5 text-ink-500">·</span>
              </>
            )}
            <span className="font-medium text-ink-200">{chapter.title}</span>
          </span>
        )}
        {chapters.length > 1 && (
          <ChevronIcon
            className={`h-3.5 w-3.5 shrink-0 text-ink-500 transition-transform duration-200 ${
              open ? "-rotate-90 rtl:rotate-90" : "rotate-90 rtl:-rotate-90"
            }`}
          />
        )}
      </button>
      {open && (
        <div
          {...menuProps}
          aria-label={t("player.chapters")}
          data-testid="player-chapters-menu"
          className="setup-pop shadow-overlay absolute start-0 bottom-full z-30 mb-2 flex max-h-80 w-80 flex-col overflow-y-auto rounded-xl border border-ink-800 bg-ink-900 p-1.5"
        >
          <p className="px-2.5 pt-1 pb-1.5 text-label font-semibold text-ink-400">
            {t("player.chapters")}
          </p>
          {chapters.map((entry, at) => {
            const isCurrent = entry.chapterId === chapter?.chapterId;
            return (
              <button
                key={entry.chapterId}
                type="button"
                role="menuitemradio"
                aria-checked={isCurrent}
                tabIndex={-1}
                data-testid={`player-chapter-item-${entry.idx}`}
                onClick={() => {
                  dispatch({ type: "seekMs", ms: entry.startMs });
                  close();
                }}
                className={`flex items-baseline gap-3 rounded-lg px-2.5 py-1.5 text-start text-secondary transition-colors duration-200 hover:bg-ink-800 ${
                  isCurrent ? "bg-ink-850 text-ink-100" : "text-ink-300"
                }`}
              >
                <span
                  className={`w-5 shrink-0 font-mono text-label tabular-nums ${
                    isCurrent ? "text-ember-400" : "text-ink-500"
                  }`}
                >
                  {entry.idx + 1}
                </span>
                <span className="min-w-0 flex-1 truncate">{entry.title}</span>
                {lengths[at] != null && (
                  <span
                    dir="ltr"
                    data-testid="player-chapter-length"
                    className="shrink-0 font-mono text-label text-ink-500 tabular-nums"
                  >
                    {formatTime(lengths[at])}
                  </span>
                )}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}

function VolumeControl() {
  const { t } = useI18n();
  const muted = usePlayback((s) => s.muted);
  const volume = usePlayback((s) => s.volume);
  const level = muted ? 0 : Math.round(volume * 100);
  return (
    <div className="flex items-center gap-1.5">
      <button
        type="button"
        data-testid="player-mute"
        aria-label={muted ? t("player.unmute") : t("player.mute")}
        onClick={() => dispatch({ type: "toggleMute" })}
        className={`${CONTROL} w-9`}
      >
        {muted || volume === 0 ? (
          <MuteIcon className="h-4 w-4" />
        ) : (
          <VolumeIcon className="h-4 w-4" />
        )}
      </button>
      <input
        type="range"
        data-testid="player-volume"
        aria-label={t("player.volume")}
        min={0}
        max={100}
        step={5}
        value={level}
        onChange={(event) => {
          const next = Number(event.currentTarget.value) / 100;
          if (muted && next > 0) dispatch({ type: "toggleMute" });
          dispatch({ type: "setVolume", volume: next });
        }}
        className="h-1 w-20 cursor-pointer accent-ember-500"
      />
    </div>
  );
}

function PlayerCapsule({ chapters }: { chapters: readonly CapsuleChapter[] }) {
  const { t } = useI18n();
  const current = usePlayback((s) => s.current);
  const duration = usePlayback((s) => s.duration);
  const chapter = chapterAt(chapters, current);
  const [scope, setScope] = useState<PlayerScope>(loadPlayerScope);
  const span =
    scope === "chapter" && chapters.length > 1
      ? chapterSpan(chapters, duration, current)
      : null;
  const left =
    Number.isFinite(duration) && duration > 0
      ? formatTime(Math.max(0, (span?.endS ?? duration) - current))
      : null;
  const prev = prevChapterTarget(chapters, current);
  const next = nextChapterTarget(chapters, current);

  return (
    <div
      data-testid="player-capsule"
      className="flex w-full flex-col gap-1.5 rounded-xl border border-ink-800 bg-ink-900 px-4 pt-2.5 pb-2"
    >
      <div className="flex items-center justify-between gap-4">
        <ChapterMenu chapters={chapters} chapter={chapter} />
        <div
          data-testid="player-time"
          data-scope={span != null ? "chapter" : "book"}
          className="flex shrink-0 items-center gap-1.5 text-label text-ink-400"
        >
          <span dir="ltr" className="font-mono tabular-nums">
            {formatTime(span != null ? current - span.startS : current)}
          </span>
          {left != null && (
            <button
              type="button"
              data-testid="player-scope"
              title={
                span != null
                  ? t("player.showBookTime")
                  : t("player.showChapterTime")
              }
              onClick={() => {
                const nextScope: PlayerScope =
                  scope === "chapter" ? "book" : "chapter";
                storePlayerScope(nextScope);
                setScope(nextScope);
              }}
              className="flex items-center gap-1.5 rounded-md px-1.5 py-0.5 text-ink-500 transition-colors duration-200 hover:bg-ink-800 hover:text-ink-200"
            >
              <span className="tabular-nums">
                {span != null
                  ? t("player.leftInChapter", { time: left })
                  : t("player.leftInBook", { time: left })}
              </span>
              <span aria-hidden="true" className="text-ink-500">
                ⇄
              </span>
            </button>
          )}
        </div>
      </div>
      <Scrubber chapters={chapters} span={span} />
      <div className="grid grid-cols-[1fr_auto_1fr] items-center gap-3">
        <div className="flex items-center gap-1">
          <SpeedMenu />
          <SleepMenu chapters={chapters} />
        </div>
        <div className="flex items-center gap-1">
          <button
            type="button"
            data-testid="player-prev-chapter"
            aria-label={t("player.prevChapter")}
            title={t("player.prevChapter")}
            disabled={prev == null}
            onClick={() => {
              if (prev != null) dispatch({ type: "seekMs", ms: prev });
            }}
            className={`${CONTROL} w-9`}
          >
            <ChevronIcon className="h-4 w-4 rotate-180" />
            <ChevronIcon className="-ms-2.5 h-4 w-4 rotate-180" />
          </button>
          <button
            type="button"
            data-testid="player-skip-back"
            aria-label={t("player.rewind15")}
            title={t("player.back15Short")}
            onClick={() => dispatch({ type: "skip", delta: -SKIP_BACK })}
            className={`${CONTROL} px-2`}
          >
            <SkipBackIcon className="h-4 w-4" />
            <span className="font-mono text-label">15</span>
          </button>
          <span className="mx-1.5">
            <PlayToggle size="md" />
          </span>
          <button
            type="button"
            data-testid="player-skip-fwd"
            aria-label={t("player.forward30")}
            title={t("player.forward30Short")}
            onClick={() => dispatch({ type: "skip", delta: SKIP_FWD })}
            className={`${CONTROL} px-2`}
          >
            <span className="font-mono text-label">30</span>
            <SkipForwardIcon className="h-4 w-4" />
          </button>
          <button
            type="button"
            data-testid="player-next-chapter"
            aria-label={t("player.nextChapter")}
            title={t("player.nextChapter")}
            disabled={next == null}
            onClick={() => {
              if (next != null) dispatch({ type: "seekMs", ms: next });
            }}
            className={`${CONTROL} w-9`}
          >
            <ChevronIcon className="h-4 w-4" />
            <ChevronIcon className="-ms-2.5 h-4 w-4" />
          </button>
        </div>
        <div className="flex justify-end">
          <VolumeControl />
        </div>
      </div>
    </div>
  );
}

export default PlayerCapsule;
