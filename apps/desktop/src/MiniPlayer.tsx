import { useRef } from "react";

import { armThreshold } from "./thresholdModel";
import { AudioIcon, CloseIcon } from "./Icons";
import { dispatch } from "./playbackStore";
import { usePlayback } from "./usePlayback";
import AudioPlayer from "./AudioPlayer";
import { modeLabel } from "./audioShelfModel";
import TypographicCover from "./TypographicCover";
import { useI18n } from "./i18n";

function MiniPlayer({ onFollowAlong }: { onFollowAlong: (() => void) | null }) {
  const { t } = useI18n();
  const coverRef = useRef<HTMLDivElement>(null);
  const item = usePlayback((s) => s.item);
  const error = usePlayback((s) => s.error);
  const errorMessage = usePlayback((s) => s.errorMessage);
  if (!item) return null;
  const playbackError = errorMessage ?? (error ? t("player.playbackFailed") : null);
  const followAlong =
    onFollowAlong == null
      ? null
      : () => {
          armThreshold(coverRef.current, item.bookId || item.title, item.title);
          onFollowAlong();
        };

  return (
    <div className="pointer-events-none absolute inset-x-0 bottom-0 z-40 flex justify-center px-6 pb-5">
      <div
        data-testid="mini-player"
        className="paper-surface animate-rise-in pointer-events-auto flex w-full max-w-3xl flex-wrap items-center gap-x-4 gap-y-2 rounded-xl border border-paper-fg/10 bg-paper-base-e/95 px-4 py-3 shadow-overlay backdrop-blur md:flex-nowrap"
      >
        <div
          ref={coverRef}
          className="relative h-12 w-12 shrink-0 overflow-hidden rounded-lg ring-1 ring-paper-fg/15"
        >
          <TypographicCover
            seed={item.bookId || item.title}
            title={item.title}
            size="sm"
          />
          {followAlong && (
            <button
              type="button"
              data-testid="mini-player-expand"
              aria-label={t("reader.followAlong")}
              onClick={followAlong}
              className="absolute inset-0 flex items-center justify-center bg-ink-950/40 text-ember-300 opacity-0 transition-opacity hover:opacity-100 focus-visible:opacity-100 active:scale-[0.97]"
            >
              <AudioIcon className="h-4 w-4" />
            </button>
          )}
        </div>

        {/* Below md the seek strip cannot share the row with the action
            cluster — it collapses under the buttons and steals their taps
            (#197) — so it wraps to a full-width second row instead. */}
        <div className="order-last flex w-full min-w-0 flex-col gap-1.5 md:order-none md:w-auto md:flex-1">
          <div className="flex items-center gap-2">
            <p className="min-w-0 flex-1 truncate font-serif text-body font-semibold text-ink-100">
              {item.title}
            </p>
            <span className="shrink-0 font-mono text-label tracking-wide text-paper-fg-muted uppercase">
              {modeLabel(item.mode)}
            </span>
          </div>
          {playbackError ? (
            <p
              role="alert"
              data-testid="player-error"
              className="truncate text-label text-reading-highlight"
              title={playbackError}
            >
              {playbackError}
            </p>
          ) : null}
          <AudioPlayer variant="mini" controls={followAlong == null} />
        </div>

        <div className="ms-auto flex shrink-0 items-center gap-1 md:ms-0">
          {followAlong && (
            <button
              type="button"
              data-testid="mini-player-follow-along"
              aria-label={t("reader.followAlong")}
              onClick={followAlong}
              className="flex h-9 items-center gap-1.5 rounded-lg bg-ember-500 px-3.5 text-secondary font-semibold text-on-accent transition-colors hover:bg-ember-600 active:scale-[0.96]"
            >
              {t("reader.followAlong")}
            </button>
          )}
          <button
            type="button"
            data-testid="mini-player-close"
            aria-label={t("player.stopAndClose")}
            onClick={() => dispatch({ type: "stop" })}
            className="flex h-9 w-9 items-center justify-center rounded-lg text-paper-fg-muted transition-colors hover:bg-paper-fg/10 hover:text-paper-fg active:scale-[0.96]"
          >
            <CloseIcon className="h-4 w-4" />
          </button>
        </div>
      </div>
    </div>
  );
}

export default MiniPlayer;
