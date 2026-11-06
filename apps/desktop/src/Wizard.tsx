import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";

import BrandMark from "./BrandMark";
import { AlertIcon, CheckIcon, ChevronIcon, StarIcon } from "./Icons";
import { subscribeHardwareProfile } from "./hardwareRefresh";
import type { ComputeEstimate } from "./jobFormModel";
import {
  downloadPct,
  downloadStatusLabel,
  fitBadge,
  formatBytes,
  licenseTag,
  type BadgeKind,
  type CatalogModelEntry,
  type FullCatalog,
  installedSet,
  type InstalledModel,
  type ModelDownloadProgress,
} from "./modelCatalogModel";
import {
  combinedDiskBlock,
  combinedDiskBytes,
  defaultChoices,
  diskLine,
  emptySelection,
  gpuDriverHint,
  gpuLine,
  pickBookPath,
  pickLlm,
  queuedModels,
  ramCpuLine,
  reasonLine,
  recommendModel,
  SKIP_COPY,
  taskModels,
  toggleTts,
  WIZARD_STEPS,
  WIZARD_TASKS,
  type WizardHardware,
  type WizardSelection,
  type WizardStep,
} from "./onboardingModel";
import { setLocalePreference } from "./i18n";
import { useI18n, type MessageKey } from "./i18n";
import { modelDisplayName } from "./i18n/modelNames";
import { requestImport } from "./views";
import { commandErrorText } from "./commandError";

type DownloadFinished =
  | { state: "installed"; modelId: string }
  | { state: "failed"; modelId: string; error: string };

const BADGE_STYLES: Record<BadgeKind, string> = {
  green: "bg-wash-green text-wash-green-ink",
  yellow: "bg-wash-yellow text-wash-yellow-ink",
  orange: "bg-ember-500/15 text-ember-300",
  red: "bg-wash-red text-wash-red-ink",
};

function RadioDot({ on }: { on: boolean }) {
  return (
    <svg viewBox="0 0 16 16" className="h-4 w-4 shrink-0" aria-hidden="true">
      <circle
        cx="8"
        cy="8"
        r="6.25"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
      />
      {on && <circle cx="8" cy="8" r="3" fill="currentColor" />}
    </svg>
  );
}

function CheckBox({ on }: { on: boolean }) {
  return (
    <svg viewBox="0 0 16 16" className="h-4 w-4 shrink-0" aria-hidden="true">
      <rect
        x="1.75"
        y="1.75"
        width="12.5"
        height="12.5"
        rx="3.5"
        fill={on ? "currentColor" : "none"}
        stroke="currentColor"
        strokeWidth="1.5"
      />
      {on && (
        <path
          d="M5 8.2l2 2 4-4.2"
          fill="none"
          stroke="var(--color-ink-950)"
          strokeWidth="1.6"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      )}
    </svg>
  );
}

const STEP_TITLES: Record<WizardStep, MessageKey> = {
  welcome: "wizard.step.welcome",
  voice: "wizard.step.voice",
  book: "wizard.step.book",
};

const STEP_FOOTNOTES: Record<WizardStep, MessageKey> = {
  welcome: "wizard.foot.welcome",
  voice: "wizard.foot.voice",
  book: "wizard.foot.book",
};

function StepRail({ current }: { current: WizardStep }) {
  const { t } = useI18n();
  const index = WIZARD_STEPS.indexOf(current);
  return (
    <ol
      aria-label={t("wizard.stepsLabel")}
      data-testid="wizard-steps"
      className="flex flex-row flex-wrap gap-x-5 gap-y-2 md:flex-col md:gap-5"
    >
      {WIZARD_STEPS.map((step, i) => {
        const state = i < index ? "done" : i === index ? "active" : "ahead";
        return (
          <li
            key={step}
            aria-current={i === index ? "step" : undefined}
            className="flex items-start gap-3"
          >
            <span
              className={`mt-0.5 flex h-6 w-6 shrink-0 items-center justify-center rounded-full font-mono text-label ${
                state === "done"
                  ? "bg-ink-800 text-ink-400"
                  : state === "active"
                    ? "bg-ember-500 font-semibold text-on-accent"
                    : "border border-ink-700 text-ink-500"
              }`}
            >
              {state === "done" ? <CheckIcon className="h-3.5 w-3.5" /> : i + 1}
            </span>
            <span className="flex min-w-0 flex-col gap-0.5">
              <span
                className={`text-secondary font-medium ${
                  state === "active"
                    ? "text-ink-50"
                    : state === "done"
                      ? "text-ink-400"
                      : "text-ink-500"
                }`}
              >
                {t(STEP_TITLES[step])}
              </span>
              <span
                className={`hidden text-label leading-snug text-ink-500 md:block ${
                  state === "active" ? "" : "md:hidden"
                }`}
              >
                {t(STEP_FOOTNOTES[step])}
              </span>
            </span>
          </li>
        );
      })}
    </ol>
  );
}

function ScanCard({
  hw,
  estimate,
}: {
  hw: WizardHardware | null;
  estimate: ComputeEstimate | null;
}) {
  const { t } = useI18n();
  if (!hw) {
    return (
      <p className="text-body text-ink-500" data-testid="wizard-scanning">
        {t("wizard.scanning")}
      </p>
    );
  }
  return (
    <div
      data-testid="wizard-scan-card"
      className="flex flex-col gap-4 rounded-xl border border-ink-800 bg-ink-950 p-5"
    >
      <div className="flex flex-col gap-2.5 text-body text-ink-300">
        <p data-testid="scan-gpu" className="flex items-baseline gap-3">
          <span className="w-10 shrink-0 text-label font-semibold tracking-[0.12em] text-ink-500 uppercase">
            GPU
          </span>
          <span className="font-mono">{gpuLine(hw)}</span>
        </p>
        {gpuDriverHint(hw) && (
          <p data-testid="scan-gpu-hint" className="ps-13 text-secondary text-ink-400">
            {gpuDriverHint(hw)}
          </p>
        )}
        <p data-testid="scan-ram" className="flex items-baseline gap-3">
          <span className="w-10 shrink-0 text-label font-semibold tracking-[0.12em] text-ink-500 uppercase">
            RAM
          </span>
          <span className="font-mono">{ramCpuLine(hw)}</span>
        </p>
        <p data-testid="scan-disk" className="flex items-baseline gap-3">
          <span className="w-10 shrink-0 text-label font-semibold tracking-[0.12em] text-ink-500 uppercase">
            {t("wizard.disk")}
          </span>
          <span className="font-mono">{diskLine(hw)}</span>
        </p>
      </div>
      {estimate && (
        <p
          data-testid="scan-score"
          className="flex items-baseline gap-3 border-t border-ink-800 pt-3.5"
        >
          <span className="text-secondary text-ink-400">
            {t("wizard.computeScore")}
          </span>
          <span className="font-serif text-title font-semibold text-ink-50">
            {estimate.computeScore}
          </span>
          <span className="font-mono text-secondary tracking-wide text-ember-400">
            — {estimate.band}
          </span>
        </p>
      )}
      <p className="text-secondary text-ink-500">{t("wizard.scanNote")}</p>
    </div>
  );
}

function ModelOption({
  model,
  models,
  hw,
  selected,
  recommended,
  fallbackReason,
  multi,
  onSelect,
}: {
  model: CatalogModelEntry;
  models: CatalogModelEntry[];
  hw: WizardHardware | null;
  selected: boolean;
  recommended: boolean;
  fallbackReason: boolean;
  multi: boolean;
  onSelect: (id: string) => void;
}) {
  const { t } = useI18n();
  const badge = fitBadge(model, hw);
  const nonCommercial = licenseTag(model);
  return (
    <button
      type="button"
      data-testid={`wizard-option-${model.id}`}
      aria-pressed={selected}
      onClick={() => onSelect(model.id)}
      className={`flex flex-col gap-1 rounded-xl border p-3 text-start transition-colors ${
        selected
          ? "border-ember-500/50 bg-ember-500/[0.07]"
          : "border-ink-800 bg-ink-900 hover:border-ink-700"
      }`}
    >
      <div className="flex items-center gap-2">
        <span className={selected ? "text-ember-300" : "text-ink-500"}>
          {multi ? <CheckBox on={selected} /> : <RadioDot on={selected} />}
        </span>
        <span className="text-body font-medium text-ink-100">
          {modelDisplayName(model, models)}
        </span>
        <span
          data-badge={badge.kind}
          title={badge.detail}
          className={`rounded px-1.5 py-0.5 font-mono text-label tracking-wide uppercase ${BADGE_STYLES[badge.kind]}`}
        >
          {badge.label}
        </span>
        {recommended && (
          <span
            data-testid={`wizard-recommended-${model.id}`}
            className="inline-flex items-center gap-1 rounded bg-ember-500/15 px-1.5 py-0.5 font-mono text-label tracking-wide text-ember-300 uppercase"
          >
            <StarIcon className="h-3 w-3 shrink-0" />
            {t("models.recommended")}
          </span>
        )}
        {nonCommercial && (
          <span
            title={model.license.spdx}
            className="inline-flex items-center gap-1 rounded bg-ink-800 px-1.5 py-0.5 font-mono text-label tracking-wide text-ink-400 uppercase"
          >
            <AlertIcon className="h-3 w-3 shrink-0" />
            {nonCommercial}
          </span>
        )}
        <span className="ms-auto font-mono text-secondary text-ink-500">
          {formatBytes(model.disk_bytes)}
        </span>
      </div>
      <p className="ps-6 text-secondary text-ink-500">{badge.detail}</p>
      {recommended && (
        <p
          data-testid={`wizard-reason-${model.id}`}
          className="ps-6 text-secondary text-ink-400"
        >
          {t("wizard.why", { reason: reasonLine(model, hw, fallbackReason) })}
        </p>
      )}
    </button>
  );
}

function ModelStep({
  task,
  models,
  hw,
  selection,
  onToggleTts,
  onPick,
}: {
  task: "tts" | "llm";
  models: CatalogModelEntry[];
  hw: WizardHardware | null;
  selection: WizardSelection;
  onToggleTts: (id: string) => void;
  onPick: (task: "llm", id: string | null) => void;
}) {
  const { t } = useI18n();
  const options = taskModels(models, task);
  const recommendation = recommendModel(models, task);
  const multi = task === "tts";
  const skipSelected = multi
    ? selection.tts.length === 0
    : selection[task] == null;
  return (
    <div className="flex flex-col gap-2" data-testid={`wizard-step-${task}`}>
      <p className="text-body text-ink-400">{t(WIZARD_TASKS[task])}</p>
      {options.map((model) => (
        <ModelOption
          key={model.id}
          model={model}
          models={models}
          hw={hw}
          multi={multi}
          selected={
            multi
              ? selection.tts.includes(model.id)
              : selection[task] === model.id
          }
          recommended={recommendation?.model.id === model.id}
          fallbackReason={recommendation?.fallback ?? false}
          onSelect={(id) =>
            multi
              ? onToggleTts(id)
              : onPick(task, selection[task] === id ? null : id)
          }
        />
      ))}
      <button
        type="button"
        data-testid={`wizard-skip-${task}`}
        aria-pressed={skipSelected}
        onClick={() => {
          if (multi) {
            selection.tts.forEach((id) => onToggleTts(id));
          } else {
            onPick(task, null);
          }
        }}
        className={`flex items-center gap-2 rounded-xl border border-dashed p-3 text-start text-secondary transition-colors ${
          skipSelected
            ? "border-ember-500/40 text-ember-200"
            : "border-ink-800 text-ink-500 hover:border-ink-700"
        }`}
      >
        <RadioDot on={skipSelected} /> {t(SKIP_COPY[task])}
      </button>
    </div>
  );
}

function DownloadQueue({
  queued,
  models,
  hw,
  progress,
  installed,
  failed,
}: {
  queued: CatalogModelEntry[];
  models: CatalogModelEntry[];
  hw: WizardHardware | null;
  progress: Record<string, ModelDownloadProgress>;
  installed: Set<string>;
  failed: Record<string, string>;
}) {
  const { t } = useI18n();
  if (queued.length === 0) {
    return (
      <p className="text-body text-ink-400" data-testid="wizard-queue-empty">
        {t("wizard.queueEmpty")}
      </p>
    );
  }
  const diskBlock = combinedDiskBlock(queued, hw);
  return (
    <div className="flex flex-col gap-2" data-testid="wizard-download-queue">
      <p className="text-body text-ink-400">
        {t("wizard.queueSummary", {
          count: queued.length,
          size: formatBytes(combinedDiskBytes(queued)),
        })}
      </p>
      {diskBlock && (
        <p
          role="alert"
          data-testid="wizard-disk-block"
          className="rounded-lg border border-wash-red-ink/20 bg-wash-red px-3 py-2 text-secondary text-wash-red-ink"
        >
          {diskBlock}
        </p>
      )}
      {queued.map((model) => {
        const p = progress[model.id];
        return (
          <div
            key={model.id}
            data-testid={`wizard-queue-${model.id}`}
            className="flex flex-col gap-1 rounded-xl border border-ink-800 bg-ink-900 p-3"
          >
            <div className="flex items-center justify-between">
              <span className="text-body text-ink-200">
                {modelDisplayName(model, models)}
              </span>
              <span className="font-mono text-secondary text-ink-500">
                {installed.has(model.id)
                  ? t("wizard.installed")
                  : failed[model.id]
                    ? t("wizard.failed")
                    : p
                      ? `${downloadStatusLabel(p)} · ${downloadPct(p)}%`
                      : t("wizard.waiting")}
              </span>
            </div>
            {p && (
              <div className="h-1 overflow-hidden rounded bg-ink-800">
                <div
                  className="h-full bg-ember-500 transition-all"
                  style={{ width: `${downloadPct(p)}%` }}
                />
              </div>
            )}
            {failed[model.id] && (
              <p className="text-secondary text-wash-red-ink">
                {failed[model.id]}
              </p>
            )}
          </div>
        );
      })}
    </div>
  );
}

const WELCOME_FACTS: MessageKey[] = [
  "wizard.welcome.offline",
  "wizard.welcome.local",
  "wizard.welcome.choice",
];

function WelcomeStep({
  hw,
  estimate,
}: {
  hw: WizardHardware | null;
  estimate: ComputeEstimate | null;
}) {
  const { t } = useI18n();
  return (
    <div
      data-testid="wizard-welcome"
      className="wizard-step flex flex-col gap-5"
    >
      <p className="text-body text-ink-300">{t("wizard.welcome.body")}</p>
      <ul className="flex flex-wrap gap-2">
        {WELCOME_FACTS.map((fact) => (
          <li
            key={fact}
            className="flex items-center gap-1.5 rounded-full border border-ink-800 bg-ink-850 px-3 py-1 text-secondary text-ink-200"
          >
            <CheckIcon className="h-3.5 w-3.5 text-ember-400" />
            {t(fact)}
          </li>
        ))}
      </ul>
      <ScanCard hw={hw} estimate={estimate} />
    </div>
  );
}

function VoiceStep({
  models,
  hw,
  selection,
  onToggleTts,
  onPickLlm,
}: {
  models: CatalogModelEntry[];
  hw: WizardHardware | null;
  selection: WizardSelection;
  onToggleTts: (id: string) => void;
  onPickLlm: (id: string | null) => void;
}) {
  const { t } = useI18n();
  return (
    <div data-testid="wizard-voice" className="wizard-step flex flex-col gap-5">
      <ModelStep
        task="tts"
        models={models}
        hw={hw}
        selection={selection}
        onToggleTts={onToggleTts}
        onPick={(_task, id) => onPickLlm(id)}
      />
      <details
        data-testid="wizard-teacher"
        className="group rounded-xl border border-ink-800 bg-ink-900"
      >
        <summary className="flex cursor-pointer list-none items-start justify-between gap-4 px-4 py-3">
          <span className="flex flex-col gap-0.5">
            <span className="text-body font-semibold text-ink-100">
              {t("wizard.teacher.title")}
            </span>
            <span className="text-secondary text-ink-400">
              {t("wizard.teacher.hint")}
            </span>
          </span>
          <ChevronIcon className="mt-1 h-4 w-4 shrink-0 rotate-90 text-ink-500 transition-transform duration-300 group-open:-rotate-90" />
        </summary>
        <div className="border-t border-ink-800 px-4 py-4">
          <ModelStep
            task="llm"
            models={models}
            hw={hw}
            selection={selection}
            onToggleTts={onToggleTts}
            onPick={(_task, id) => onPickLlm(id)}
          />
        </div>
      </details>
    </div>
  );
}

function ShelfDrop({ onPath }: { onPath: (path: string) => void }) {
  const { t } = useI18n();
  const [over, setOver] = useState(false);
  const [unsupported, setUnsupported] = useState(false);
  const [pickerFailed, setPickerFailed] = useState(false);
  const onPathRef = useRef(onPath);

  useEffect(() => {
    onPathRef.current = onPath;
  }, [onPath]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    let webview: ReturnType<typeof getCurrentWebview> | null = null;
    try {
      webview = getCurrentWebview();
    } catch {
      webview = null;
    }
    webview
      ?.onDragDropEvent((event) => {
        if (disposed) return;
        const payload = event.payload;
        if (payload.type === "enter" || payload.type === "over") {
          setOver(true);
        } else if (payload.type === "leave") {
          setOver(false);
        } else if (payload.type === "drop") {
          setOver(false);
          const path = pickBookPath(payload.paths);
          setUnsupported(path == null);
          setPickerFailed(false);
          if (path != null) onPathRef.current(path);
        }
      })
      .then((stop) => {
        if (disposed) stop();
        else unlisten = stop;
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
      unlisten = undefined;
    };
  }, []);

  const choose = async () => {
    let path: string | string[] | null;
    try {
      path = await open({
        multiple: false,
        filters: [{ name: "Books", extensions: ["pdf", "epub"] }],
      });
    } catch (error) {
      console.error(error);
      setPickerFailed(true);
      return;
    }
    setPickerFailed(false);
    if (typeof path === "string") {
      setUnsupported(false);
      onPath(path);
    }
  };

  return (
    <div className="flex flex-col gap-2">
      <button
        type="button"
        data-testid="wizard-drop"
        data-over={over ? "true" : "false"}
        onClick={() => void choose()}
        className="shelf wizard-drop"
      >
        <span aria-hidden="true" className="wizard-drop-ghost">
          <span className="wizard-drop-ghost-spine" />
        </span>
        <span className="wizard-drop-copy">
          <span className="text-body font-semibold text-ink-100">
            {t("wizard.book.drop")}
          </span>
          <span className="text-secondary text-ink-400">
            {t("wizard.book.or")} · {t("wizard.book.formats")}
          </span>
        </span>
        <span aria-hidden="true" className="wizard-drop-plank" />
      </button>
      {unsupported && (
        <p
          role="alert"
          data-testid="wizard-drop-unsupported"
          className="text-secondary text-wash-yellow-ink"
        >
          {t("wizard.book.unsupported")}
        </p>
      )}
      {pickerFailed && (
        <p
          role="alert"
          data-testid="wizard-picker-error"
          className="text-secondary text-wash-yellow-ink"
        >
          {t("wizard.book.pickerFailed")}
        </p>
      )}
    </div>
  );
}

function BookStep({
  onPath,
  queued,
  models,
  hw,
  progress,
  installed,
  failed,
}: {
  onPath: (path: string) => void;
  queued: CatalogModelEntry[];
  models: CatalogModelEntry[];
  hw: WizardHardware | null;
  progress: Record<string, ModelDownloadProgress>;
  installed: Set<string>;
  failed: Record<string, string>;
}) {
  const { t } = useI18n();
  const hasFailures = Object.keys(failed).length > 0;
  const offline = hasFailures && !navigator.onLine;
  return (
    <div data-testid="wizard-book" className="wizard-step flex flex-col gap-5">
      <ShelfDrop onPath={onPath} />
      {(queued.length > 0 || hasFailures) && (
        <section className="flex flex-col gap-2">
          <h3 className="text-label font-semibold tracking-[0.12em] text-ink-500 uppercase">
            {t("wizard.book.downloads")}
          </h3>
          {offline && (
            <p
              role="alert"
              data-testid="wizard-offline-banner"
              className="rounded-lg border border-wash-yellow-ink/20 bg-wash-yellow px-3 py-2 text-secondary text-wash-yellow-ink"
            >
              {t("wizard.offline")}
            </p>
          )}
          {hasFailures && !offline && (
            <p
              role="alert"
              data-testid="wizard-download-failures"
              className="rounded-lg border border-wash-yellow-ink/20 bg-wash-yellow px-3 py-2 text-secondary text-wash-yellow-ink"
            >
              {t("wizard.downloadsFailed")}
            </p>
          )}
          <DownloadQueue
            queued={queued}
            models={models}
            hw={hw}
            progress={progress}
            installed={installed}
            failed={failed}
          />
        </section>
      )}
    </div>
  );
}

function Wizard({ onDone }: { onDone: () => void }) {
  const { t, locale } = useI18n();
  const [step, setStep] = useState<WizardStep>("welcome");
  const headingRef = useRef<HTMLHeadingElement>(null);
  const previousStep = useRef(step);
  useEffect(() => {
    if (previousStep.current !== step) headingRef.current?.focus();
    previousStep.current = step;
  }, [step]);
  const [hw, setHw] = useState<WizardHardware | null>(null);
  const [estimate, setEstimate] = useState<ComputeEstimate | null>(null);
  const [catalog, setCatalog] = useState<FullCatalog | null>(null);
  const [selection, setSelection] = useState<WizardSelection>(emptySelection());
  const [installed, setInstalled] = useState<Set<string>>(new Set());
  const [progress, setProgress] = useState<
    Record<string, ModelDownloadProgress>
  >({});
  const [failed, setFailed] = useState<Record<string, string>>({});
  const [startedIds, setStartedIds] = useState<Set<string> | null>(null);

  useEffect(() => {
    invoke<ComputeEstimate>("get_compute_estimate")
      .then(setEstimate)
      .catch(console.error);
    invoke<FullCatalog>("get_catalog").then(setCatalog).catch(console.error);
    invoke<InstalledModel[]>("list_installed_models")
      .then((installs) => setInstalled(installedSet(installs)))
      .catch(console.error);
  }, []);

  useEffect(
    () =>
      subscribeHardwareProfile(
        () => invoke<WizardHardware>("get_hardware_profile"),
        setHw,
      ),
    [],
  );

  useEffect(() => {
    let active = true;
    const unlistenProgress = listen<ModelDownloadProgress>(
      "model-download-progress",
      (event) => {
        if (!active) return;
        setProgress((current) => ({
          ...current,
          [event.payload.modelId]: event.payload,
        }));
      },
    );
    const unlistenFinished = listen<DownloadFinished>(
      "model-download-finished",
      (event) => {
        if (!active) return;
        setProgress((current) => {
          const next = { ...current };
          delete next[event.payload.modelId];
          return next;
        });
        if (event.payload.state === "failed") {
          const { modelId, error } = event.payload;
          setFailed((current) => ({ ...current, [modelId]: error }));
        } else {
          const { modelId } = event.payload;
          setInstalled((current) => new Set(current).add(modelId));
        }
      },
    );
    return () => {
      active = false;
      unlistenProgress.then((fn) => fn()).catch(console.error);
      unlistenFinished.then((fn) => fn()).catch(console.error);
    };
  }, []);

  const models = useMemo(() => catalog?.models ?? [], [catalog]);
  const queued = useMemo(() => {
    const pending = new Set(
      queuedModels(models, selection, installed).map((model) => model.id),
    );
    return models.filter(
      (model) => pending.has(model.id) || startedIds?.has(model.id),
    );
  }, [models, selection, installed, startedIds]);

  const startDownloads = useCallback(() => {
    const queue = queuedModels(models, selection, installed);
    const pending = queue.filter((model) => !startedIds?.has(model.id));
    if (combinedDiskBlock(pending, hw) != null) return;
    setStartedIds(
      new Set([...(startedIds ?? []), ...pending.map((model) => model.id)]),
    );
    for (const choice of defaultChoices(models, selection)) {
      invoke("set_default_model", {
        task: choice.task,
        modelId: choice.modelId,
      }).catch(console.error);
    }
    for (const model of pending) {
      invoke("download_model", { modelId: model.id }).catch(
        (error: unknown) => {
          setFailed((current) => ({ ...current, [model.id]: commandErrorText(error) }));
        },
      );
    }
  }, [models, selection, installed, hw, startedIds]);

  const [finishError, setFinishError] = useState<string | null>(null);
  const [finishing, setFinishing] = useState(false);
  const finishingRef = useRef(false);

  const finish = useCallback(async () => {
    if (finishingRef.current) return;
    finishingRef.current = true;
    setFinishing(true);
    let completed = false;
    try {
      setFinishError(null);
      await invoke("set_onboarding_complete");
      completed = true;
    } catch (error) {
      console.error(error);
      setFinishError(commandErrorText(error));
    } finally {
      finishingRef.current = false;
      setFinishing(false);
    }
    if (completed) onDone();
  }, [onDone]);

  const next = useCallback(() => {
    const index = WIZARD_STEPS.indexOf(step);
    const target = WIZARD_STEPS[index + 1];
    if (!target) return;
    if (step === "voice") startDownloads();
    setStep(target);
  }, [step, startDownloads]);

  const importFirstBook = useCallback(
    (path: string) => {
      requestImport(path);
      void finish();
    },
    [finish],
  );

  return (
    <section
      data-testid="wizard"
      aria-busy={finishing}
      className="animate-modal-in my-auto grid w-full max-w-4xl gap-8 rounded-2xl border border-ink-700 bg-ink-900 p-8 shadow-overlay md:grid-cols-[230px_1fr]"
    >
      <div className="flex flex-col gap-6 md:border-e md:border-ink-800 md:pe-7">
        <div className="flex items-center gap-3">
          <BrandMark />
          <div className="leading-tight">
            <p className="font-serif text-heading font-semibold tracking-tight text-ink-50">
              txtCaster
            </p>
            <p className="text-label text-ink-500">{t("wizard.firstRun")}</p>
          </div>
        </div>
        <div
          className="flex flex-wrap items-center gap-2"
          role="group"
          aria-label={t("settings.language")}
        >
          {(["en", "ar"] as const).map((language) => (
            <button
              key={language}
              type="button"
              lang={language}
              dir={language === "ar" ? "rtl" : "ltr"}
              aria-pressed={locale === language}
              data-testid={`wizard-language-${language}`}
              onClick={() => setLocalePreference(language)}
              className="rounded-lg border border-ink-700 px-3 py-1.5 text-secondary text-ink-200 aria-pressed:bg-ink-800"
            >
              {t(
                language === "en"
                  ? "settings.languageEnglish"
                  : "settings.languageArabic",
              )}
            </button>
          ))}
        </div>
        <StepRail current={step} />
        <p className="mt-auto hidden text-label leading-relaxed text-ink-500 md:block">
          {t("wizard.intro")}
        </p>
      </div>
      <div className="flex min-w-0 flex-col gap-5">
        <h2
          ref={headingRef}
          tabIndex={-1}
          data-testid="wizard-heading"
          className="font-serif text-[26px] leading-tight font-semibold tracking-tight text-ink-50"
        >
          {step === "welcome"
            ? t("wizard.welcome.title")
            : t(STEP_TITLES[step])}
        </h2>

        {step === "welcome" && <WelcomeStep hw={hw} estimate={estimate} />}
        {step === "voice" && (
          <VoiceStep
            models={models}
            hw={hw}
            selection={selection}
            onToggleTts={(id) =>
              setSelection((current) => toggleTts(current, id))
            }
            onPickLlm={(id) => setSelection((current) => pickLlm(current, id))}
          />
        )}
        {step === "book" && startedIds != null && startedIds.size > 0 && (
          <p className="text-secondary text-ink-500">
            {t("wizard.backDownloads")}
          </p>
        )}
        {step === "book" && (
          <BookStep
            onPath={importFirstBook}
            queued={queued}
            models={models}
            hw={hw}
            progress={progress}
            installed={installed}
            failed={failed}
          />
        )}

        {finishError && (
          <p className="text-secondary text-wash-red-ink" role="alert">
            {finishError}
          </p>
        )}

        <div className="flex items-center justify-between pt-2">
          <button
            type="button"
            data-testid="wizard-skip-all"
            disabled={finishing}
            onClick={finish}
            className="rounded-lg px-3 py-1.5 text-body text-ink-500 transition-colors hover:text-ink-300"
          >
            {t("wizard.skip")}
          </button>
          {step !== "welcome" && (
            <button
              type="button"
              data-testid="wizard-back"
              disabled={finishing}
              onClick={() =>
                setStep(WIZARD_STEPS[WIZARD_STEPS.indexOf(step) - 1])
              }
              className="rounded-lg px-3 py-1.5 text-body text-ink-300"
            >
              {t("wizard.back")}
            </button>
          )}
          {step !== "book" ? (
            <button
              type="button"
              data-testid="wizard-next"
              disabled={finishing}
              onClick={next}
              className="rounded-lg bg-ink-50 px-4 py-1.5 text-body font-semibold text-ink-950 transition-all hover:bg-ink-150 active:scale-[0.98]"
            >
              {t("session.continue")}
            </button>
          ) : (
            <button
              type="button"
              data-testid="wizard-finish"
              disabled={finishing}
              onClick={finish}
              className="rounded-lg bg-ink-50 px-4 py-1.5 text-body font-semibold text-ink-950 transition-all hover:bg-ink-150 active:scale-[0.98]"
            >
              {t(finishing ? "wizard.finishing" : "wizard.finish")}
            </button>
          )}
        </div>
      </div>
    </section>
  );
}

export default Wizard;
