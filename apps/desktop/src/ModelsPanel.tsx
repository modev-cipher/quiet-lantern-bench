import ModelRemovalDialog from "./ModelRemovalDialog";
import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import {
  AlertIcon,
  CheckIcon,
  CloseIcon,
  DownloadIcon,
  StarIcon,
} from "./Icons";
import { subscribeHardwareProfile } from "./hardwareRefresh";
import { resolveTaskModel } from "./jobWarningModel";
import { useModelManagement, type ModelAction } from "./useModelManagement";
import {
  canSetDefault,
  diskBlockMessage,
  downloadPct,
  downloadStatusLabel,
  fitBadge,
  fitReason,
  formatBytes,
  groupByTask,
  installedSet,
  licenseTag,
  recommendModel,
  setupStatus,
  TASK_PURPOSE,
  updateAvailable,
  updateLabel,
  type BadgeKind,
  type CatalogModelEntry,
  type HardwareFitProfile,
  type ModelDownloadProgress,
  type Recommendation,
} from "./modelCatalogModel";
import { useI18n } from "./i18n";
import { modelDisplayName } from "./i18n/modelNames";
import { GlideGroup } from "./GlideThumb";
import { downloadErrorView } from "./downloadErrorView";

type DownloadFinished =
  | { state: "installed"; modelId: string }
  | { state: "failed"; modelId: string; error: string };

const ONBOARDING_KEY = "txtcaster:models-onboarding-dismissed";

const BADGE_STYLES: Record<BadgeKind, string> = {
  green: "bg-wash-green text-wash-green-ink",
  yellow: "bg-wash-yellow text-wash-yellow-ink",
  orange: "bg-ember-500/15 text-ember-300",
  red: "bg-wash-red text-wash-red-ink",
};

function Badge({
  model,
  hw,
}: {
  model: CatalogModelEntry;
  hw: HardwareFitProfile | null;
}) {
  const badge = fitBadge(model, hw);
  return (
    <span
      data-testid={`model-badge-${model.id}`}
      data-badge={badge.kind}
      title={badge.detail}
      className={`shrink-0 rounded px-1.5 py-0.5 font-mono text-label tracking-wide uppercase ${BADGE_STYLES[badge.kind]}`}
    >
      {badge.label}
    </span>
  );
}

function SetupCards({
  models,
  installed,
  defaults,
  progress,
  onChoose,
}: {
  models: CatalogModelEntry[];
  installed: Set<string>;
  defaults: Record<string, string | null | undefined>;
  progress: Record<string, ModelDownloadProgress>;
  onChoose: (task: string) => void;
}) {
  const { t } = useI18n();
  const status = setupStatus(models, installed);
  if (status.length === 0) return null;
  return (
    <section
      data-testid="setup-status"
      aria-label={t("models.setupTitle")}
      className="grid grid-cols-[repeat(auto-fit,minmax(13rem,1fr))] gap-3"
    >
      {status.map((capability) => {
        const inTask = models.filter((model) => model.task === capability.task);
        const downloading = inTask
          .map((model) => progress[model.id])
          .find((entry) => entry != null);
        const chosen = resolveTaskModel(
          models,
          capability.task,
          defaults,
          installed,
        );
        return (
          <div
            key={capability.task}
            data-testid={`capability-${capability.task}`}
            data-ready={capability.ready}
            className="flex flex-col gap-2 rounded-xl border border-ink-800 bg-ink-900 p-4"
          >
            <div className="flex items-center justify-between gap-2">
              <p className="text-body font-semibold text-ink-100">
                {capability.label}
              </p>
              {capability.ready && !downloading && (
                <span className="flex h-5 w-5 items-center justify-center rounded-full bg-wash-green text-wash-green-ink">
                  <CheckIcon className="h-3 w-3" />
                </span>
              )}
            </div>
            {downloading ? (
              <>
                <p className="text-secondary text-ink-300">
                  {t("models.setup.downloading", {
                    pct: downloadPct(downloading),
                  })}
                </p>
                <div className="h-1 overflow-hidden rounded-full bg-ink-800">
                  <div
                    className="fill-glide h-full rounded-full bg-ember-500"
                    style={{ width: `${downloadPct(downloading)}%` }}
                  />
                </div>
              </>
            ) : capability.ready && chosen != null ? (
              <p className="truncate text-secondary text-ink-300">
                {t("models.setup.ready", {
                  name:
                    modelDisplayName(chosen, models).split(" · ").pop() ??
                    modelDisplayName(chosen, models),
                })}
              </p>
            ) : (
              <p className="text-secondary text-ink-400">
                {capability.task === "tts"
                  ? t("models.setup.needed")
                  : t("models.setup.optional")}
              </p>
            )}
            <button
              type="button"
              data-testid={`capability-choose-${capability.task}`}
              onClick={() => onChoose(capability.task)}
              className="mt-auto self-start rounded-lg border border-ink-700 px-3 py-1 text-secondary font-medium text-ink-200 transition-colors duration-200 hover:bg-ink-850 hover:text-ink-50"
            >
              {downloading
                ? t("models.setup.view")
                : capability.ready
                  ? t("models.setup.change")
                  : t("models.setup.choose")}
            </button>
          </div>
        );
      })}
    </section>
  );
}

function OnboardingHero({
  voice,
  llm,
  models,
  onDownload,
  onDismiss,
}: {
  voice: Recommendation | null;
  llm: Recommendation | null;
  models: CatalogModelEntry[];
  onDownload: (id: string) => void;
  onDismiss: () => void;
}) {
  const { t } = useI18n();
  return (
    <div
      data-testid="models-onboarding"
      className="animate-modal-in relative overflow-hidden rounded-xl border border-ember-500/25 bg-ink-900 p-8"
    >
      <button
        type="button"
        data-testid="dismiss-onboarding"
        onClick={onDismiss}
        aria-label={t("update.dismiss")}
        className="absolute top-4 end-4 rounded-lg p-1.5 text-ink-500 transition-colors hover:bg-ink-800 hover:text-ink-200 active:scale-[0.96]"
      >
        <CloseIcon className="h-4 w-4" />
      </button>

      <div className="relative flex max-w-2xl flex-col gap-5">
        <div className="flex flex-col gap-2">
          <span className="text-label font-semibold tracking-[0.18em] text-ember-400/90 uppercase">
            {t("models.welcome")}
          </span>
          <h2 className="font-serif text-display font-semibold tracking-tight text-ink-50">
            {t("models.heroTitle")}
          </h2>
          <p className="max-w-xl text-body leading-relaxed text-ink-300">
            {t("models.heroBody")}
          </p>
        </div>

        <div className="flex flex-wrap items-center gap-3">
          {voice ? (
            <button
              type="button"
              data-testid="onboarding-install-voice"
              onClick={() => onDownload(voice.model.id)}
              className="flex items-center gap-2 rounded-lg bg-ink-50 px-4 py-2.5 text-body font-semibold text-ink-950 transition-all hover:bg-ink-150 active:scale-[0.98]"
            >
              <DownloadIcon className="h-4 w-4" />
              {t("models.heroDownload", {
                name: modelDisplayName(voice.model, models),
                size: formatBytes(voice.model.disk_bytes),
              })}
            </button>
          ) : (
            <span className="text-body text-ink-400">
              {t("models.noVoiceModels")}
            </span>
          )}
          {llm && (
            <button
              type="button"
              data-testid="onboarding-install-llm"
              onClick={() => onDownload(llm.model.id)}
              className="rounded-xl border border-ink-700 bg-ink-800/60 px-4 py-2.5 text-body font-medium text-ink-200 transition-colors hover:bg-ink-800 active:scale-[0.97]"
            >
              {t("models.addTeacher")}
            </button>
          )}
        </div>

        {voice?.fallback && (
          <p
            data-testid="onboarding-voice-fallback"
            className="text-secondary text-ink-400"
          >
            {t("models.heroFallback")}
          </p>
        )}

        <p className="text-secondary text-ink-500">{t("models.heroBrowse")}</p>
      </div>
    </div>
  );
}

function CardSkeleton() {
  return (
    <div className="flex flex-col gap-3 rounded-xl border border-ink-800/70 bg-ink-900 p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="flex min-w-0 flex-1 flex-col gap-2">
          <div className="flex items-center gap-2">
            <div className="shimmer-bar h-4 w-40 rounded bg-ink-800/70" />
            <div className="shimmer-bar h-4 w-12 rounded bg-ink-800/70" />
          </div>
          <div className="shimmer-bar h-3 w-56 rounded bg-ink-800/50" />
        </div>
        <div className="flex flex-col items-end gap-2">
          <div className="shimmer-bar h-3 w-12 rounded bg-ink-800/50" />
          <div className="shimmer-bar h-7 w-20 rounded-lg bg-ink-800/70" />
        </div>
      </div>
    </div>
  );
}

function ModelsSkeleton() {
  return (
    <div data-testid="models-skeleton" className="flex flex-col gap-8">
      <div className="flex flex-wrap items-center gap-2.5 rounded-xl border border-ink-800/70 bg-ink-900 px-4 py-3">
        <div className="shimmer-bar h-3 w-12 rounded bg-ink-800/60" />
        {[0, 1, 2].map((chip) => (
          <div
            key={chip}
            className="shimmer-bar h-6 w-28 rounded-full bg-ink-800/60"
          />
        ))}
      </div>
      {[0, 1].map((group) => (
        <div key={group} className="flex flex-col gap-3">
          <div className="flex flex-col gap-1.5">
            <div className="shimmer-bar h-4 w-32 rounded bg-ink-800/70" />
            <div className="shimmer-bar h-3 w-64 rounded bg-ink-800/50" />
          </div>
          <div className="grid grid-cols-1 gap-3">
            {[0, 1, 2].map((card) => (
              <CardSkeleton key={card} />
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}

function languageNames(codes: readonly string[], locale: string): string {
  if (codes.length === 0) return "";
  let names: Intl.DisplayNames | null = null;
  try {
    names = new Intl.DisplayNames([locale], { type: "language" });
  } catch {
    names = null;
  }
  const label = (code: string) => {
    try {
      return names?.of(code) ?? code;
    } catch {
      return code;
    }
  };
  const shown = codes.slice(0, 3).map(label);
  return codes.length > 3
    ? `${shown.join(" · ")} · +${codes.length - 3}`
    : shown.join(" · ");
}

function ModelCard({
  model,
  models,
  hw,
  installed,
  isDefault,
  recommended,
  recommendedFallback,
  hasUpdate,
  updateText,
  progress,
  error,
  actionsDisabled,
  busy,
  onDownload,
  onCancel,
  onDelete,
  onSetDefault,
  layout = "row",
}: {
  layout?: "row" | "tile";
  model: CatalogModelEntry;
  models: CatalogModelEntry[];
  hw: HardwareFitProfile | null;
  installed: boolean;
  isDefault: boolean;
  recommended: boolean;
  recommendedFallback: boolean;
  hasUpdate: boolean;
  updateText: string;
  progress: ModelDownloadProgress | null;
  error: string | null;
  actionsDisabled: boolean;
  busy: boolean;
  onDownload: (id: string) => void;
  onCancel: (id: string) => void;
  onDelete: (id: string) => void;
  onSetDefault: (task: string, id: string | null) => void;
}) {
  const { t, locale } = useI18n();
  const tile = layout === "tile";
  const languages = languageNames(model.languages ?? [], locale);
  const diskBlock = diskBlockMessage(model, hw);
  const errorView = error == null ? null : downloadErrorView(error);
  const nonCommercial = licenseTag(model);
  const downloading = progress != null;

  return (
    <article
      data-testid={`model-row-${model.id}`}
      data-installed={installed}
      data-layout={layout}
      className={`animate-rise-in flex flex-col gap-3 rounded-xl border p-4 transition-colors duration-300 ${
        tile ? "h-full" : ""
      } ${
        installed
          ? "border-ink-700 bg-ink-900"
          : recommended && !recommendedFallback
            ? "border-ember-500/40 bg-ember-500/[0.05]"
            : "border-ink-800/70 bg-ink-900 hover:border-ink-700"
      }`}
    >
      {recommended && !installed && (
        <div
          data-testid={`model-recommended-${model.id}`}
          data-fallback={recommendedFallback}
          className={`flex items-center gap-1.5 text-label font-semibold tracking-wide ${
            recommendedFallback ? "text-ink-400" : "text-ember-300"
          }`}
        >
          <StarIcon className="h-3.5 w-3.5" />
          {recommendedFallback
            ? t("models.closestFit")
            : t("models.recommended")}
        </div>
      )}
      <div
        className={
          tile
            ? "flex flex-1 flex-col gap-3"
            : "flex items-start justify-between gap-3"
        }
      >
        <div className="flex min-w-0 flex-col gap-1">
          <div className="flex min-w-0 flex-wrap items-center gap-2">
            {installed && (
              <span className="flex h-4 w-4 shrink-0 items-center justify-center rounded-full bg-wash-green-ink/15 text-wash-green-ink">
                <CheckIcon className="h-3 w-3" />
              </span>
            )}
            <h3 className="truncate text-body font-semibold text-ink-100">
              {modelDisplayName(model, models)}
            </h3>
            <Badge model={model} hw={hw} />
            {nonCommercial && (
              <span
                data-testid={`model-license-${model.id}`}
                title={model.license.spdx}
                className="shrink-0 rounded bg-ink-800 px-1.5 py-0.5 font-mono text-label tracking-wide text-ink-400 uppercase"
              >
                {nonCommercial}
              </span>
            )}
            {isDefault && (
              <span
                data-testid={`model-default-${model.id}`}
                className="shrink-0 rounded bg-ember-500/15 px-1.5 py-0.5 font-mono text-label tracking-wide text-ember-300 uppercase"
              >
                {t("models.defaultBadge")}
              </span>
            )}
          </div>
          <div className="flex items-center gap-1.5 text-caption font-mono text-ink-400">
            <span className="truncate" title={model.id}>
              {model.display_name}
            </span>
            {model.quant && (
              <span className="shrink-0 rounded bg-ink-800/80 px-1 py-0.2 text-[10px] text-ink-500 uppercase">
                {model.quant}
              </span>
            )}
          </div>
          {tile && languages && (
            <p
              data-testid={`model-languages-${model.id}`}
              className="text-secondary text-ink-300"
            >
              {languages}
            </p>
          )}
          {fitReason(model, hw) !== "" && (
            <p className="text-secondary text-ink-500">
              {fitReason(model, hw)}
            </p>
          )}
        </div>
        <div
          className={
            tile
              ? "mt-auto flex items-center justify-between gap-2"
              : "flex shrink-0 flex-col items-end gap-2"
          }
        >
          <span className="font-mono text-secondary text-ink-500 tabular-nums">
            {formatBytes(model.disk_bytes)}
          </span>
          <div className="flex items-center gap-2">
            {installed ? (
              <>
                {hasUpdate && !downloading && (
                  <button
                    type="button"
                    disabled={actionsDisabled}
                    data-testid={`update-model-${model.id}`}
                    onClick={() => onDownload(model.id)}
                    className="rounded-lg bg-ink-50 px-2.5 py-1 disabled:cursor-not-allowed disabled:opacity-40 text-secondary font-semibold text-ink-950 transition-colors hover:bg-ink-150 active:scale-[0.98]"
                  >
                    {updateText}
                  </button>
                )}
                {hasUpdate && downloading && (
                  <button
                    type="button"
                    disabled={busy}
                    data-testid={`cancel-download-${model.id}`}
                    onClick={() => onCancel(model.id)}
                    className="rounded-lg px-2 py-1 disabled:cursor-not-allowed disabled:opacity-40 text-secondary text-ink-400 transition-colors hover:bg-ink-800 hover:text-ink-200"
                  >
                    {t("library.cancel")}
                  </button>
                )}
                {canSetDefault(model.task) && !isDefault && (
                  <button
                    type="button"
                    disabled={actionsDisabled}
                    data-testid={`set-default-${model.id}`}
                    onClick={() => onSetDefault(model.task, model.id)}
                    className="rounded-lg px-2 py-1 disabled:cursor-not-allowed disabled:opacity-40 text-secondary text-ink-400 transition-colors hover:bg-ink-800 hover:text-ink-200"
                  >
                    {t("models.setDefault")}
                  </button>
                )}
                <button
                  type="button"
                  disabled={actionsDisabled}
                  data-testid={`delete-model-${model.id}`}
                  onClick={() => onDelete(model.id)}
                  className="rounded-lg px-2 py-1 disabled:cursor-not-allowed disabled:opacity-40 text-secondary text-ink-400 transition-colors hover:bg-wash-red hover:text-wash-red-ink"
                >
                  {t("session.deleteConfirmLabel")}
                </button>
              </>
            ) : downloading ? (
              <button
                type="button"
                disabled={busy}
                data-testid={`cancel-download-${model.id}`}
                onClick={() => onCancel(model.id)}
                className="rounded-lg px-2 py-1 disabled:cursor-not-allowed disabled:opacity-40 text-secondary text-ink-400 transition-colors hover:bg-ink-800 hover:text-ink-200"
              >
                {t("library.cancel")}
              </button>
            ) : (
              <button
                type="button"
                data-testid={`download-model-${model.id}`}
                onClick={() => onDownload(model.id)}
                disabled={diskBlock != null || actionsDisabled}
                className={`flex items-center gap-1.5 rounded-lg px-2.5 py-1 text-secondary font-semibold transition-colors active:scale-[0.98] disabled:cursor-not-allowed disabled:bg-ink-800 disabled:text-ink-500 ${
                  recommended && !recommendedFallback
                    ? "bg-ink-50 text-ink-950 hover:bg-ink-150"
                    : "bg-ink-100 text-ink-950 hover:bg-ink-150"
                }`}
              >
                <DownloadIcon className="h-3.5 w-3.5" />
                {t("models.install")}
              </button>
            )}
          </div>
        </div>
      </div>

      {downloading && progress && (
        <div className="flex flex-col gap-1.5">
          <div className="flex items-center justify-between text-secondary">
            <span
              data-testid={`download-status-${model.id}`}
              className="text-ink-400"
            >
              {downloadStatusLabel(progress)}
            </span>
            <span className="font-mono text-ink-500 tabular-nums">
              {downloadPct(progress)}%
            </span>
          </div>
          <div className="shimmer-bar h-1.5 overflow-hidden rounded-full bg-ink-800">
            <div
              data-testid={`download-bar-${model.id}`}
              className="h-full rounded-full bg-ember-500 transition-[width] duration-300"
              style={{ width: `${downloadPct(progress)}%` }}
            />
          </div>
        </div>
      )}

      {diskBlock && !installed && !downloading && (
        <p
          data-testid={`disk-block-${model.id}`}
          className="rounded-lg border border-ember-500/25 bg-ember-500/[0.06] px-3 py-2 text-secondary text-ember-300"
        >
          {diskBlock}
        </p>
      )}

      {errorView && (
        <div
          role="alert"
          data-testid={`download-error-${model.id}`}
          data-kind={errorView.kind}
          className="flex flex-col gap-2 rounded-lg border border-ink-800 bg-ink-950/40 px-3 py-2.5 text-secondary"
        >
          <div className="flex items-start gap-2.5">
            <AlertIcon className="mt-0.5 h-4 w-4 shrink-0 text-wash-yellow-ink" />
            <p className="min-w-0 flex-1 text-ink-200">{errorView.message}</p>
            <button
              type="button"
              disabled={actionsDisabled}
              data-testid={`download-resume-${model.id}`}
              onClick={() => onDownload(model.id)}
              className="shrink-0 disabled:cursor-not-allowed disabled:opacity-40 rounded-md bg-ember-500 px-2.5 py-1 font-semibold text-on-accent transition-colors hover:bg-ember-600 active:scale-[0.98]"
            >
              {errorView.resumable
                ? t("models.error.resume")
                : t("models.error.retry")}
            </button>
          </div>
          <details className="ps-6.5 text-label text-ink-500">
            <summary className="w-fit cursor-pointer select-none hover:text-ink-300">
              {t("models.error.details")}
            </summary>
            <p className="mt-1 font-mono break-words text-ink-400">
              {errorView.detail}
            </p>
          </details>
        </div>
      )}
    </article>
  );
}

function ModelsPanel() {
  const { t, tx } = useI18n();
  const management = useModelManagement();
  const catalog = management.catalog.data;
  const installs = management.installs.data ?? [];
  const defaults = management.defaults.data ?? {};
  const [removal, setRemoval] = useState<CatalogModelEntry | null>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const mutationOpener = useRef<HTMLElement | null>(null);
  useEffect(() => {
    if (management.pending != null || mutationOpener.current == null) return;
    if (
      !mutationOpener.current.isConnected &&
      document.activeElement === document.body
    )
      headingRef.current?.focus();
    mutationOpener.current = null;
  }, [management.pending, installs, defaults]);
  const runAction = (action: ModelAction) => {
    mutationOpener.current =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    void management.run(action);
  };
  const refreshInstalls = management.refreshInstalls;
  const [hw, setHw] = useState<HardwareFitProfile | null>(null);
  const [progress, setProgress] = useState<
    Record<string, ModelDownloadProgress>
  >({});
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [tab, setTab] = useState<string>("tts");
  const [onboardingDismissed, setOnboardingDismissed] = useState(() => {
    try {
      return localStorage.getItem(ONBOARDING_KEY) === "1";
    } catch {
      return false;
    }
  });

  useEffect(
    () =>
      subscribeHardwareProfile(
        () => invoke<HardwareFitProfile>("get_hardware_profile"),
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
          setErrors((current) => ({ ...current, [modelId]: error }));
        } else {
          refreshInstalls();
        }
      },
    );
    return () => {
      active = false;
      unlistenProgress.then((fn) => fn()).catch(console.error);
      unlistenFinished.then((fn) => fn()).catch(console.error);
    };
  }, [refreshInstalls]);

  const download = useCallback(
    (modelId: string) => {
      if (!management.ready || management.pending != null) return;
      setErrors((current) => {
        const next = { ...current };
        delete next[modelId];
        return next;
      });
      invoke("download_model", { modelId }).catch((error: unknown) => {
        setErrors((current) => ({ ...current, [modelId]: String(error) }));
      });
    },
    [management.ready, management.pending],
  );

  const cancel = (modelId: string) => {
    runAction({ kind: "cancel", modelId });
  };
  const remove = (modelId: string) => {
    setRemoval(catalog?.models.find((model) => model.id === modelId) ?? null);
  };
  const setDefault = (task: string, modelId: string | null) => {
    runAction({ kind: "default", task, modelId });
  };

  const dismissOnboarding = useCallback(() => {
    try {
      localStorage.setItem(ONBOARDING_KEY, "1");
    } catch (error) {
      console.error("onboarding persistence failed", error);
    }
    setOnboardingDismissed(true);
  }, []);

  const installed = installedSet(installs);
  const models = catalog?.models ?? [];
  const groups = groupByTask(models, installed);
  const installedModels = models.filter((m) => installed.has(m.id));

  const recommendedByTask = new Map(
    groups.map((group) => [group.task, recommendModel(group.models)]),
  );

  const activeTab = groups.some((group) => group.task === tab)
    ? tab
    : (groups[0]?.task ?? "tts");
  const voiceGroup = groups.find((group) => group.task === "tts");
  const llmGroup = groups.find((group) => group.task === "llm");
  const recommendedVoice = voiceGroup
    ? (recommendModel(voiceGroup.models) ?? null)
    : null;
  const recommendedLlm = llmGroup
    ? (recommendModel(llmGroup.models) ?? null)
    : null;
  const showOnboarding =
    management.ready &&
    !onboardingDismissed &&
    catalog != null &&
    installedModels.length === 0;

  const reads = [
    {
      id: "catalog",
      resource: management.catalog,
      key: "models.read.catalogFailed",
    },
    {
      id: "installs",
      resource: management.installs,
      key: "models.read.installsFailed",
    },
    {
      id: "defaults",
      resource: management.defaults,
      key: "models.read.defaultsFailed",
    },
  ] as const;
  const actionFailure = management.actionError;
  const actionName = (modelId: string | null) => {
    const entry = models.find((model) => model.id === modelId);
    return entry
      ? modelDisplayName(entry, models)
      : (modelId ?? t("models.automatic"));
  };
  const canRetryAction =
    actionFailure != null &&
    (actionFailure.action.kind !== "cancel" ||
      progress[actionFailure.action.modelId] != null);

  return (
    <section data-testid="models-panel" className="flex w-full flex-col gap-8">
      <ModelRemovalDialog
        model={
          removal == null
            ? null
            : {
                modelId: removal.id,
                name: modelDisplayName(removal, catalog?.models ?? []),
                diskBytes:
                  installs.find((entry) => entry.modelId === removal.id)
                    ?.diskBytes ?? removal.disk_bytes,
              }
        }
        isDeleting={false}
        availability={
          management.ready
            ? undefined
            : reads.some(({ resource }) => resource.error != null)
              ? "unavailable"
              : "checking"
        }
        onClose={() => setRemoval(null)}
        onConfirm={() => {
          if (
            removal == null ||
            !management.ready ||
            management.pending != null
          )
            return;
          const modelId = removal.id;
          setRemoval(null);
          runAction({ kind: "remove", modelId });
        }}
      />
      <header className="flex flex-col gap-1.5">
        <h2
          ref={headingRef}
          tabIndex={-1}
          className="font-serif text-title font-semibold tracking-tight text-ink-50"
        >
          {t("nav.voicesAndModels")}
        </h2>
        <p className="max-w-xl text-body text-ink-400">{t("models.intro")}</p>
      </header>

      {reads.map(
        ({ id, resource, key }) =>
          resource.error != null && (
            <div
              key={id}
              role="alert"
              data-testid={`models-read-error-${id}`}
              className="flex flex-col gap-2 rounded-xl border border-wash-red-ink/20 bg-wash-red px-4 py-3 text-body text-wash-red-ink"
            >
              <p>{t(key)}</p>
              <details className="text-secondary">
                <summary className="cursor-pointer">
                  {t("models.error.details")}
                </summary>
                <bdi className="block break-words [overflow-wrap:anywhere]">
                  {resource.error}
                </bdi>
              </details>
              <button
                type="button"
                disabled={resource.loading}
                onClick={() => {
                  void resource.refresh();
                  headingRef.current?.focus();
                }}
                className="self-start rounded-lg border border-ink-700 px-3 py-1.5 text-secondary text-ink-200 disabled:opacity-40"
              >
                {resource.loading ? t("models.refreshing") : t("models.retry")}
              </button>
            </div>
          ),
      )}
      {management.pending && (
        <p role="status" className="text-body text-ink-400">
          {t("models.action.working")}
        </p>
      )}
      {actionFailure && (
        <div
          role="alert"
          data-testid="models-action-error"
          className="flex flex-col gap-2 rounded-xl border border-wash-red-ink/20 bg-wash-red px-4 py-3 text-body text-wash-red-ink"
        >
          <p>
            {tx(`models.action.${actionFailure.action.kind}Failed`, {
              name: actionName(actionFailure.action.modelId),
            })}
          </p>
          <details className="text-secondary">
            <summary className="cursor-pointer">
              {t("models.error.details")}
            </summary>
            <bdi className="block break-words [overflow-wrap:anywhere]">
              {actionFailure.detail}
            </bdi>
          </details>
          <div className="flex flex-wrap gap-2">
            {canRetryAction && (
              <button
                type="button"
                disabled={
                  management.pending != null ||
                  (!management.ready && actionFailure.action.kind !== "cancel")
                }
                onClick={() => {
                  void management.run(actionFailure.action);
                  headingRef.current?.focus();
                }}
                className="rounded-lg border border-ink-700 px-3 py-1.5 text-secondary text-ink-200 disabled:opacity-40"
              >
                {t("models.retry")}
              </button>
            )}
            <button
              type="button"
              onClick={() => {
                management.clearActionError();
                headingRef.current?.focus();
              }}
              className="rounded-lg px-3 py-1.5 text-secondary text-ink-400"
            >
              {t("models.dismiss")}
            </button>
          </div>
        </div>
      )}
      {catalog != null &&
        !management.ready &&
        !reads.some(({ resource }) => resource.error != null) && (
          <p role="status" className="text-body text-ink-400">
            {t("models.refreshing")}
          </p>
        )}

      {catalog == null ? (
        management.catalog.error == null ? (
          <ModelsSkeleton />
        ) : null
      ) : models.length === 0 ? (
        <p className="text-body text-ink-400">{t("models.emptyCatalog")}</p>
      ) : (
        <>
          {management.installs.data != null &&
            management.defaults.data != null && (
              <SetupCards
                models={models}
                installed={installed}
                defaults={defaults}
                progress={progress}
                onChoose={(task) => {
                  setTab(task);
                  document
                    .getElementById("models-catalog")
                    ?.scrollIntoView({ behavior: "smooth", block: "start" });
                }}
              />
            )}

          {showOnboarding && (
            <OnboardingHero
              voice={recommendedVoice}
              models={models}
              llm={recommendedLlm}
              onDownload={download}
              onDismiss={dismissOnboarding}
            />
          )}

          {installedModels.length > 0 && (
            <div className="flex flex-col gap-3">
              <h2 className="text-label font-semibold tracking-[0.16em] text-ink-500 uppercase">
                {t("models.installed")}
              </h2>
              <div
                data-testid="installed-models"
                className="grid grid-cols-1 gap-3"
              >
                {installedModels.map((model) => (
                  <ModelCard
                    key={model.id}
                    model={model}
                    models={models}
                    hw={hw}
                    installed
                    recommended={false}
                    recommendedFallback={false}
                    isDefault={
                      installed.has(model.id) &&
                      defaults[model.task] === model.id
                    }
                    hasUpdate={updateAvailable(model, installs)}
                    updateText={updateLabel(model, installs)}
                    progress={progress[model.id] ?? null}
                    error={errors[model.id] ?? null}
                    onDownload={download}
                    onCancel={cancel}
                    actionsDisabled={
                      !management.ready || management.pending != null
                    }
                    busy={management.pending != null}
                    onDelete={remove}
                    onSetDefault={setDefault}
                  />
                ))}
              </div>
            </div>
          )}

          <div id="models-catalog" className="flex flex-col gap-4 scroll-mt-6">
            <GlideGroup
              thumbClassName="border-b-2 border-ember-500"
              role="tablist"
              aria-label={t("models.catalog")}
              className="flex flex-wrap items-end gap-6 border-b border-ink-800"
            >
              {groups.map((group) => (
                <button
                  key={group.task}
                  type="button"
                  role="tab"
                  data-testid={`models-tab-${group.task}`}
                  aria-selected={group.task === activeTab}
                  onClick={() => setTab(group.task)}
                  className={`relative flex items-baseline gap-1.5 px-0.5 pb-2 text-body transition-colors duration-200 ${
                    group.task === activeTab
                      ? "font-semibold text-ink-50"
                      : "text-ink-400 hover:text-ink-100"
                  }`}
                >
                  {group.label}
                  <span className="font-mono text-label text-ink-500 tabular-nums">
                    {group.models.length}
                  </span>
                </button>
              ))}
            </GlideGroup>
            {groups
              .filter((group) => group.task === activeTab)
              .map((group) => (
                <div key={group.task} className="flex flex-col gap-3">
                  <div className="flex flex-col gap-0.5">
                    {TASK_PURPOSE[group.task] && (
                      <p className="text-secondary text-ink-500">
                        {t(TASK_PURPOSE[group.task])}
                      </p>
                    )}
                  </div>
                  <div
                    className={
                      group.task === "tts"
                        ? "grid grid-cols-[repeat(auto-fill,minmax(15rem,1fr))] gap-3"
                        : "grid grid-cols-1 gap-3"
                    }
                  >
                    {group.models.map((model) => {
                      const recommendation = recommendedByTask.get(group.task);
                      return (
                        <ModelCard
                          key={model.id}
                          layout={group.task === "tts" ? "tile" : "row"}
                          model={model}
                          models={models}
                          hw={hw}
                          installed={installed.has(model.id)}
                          recommended={recommendation?.model.id === model.id}
                          recommendedFallback={
                            recommendation?.model.id === model.id
                              ? recommendation.fallback
                              : false
                          }
                          isDefault={
                            installed.has(model.id) &&
                            defaults[model.task] === model.id
                          }
                          hasUpdate={
                            installed.has(model.id) &&
                            updateAvailable(model, installs)
                          }
                          updateText={updateLabel(model, installs)}
                          progress={progress[model.id] ?? null}
                          error={errors[model.id] ?? null}
                          onDownload={download}
                          onCancel={cancel}
                          actionsDisabled={
                            !management.ready || management.pending != null
                          }
                          busy={management.pending != null}
                          onDelete={remove}
                          onSetDefault={setDefault}
                        />
                      );
                    })}
                  </div>
                </div>
              ))}
          </div>
        </>
      )}
    </section>
  );
}

export default ModelsPanel;
