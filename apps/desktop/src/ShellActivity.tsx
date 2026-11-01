import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { presentStatus, type EngineStatus } from "./engineStatus";
import { runningJob, runningPct, type StudioJobs } from "./ActivityTray";
import { useI18n } from "./i18n";

export const RING = 2 * Math.PI * 8;

export function useEngineStatus(): EngineStatus | null {
  const [status, setStatus] = useState<EngineStatus | null>(null);

  useEffect(() => {
    let mounted = true;
    invoke<EngineStatus>("engine_status")
      .then((initial) => {
        if (mounted) setStatus((current) => current ?? initial);
      })
      .catch(console.error);
    const unlisten = listen<EngineStatus>("engine-status", (event) => {
      if (mounted) setStatus(event.payload);
    });
    return () => {
      mounted = false;
      unlisten.then((fn) => fn()).catch(console.error);
    };
  }, []);

  return status;
}

export function ShellActivityPill({
  active,
  engine,
  studio,
  onToggle,
  testId = "nav-jobs",
}: {
  active: boolean;
  testId?: string;
  engine: EngineStatus | null;
  studio: StudioJobs;
  onToggle: () => void;
}) {
  const { t } = useI18n();
  const ready = engine?.state === "ready";
  const badge = presentStatus(engine);
  const casting = runningJob(studio.jobs);
  const pct = runningPct(casting, studio.progress) ?? 0;
  const form = !ready ? "engine" : casting != null ? "casting" : "idle";

  return (
    <button
      type="button"
      data-testid={testId}
      data-activity={form}
      aria-expanded={active}
      aria-controls="activity-tray"
      title={t("nav.jobsTitle")}
      onClick={onToggle}
      className={`flex items-center gap-2.5 rounded-md py-1.5 transition-colors duration-200 active:scale-[0.98] ${
        form === "casting" ? "ps-2 pe-3.5" : "px-3.5"
      } ${active ? "bg-ink-800" : "bg-ink-850 hover:bg-ink-800"}`}
    >
      {form === "engine" && (
        <>
          <span className={`h-2 w-2 rounded-full ${badge.dotClass}`} />
          <span
            aria-live="polite"
            className="text-label font-medium text-ink-200"
          >
            {badge.label}
          </span>
        </>
      )}
      {form === "casting" && (
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
                strokeDashoffset={(RING * (1 - pct / 100)).toFixed(2)}
                strokeLinecap="round"
              />
            </svg>
          </span>
          <span className="text-label font-medium text-ink-200">
            {t("nav.casting")}
          </span>
        </>
      )}
      {form === "idle" && (
        <span className="text-label text-ink-400">{t("nav.activity")}</span>
      )}
    </button>
  );
}
