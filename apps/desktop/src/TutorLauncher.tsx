import { useSyncExternalStore } from "react";

import { MicIcon, MinimizeIcon } from "./Icons";
import { getActiveVoiceSnapshot, subscribeVoice } from "./voiceSessionStore";
import { useI18n } from "./i18n";

export const TUTOR_LAUNCHER_ID = "tutor-launcher";

function TutorLauncher({
  open,
  raised,
  onClick,
}: {
  open: boolean;
  raised: boolean;
  onClick: () => void;
}) {
  const { t } = useI18n();
  const { session } = useSyncExternalStore(
    subscribeVoice,
    getActiveVoiceSnapshot,
  );
  const live = session.status === "live" || session.status === "renewing";
  const label = open ? t("companion.fold") : t("nav.talkToTutor");
  return (
    <div
      className={`tutor-launcher-slot pointer-events-none absolute start-6 z-40 ${
        raised ? "bottom-32 md:bottom-28" : "bottom-6"
      }`}
    >
      <button
        type="button"
        id={TUTOR_LAUNCHER_ID}
        data-testid="tutor-launcher"
        data-live={live ? "true" : "false"}
        aria-label={label}
        title={label}
        aria-expanded={open}
        aria-controls="tutor-panel"
        onClick={onClick}
        className="tutor-launcher shadow-overlay pointer-events-auto relative flex h-12 w-12 items-center justify-center rounded-full border border-ink-800 bg-ink-900 text-ember-400 transition-[background-color,color,transform] duration-200 hover:bg-ink-850 hover:text-ember-300 active:scale-[0.96]"
      >
        {live && (
          <span
            aria-hidden="true"
            className="tutor-live-ring absolute inset-0 rounded-full"
          />
        )}
        {open ? (
          <MinimizeIcon className="h-5 w-5" />
        ) : (
          <MicIcon className="h-5 w-5" />
        )}
        {live && !open && (
          <span
            data-testid="tutor-launcher-live"
            className="absolute -top-0.5 -end-0.5 h-3 w-3 rounded-full border-2 border-ink-900 bg-ember-400"
          />
        )}
      </button>
    </div>
  );
}

export default TutorLauncher;
