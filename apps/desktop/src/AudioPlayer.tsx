import { useCallback, useRef } from "react";

import {
  MuteIcon,
  PauseIcon,
  PlayIcon,
  SkipBackIcon,
  SkipForwardIcon,
  VolumeIcon,
} from "./Icons";
import {
  formatRate,
  formatTime,
  progressPercent,
  seekFraction,
} from "./audioPlayerModel";
import { dispatch } from "./playbackStore";
import { usePlayback } from "./usePlayback";
import { useI18n } from "./i18n";

const SKIP_BACK = 15;
const SKIP_FWD = 30;
const KEY_SEEK = 5;

const TONES = {
  paper: {
    track: "bg-paper-fg-muted/25",
    time: "text-paper-fg-muted",
    control:
      "text-paper-fg-muted transition-colors hover:bg-paper-fg/10 hover:text-paper-fg active:scale-[0.97]",
    rate: "transition-colors hover:bg-paper-fg/10 active:scale-[0.97]",
    rateIdle: "text-paper-fg-muted hover:text-paper-fg",
    rateSet: "text-reading-highlight",
  },
  page: {
    track: "bg-ink-700",
    time: "text-ink-400",
    control:
      "text-ink-400 transition-colors hover:bg-ink-800 hover:text-ink-100 active:scale-[0.97]",
    rate: "transition-colors hover:bg-ink-800 active:scale-[0.97]",
    rateIdle: "text-ink-400 hover:text-ink-100",
    rateSet: "text-ember-400",
  },
} as const;

export type PlayerTone = keyof typeof TONES;

function SeekBar({
  variant,
  tone,
}: {
  variant: "full" | "mini";
  tone: PlayerTone;
}) {
  const { t } = useI18n();
  const railRef = useRef<HTMLDivElement>(null);
  const current = usePlayback((s) => s.current);
  const duration = usePlayback((s) => s.duration);
  const buffering = usePlayback((s) => s.buffering);
  const percent = progressPercent(current, duration);
  const knownDuration = Number.isFinite(duration) && duration > 0;

  const seekFromPointer = useCallback((clientX: number) => {
    const rail = railRef.current;
    if (!rail) return;
    const rect = rail.getBoundingClientRect();
    dispatch({
      type: "seekFraction",
      fraction: seekFraction(
        clientX,
        rect.left,
        rect.width,
        getComputedStyle(rail).direction === "rtl" ? "rtl" : "ltr",
      ),
    });
  }, []);

  return (
    <div
      className={`flex flex-1 flex-col ${variant === "full" ? "gap-2" : "gap-1"}`}
    >
      <div
        ref={railRef}
        data-testid="player-seek"
        role="slider"
        tabIndex={0}
        aria-label={t("player.seek")}
        aria-valuemin={0}
        aria-valuemax={knownDuration ? Math.floor(duration) : 0}
        aria-valuenow={Math.floor(current)}
        aria-valuetext={formatTime(current)}
        onPointerDown={(event) => {
          event.currentTarget.setPointerCapture(event.pointerId);
          seekFromPointer(event.clientX);
        }}
        onPointerMove={(event) => {
          if (event.currentTarget.hasPointerCapture(event.pointerId)) {
            seekFromPointer(event.clientX);
          }
        }}
        onKeyDown={(event) => {
          const rtl = getComputedStyle(event.currentTarget).direction === "rtl";
          const forward = rtl ? "ArrowLeft" : "ArrowRight";
          const back = rtl ? "ArrowRight" : "ArrowLeft";
          if (event.key === back) {
            event.preventDefault();
            dispatch({ type: "skip", delta: -KEY_SEEK });
          } else if (event.key === forward) {
            event.preventDefault();
            dispatch({ type: "skip", delta: KEY_SEEK });
          }
        }}
        className={`group relative cursor-pointer touch-none select-none ${
          variant === "full" ? "h-4" : "h-3"
        } ${buffering ? "shimmer-bar rounded-full" : ""}`}
      >
        <div
          className={`absolute start-0 top-1/2 h-1 w-full -translate-y-1/2 rounded-full ${TONES[tone].track}`}
        />
        <div
          data-testid="player-seek-fill"
          className="absolute start-0 top-1/2 h-1 -translate-y-1/2 rounded-full bg-ember-500"
          style={{ inlineSize: `${percent}%` }}
        />
        <div
          className="absolute top-1/2 h-3 w-3 -translate-y-1/2 rounded-full bg-ember-500 ring-4 ring-ember-500/15 opacity-0 transition-opacity group-hover:opacity-100 ltr:-translate-x-1/2 rtl:translate-x-1/2"
          style={{ insetInlineStart: `${percent}%` }}
        />
      </div>
      <div
        data-testid="player-time"
        className={`flex items-center justify-between font-mono tabular-nums ${TONES[tone].time} ${
          variant === "full" ? "text-secondary" : "text-label"
        }`}
      >
        <span>{formatTime(current)}</span>
        <span>{knownDuration ? formatTime(duration) : "—"}</span>
      </div>
    </div>
  );
}

export function PlayToggle({ size }: { size: "sm" | "md" | "lg" }) {
  const { t } = useI18n();
  const playing = usePlayback((s) => s.playing);
  const dim = { sm: "h-9 w-9", md: "h-12 w-12", lg: "h-16 w-16" }[size];
  const icon = { sm: "h-4 w-4", md: "h-5 w-5", lg: "h-6 w-6" }[size];
  return (
    <button
      type="button"
      data-testid="player-toggle"
      aria-label={playing ? t("player.pause") : t("player.play")}
      onClick={() => dispatch({ type: "toggle" })}
      className={`relative flex shrink-0 items-center justify-center rounded-full bg-ember-500 text-on-accent transition-transform hover:bg-ember-600 active:scale-[0.94] ${dim}`}
    >
      <span
        className={`absolute transition-all duration-200 ${
          playing ? "scale-100 opacity-100" : "scale-50 opacity-0"
        }`}
      >
        <PauseIcon className={icon} />
      </span>
      <span
        className={`absolute transition-all duration-200 ${
          playing ? "scale-50 opacity-0" : "scale-100 opacity-100"
        }`}
      >
        <PlayIcon className={`ms-0.5 ${icon}`} />
      </span>
    </button>
  );
}

function AudioControls({ tone }: { tone: PlayerTone }) {
  const { t } = useI18n();
  const rate = usePlayback((s) => s.rate);
  const muted = usePlayback((s) => s.muted);
  return (
    <div className="flex items-center gap-1">
      <button
        type="button"
        data-testid="player-skip-back"
        aria-label={t("player.rewind15")}
        title={t("player.back15Short")}
        onClick={() => dispatch({ type: "skip", delta: -SKIP_BACK })}
        className={`flex h-8 items-center gap-0.5 rounded-lg px-2 ${TONES[tone].control}`}
      >
        <SkipBackIcon className="h-4 w-4" />
        <span className="font-mono text-label">15</span>
      </button>
      <button
        type="button"
        data-testid="player-skip-fwd"
        aria-label={t("player.forward30")}
        title={t("player.forward30Short")}
        onClick={() => dispatch({ type: "skip", delta: SKIP_FWD })}
        className={`flex h-8 items-center gap-0.5 rounded-lg px-2 ${TONES[tone].control}`}
      >
        <span className="font-mono text-label">30</span>
        <SkipForwardIcon className="h-4 w-4" />
      </button>

      <button
        type="button"
        data-testid="player-rate"
        aria-label={t("player.speed", { rate: formatRate(rate) })}
        onClick={() => dispatch({ type: "cycleRate" })}
        className={`ms-auto rounded-lg px-2 py-1 font-mono text-label ${TONES[tone].rate} ${
          rate === 1 ? TONES[tone].rateIdle : TONES[tone].rateSet
        }`}
      >
        {formatRate(rate)}
      </button>
      <button
        type="button"
        data-testid="player-mute"
        aria-label={muted ? t("player.unmute") : t("player.mute")}
        onClick={() => dispatch({ type: "toggleMute" })}
        className={`flex h-8 w-8 items-center justify-center rounded-lg ${TONES[tone].control}`}
      >
        {muted ? (
          <MuteIcon className="h-4 w-4" />
        ) : (
          <VolumeIcon className="h-4 w-4" />
        )}
      </button>
    </div>
  );
}

function AudioPlayer({
  variant,
  tone = "paper",
  controls = false,
}: {
  variant: "full" | "mini";
  tone?: PlayerTone;
  controls?: boolean;
}) {
  if (variant === "mini") {
    return (
      <div className="flex flex-col gap-1">
        <div className="flex items-center gap-3">
          <PlayToggle size="sm" />
          <SeekBar variant="mini" tone={tone} />
        </div>
        {controls && <AudioControls tone={tone} />}
      </div>
    );
  }
  return (
    <div className="flex w-full flex-col gap-5">
      <div className="flex items-center gap-4">
        <PlayToggle size="lg" />
        <SeekBar variant="full" tone={tone} />
      </div>
      <AudioControls tone={tone} />
    </div>
  );
}

export default AudioPlayer;
