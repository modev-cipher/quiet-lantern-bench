import { useLayoutEffect, useRef, useState } from "react";

import BrandMark from "./BrandMark";
import {
  BooksIcon,
  HeadphonesIcon,
  HelpCircleIcon,
  SearchIcon,
  SettingsIcon,
  WaveIcon,
} from "./Icons";
import { RING } from "./ShellActivity";
import { isSettingsSection } from "./SettingsFrame";
import { presentStatus, type EngineStatus } from "./engineStatus";
import { runningJob, runningPct, type StudioJobs } from "./ActivityTray";
import type { View } from "./views";
import { useI18n } from "./i18n";

type RailKey = "library" | "audio" | "settings";

const FAR_JUMP_PX = 64;

function RailTip({ label, keys }: { label: string; keys?: string }) {
  return (
    <span
      aria-hidden="true"
      className="rail-tip shadow-overlay pointer-events-none absolute start-full top-1/2 z-50 ms-3 flex items-center gap-2 whitespace-nowrap rounded-md border border-ink-700 bg-ink-850 px-2.5 py-1.5 text-label font-medium text-ink-100"
    >
      {label}
      {keys != null && (
        <kbd
          dir="ltr"
          className="rounded border border-ink-700 bg-ink-900 px-1.5 font-mono text-[11px] text-ink-300"
        >
          {keys}
        </kbd>
      )}
    </span>
  );
}

function RailButton({
  testId,
  label,
  keys,
  current,
  expanded,
  controls,
  onClick,
  itemRef,
  children,
}: {
  testId: string;
  label: string;
  keys?: string;
  current?: boolean;
  expanded?: boolean;
  controls?: string;
  onClick: () => void;
  itemRef?: (el: HTMLButtonElement | null) => void;
  children: React.ReactNode;
}) {
  return (
    <button
      ref={itemRef}
      type="button"
      data-testid={testId}
      aria-label={label}
      aria-current={current ? "page" : undefined}
      aria-expanded={expanded}
      aria-controls={controls}
      onClick={onClick}
      className={`rail-item group relative flex h-10 w-10 items-center justify-center rounded-lg transition-[color,background-color,transform] duration-200 active:scale-[0.94] ${
        current || expanded
          ? "text-ember-400"
          : "text-ink-400 hover:bg-ink-850 hover:text-ink-100"
      }`}
    >
      {children}
      <RailTip label={label} keys={keys} />
    </button>
  );
}

function ActivityGlyph({
  engine,
  studio,
}: {
  engine: EngineStatus | null;
  studio: StudioJobs;
}) {
  const ready = engine?.state === "ready";
  const casting = runningJob(studio.jobs);
  const pct = runningPct(casting, studio.progress) ?? 0;
  if (!ready) {
    return (
      <span className="relative flex h-5 w-5 items-center justify-center">
        <span
          className={`rail-breathe h-2 w-2 rounded-full ${presentStatus(engine).dotClass}`}
        />
      </span>
    );
  }
  if (casting != null) {
    return (
      <svg
        viewBox="0 0 20 20"
        className="h-5 w-5 -rotate-90"
        data-testid="rail-casting-ring"
      >
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
          className="transition-[stroke-dashoffset] duration-700 ease-out"
        />
      </svg>
    );
  }
  return <WaveIcon className="h-5 w-5" />;
}

function ShellRail({
  view,
  version,
  onHome,
  onSelect,
  onOpenListening,
  trayOpen,
  onToggleTray,
  onOpenSearch,
  onOpenReportIssue,
  engine,
  studio,
}: {
  view: View;
  version: string;
  onHome: () => void;
  onSelect: (view: View) => void;
  onOpenListening: () => void;
  trayOpen: boolean;
  onToggleTray: () => void;
  onOpenSearch: () => void;
  onOpenReportIssue: () => void;
  engine: EngineStatus | null;
  studio: StudioJobs;
}) {
  const { t } = useI18n();
  const railRef = useRef<HTMLElement>(null);
  const itemsRef = useRef(new Map<RailKey, HTMLButtonElement>());
  const [marker, setMarker] = useState<{ top: number; jumps: number } | null>(
    null,
  );

  const active: RailKey | null =
    view === "library"
      ? "library"
      : view === "audio"
        ? "audio"
        : isSettingsSection(view)
          ? "settings"
          : null;

  useLayoutEffect(() => {
    const rail = railRef.current;
    if (rail == null) return;
    const measure = (activeChanged: boolean) => {
      const item = active == null ? null : itemsRef.current.get(active);
      if (item == null) {
        setMarker(null);
        return;
      }
      const top =
        item.getBoundingClientRect().top - rail.getBoundingClientRect().top;
      setMarker((current) => {
        if (current != null && current.top === top) return current;
        const far =
          activeChanged &&
          current != null &&
          Math.abs(current.top - top) > FAR_JUMP_PX;
        return { top, jumps: (current?.jumps ?? 0) + (far ? 1 : 0) };
      });
    };
    measure(true);
    const onResize = () => measure(false);
    const resizes = new ResizeObserver(onResize);
    resizes.observe(rail);
    window.addEventListener("resize", onResize);
    return () => {
      resizes.disconnect();
      window.removeEventListener("resize", onResize);
    };
  }, [active]);

  const register = (key: RailKey) => (el: HTMLButtonElement | null) => {
    if (el == null) itemsRef.current.delete(key);
    else itemsRef.current.set(key, el);
  };

  const ready = engine?.state === "ready";
  const casting = runningJob(studio.jobs);
  const pct = runningPct(casting, studio.progress);
  const activityLabel = !ready
    ? presentStatus(engine).label
    : casting != null
      ? pct != null
        ? t("nav.castingPct", { pct: Math.round(pct) })
        : t("nav.casting")
      : t("nav.activity");

  return (
    <nav
      ref={railRef}
      data-testid="shell-rail"
      aria-label={t("nav.destinations")}
      className="relative flex h-full w-15 shrink-0 flex-col items-center gap-1 border-e border-ink-800 bg-ink-900 pt-3 pb-3"
    >
      <span
        key={marker?.jumps ?? 0}
        aria-hidden="true"
        data-testid="rail-marker"
        data-jumps={marker?.jumps ?? 0}
        className={`rail-marker pointer-events-none absolute inset-x-2.5 top-0 h-10 rounded-lg bg-ember-500/12 ${
          marker == null ? "opacity-0" : "opacity-100"
        } ${marker != null && marker.jumps > 0 ? "rail-marker-arrive" : ""}`}
        style={{ transform: `translateY(${marker?.top ?? 0}px)` }}
      >
        <span className="absolute inset-y-2.5 -start-2.5 w-[3px] rounded-e-full bg-ember-500" />
      </span>
      <button
        type="button"
        data-testid="nav-home"
        aria-label={t("nav.home")}
        onClick={onHome}
        className="rail-item group relative mb-3 flex h-10 w-10 items-center justify-center rounded-lg transition-colors hover:bg-ink-850"
        title={
          version ? t("nav.appTitleVersion", { version }) : t("nav.appTitle")
        }
      >
        <BrandMark />
        <RailTip label={t("nav.home")} />
      </button>
      <RailButton
        testId="nav-search"
        label={t("nav.search")}
        keys="Ctrl K"
        onClick={onOpenSearch}
      >
        <SearchIcon className="h-5 w-5" />
      </RailButton>
      <span className="my-1.5 h-px w-6 bg-ink-800" />
      <RailButton
        testId="nav-library"
        label={t("nav.shelf")}
        current={active === "library"}
        itemRef={register("library")}
        onClick={() => onSelect("library")}
      >
        <BooksIcon className="h-5 w-5" />
      </RailButton>
      <RailButton
        testId="nav-listening"
        label={t("nav.listening")}
        current={active === "audio"}
        itemRef={register("audio")}
        onClick={onOpenListening}
      >
        <HeadphonesIcon className="h-5 w-5" />
      </RailButton>
      <RailButton
        testId="nav-jobs"
        label={activityLabel}
        expanded={trayOpen || view === "jobs"}
        controls="activity-tray"
        onClick={onToggleTray}
      >
        <span
          data-activity={
            !ready ? "engine" : casting != null ? "casting" : "idle"
          }
          className="flex items-center justify-center"
        >
          <ActivityGlyph engine={engine} studio={studio} />
        </span>
        <span className="sr-only" aria-live="polite">
          {activityLabel}
        </span>
      </RailButton>
      <span className="flex-1" />
      <RailButton
        testId="nav-report-issue"
        label={t("nav.reportIssueTitle")}
        onClick={onOpenReportIssue}
      >
        <HelpCircleIcon className="h-5 w-5" />
      </RailButton>
      <RailButton
        testId="nav-settings"
        label={t("nav.settings")}
        current={active === "settings"}
        itemRef={register("settings")}
        onClick={() => onSelect("settings")}
      >
        <SettingsIcon className="h-5 w-5" />
      </RailButton>
    </nav>
  );
}

export default ShellRail;
