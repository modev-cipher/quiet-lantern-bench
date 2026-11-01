import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { engineRefusal } from "./engineSetupModel";
import { openEngineSetup } from "./engineSetupStore";

import {
  cloudArgs,
  DEFAULT_EFFORT,
  EFFORT_CHOICES,
  initialProviderSelection,
  LOOPBACK_FACT,
  parseConsentRequired,
  consentDataCategoryCopy,
  consentPhaseCopy,
  consentTitleCopy,
  type CastProviderOptions,
  type ConsentRequirement,
  type ProviderSelection,
  type ReasoningEffort,
} from "./castProviderModel";
import {
  getCloudProvidersForCategory,
  buildCloudLlmOptions,
  computeCategoryRenderPlan,
  type ModelOption,
  type ProviderSelectionMemory,
} from "./castModelSelectionModel";

import type { BookSummary } from "./libraryModel";
import {
  AlertIcon,
  CastIcon,
  ChevronIcon,
  CloseIcon,
  DownloadIcon,
  InfoIcon,
  MicIcon,
} from "./Icons";
import { useDialogFocus } from "./useDialogFocus";
import { goToView, requestBookTutor } from "./views";
import { clearCastDraft, getCastDraft, saveCastDraft } from "./castDraftStore";
import { CostFact } from "./CostFact";
import { VoiceSelector, type LocalEngineOption } from "./VoiceSelector";
import { phaseSummaryLines } from "./castPhaseSummary";
import { CustomDropdown } from "./CustomDropdown";
import {
  buildCloudAwareEtaRequest,
  chapterPages,
  formatEta,
  missingModels,
  pageRangeWords,
  readLastVoiceId,
  resolveDefaultVoiceId,
  selectedChapterWords,
  validatePageRange,
  voiceFit,
  writeLastVoiceId,
  type ChapterSummary,
  type CreateJobArgs,
  type EtaBreakdown,
  type HardwareProfile,
  type JobMode,
  type Scope,
  type VoiceProfileSummary,
  castStartErrorMessage,
} from "./jobFormModel";
import type { FullCatalog, InstalledModel } from "./modelCatalogModel";
import {
  completeReadySet,
  installedSet,
  projectorMissingSet,
  textReadySet,
  visionReadySet,
} from "./modelCatalogModel";
import {
  castFigureModelId,
  figureLine,
  figureModel,
  figureOverride,
  figureState,
} from "./figureModel";
import { languageAdvisory, normalizeLang } from "./languageAdvisoryModel";
import CloneVoiceModal, { type CloningEngineOption } from "./CloneVoiceModal";
import { cloningEngineIds, cloningInstalled } from "./cloneVoiceModel";
import {
  activeJobModels,
  badgeSeverity,
  etaWarningLine,
  jobModelsInUse,
  redInlineCopy,
  resolveJobModels,
  visionBadgeModel,
  worstBadge,
  worstModel,
  confirmToken,
  primaryAction,
  redConfirmCopy,
  startGate,
  usableInstalledModels,
  type StartIntent,
} from "./jobWarningModel";
import { useI18n } from "./i18n";
import { modelDisplayName, type ModelRole } from "./i18n/modelNames";
import Button from "./ui/Button";
import SegmentedSwitch from "./ui/SegmentedSwitch";

type CastStep = "scope" | "sound" | "review";

function SettingRow({
  label,
  testId,
  children,
}: {
  label: string;
  testId?: string;
  children: React.ReactNode;
}) {
  return (
    <div
      data-testid={testId}
      className="grid gap-x-6 gap-y-2 py-5 first:pt-1 sm:grid-cols-[7.5rem_minmax(0,1fr)]"
    >
      <div className="text-body font-medium text-ink-300 sm:pt-1.5">
        {label}
      </div>
      <div className="min-w-0">{children}</div>
    </div>
  );
}

function ChapterSkeleton() {
  return (
    <div className="flex flex-col gap-2 p-3">
      {[0, 1, 2, 3, 4].map((row) => (
        <div key={row} className="shimmer-bar h-9 rounded-lg bg-ink-800/60" />
      ))}
    </div>
  );
}

function JobModal({
  book,
  onClose: dismiss,
  onCreated,
  onPickModels,
}: {
  book: BookSummary;
  onClose: () => void;
  onCreated: () => void;
  onPickModels: () => void;
  onOpenProviders?: () => void;
}) {
  const { t, locale } = useI18n();
  const [draft] = useState(() => getCastDraft(book.id));
  const onClose = () => {
    clearCastDraft(book.id);
    dismiss();
    requestAnimationFrame(() => {
      if (
        document.activeElement === document.body &&
        !document.querySelector('[aria-modal="true"]')
      ) {
        document.getElementById("main-content")?.focus({ preventScroll: true });
      }
    });
  };
  const [readRevision, setReadRevision] = useState(0);
  const [setupReadError, setSetupReadError] = useState(false);
  const [providerReadError, setProviderReadError] = useState(false);
  const [providerReading, setProviderReading] = useState(true);
  const [draftAdjusted, setDraftAdjusted] = useState(false);
  const chaptersSeen = useRef(false);
  const [chapters, setChapters] = useState<ChapterSummary[] | null>(null);
  const [voices, setVoices] = useState<VoiceProfileSummary[]>([]);
  const [cloneOpen, setCloneOpen] = useState(false);
  const [hardware, setHardware] = useState<HardwareProfile | null>(null);
  const [catalog, setCatalog] = useState<FullCatalog | null>(null);
  const [defaults, setDefaults] = useState<Record<string, string>>({});
  const [installed, setInstalled] = useState<InstalledModel[]>([]);
  const [selectedLocalLlmId, setSelectedLocalLlmId] = useState<string | null>(
    draft?.selectedLocalLlmId ?? null,
  );
  const [warningsPending, setWarningsPending] = useState(3);
  // U5: the 🔴 confirm layer is open for this intent. The confirm token is
  // never held in state: "Run anyway" mints it for the current fit and passes
  // it into submit(), which spends it (F2 — no replay after a failed start,
  // a consent decline, or a fit change).
  const [confirmingRed, setConfirmingRed] = useState<StartIntent | null>(null);

  const [scopeKind, setScopeKind] = useState<"chapters" | "pages">(
    draft?.scopeKind ?? "chapters",
  );
  const [selected, setSelected] = useState<ReadonlySet<string>>(
    new Set(draft?.selected ?? []),
  );
  const [pageStart, setPageStart] = useState(draft?.pageStart ?? "1");
  const [pageEnd, setPageEnd] = useState(
    draft?.pageEnd ?? (book.pageCount != null ? String(book.pageCount) : ""),
  );
  const [mode, setMode] = useState<JobMode>(draft?.mode ?? "narrator");
  const [voiceEngineMode, setVoiceEngineMode] = useState<"local" | "cloud">(
    draft?.voiceEngineMode ?? "local",
  );
  const [pickedVoiceId, setPickedVoiceId] = useState<string | null>(
    draft?.pickedVoiceId ?? null,
  );
  const [lastVoiceId] = useState(readLastVoiceId);
  const [selectedCloudProviderId, setSelectedCloudProviderId] = useState<
    string | null
  >(draft?.selectedCloudProviderId ?? null);
  const [cloudVoiceMemory, setCloudVoiceMemory] = useState<
    Record<string, string>
  >(draft?.cloudVoiceMemory ?? {});
  const [explainFigures, setExplainFigures] = useState(
    draft?.explainFigures ?? true,
  );

  // Fresh and duplicated casts start Local/Local; only this setup draft resumes choices.
  const [providerSelection, setProviderSelection] = useState<ProviderSelection>(
    () => draft?.providerSelection ?? initialProviderSelection(),
  );
  const [providerOptions, setProviderOptions] =
    useState<CastProviderOptions | null>(null);
  const [stickyMemory, setStickyMemory] = useState<ProviderSelectionMemory>(
    draft?.stickyMemory ?? {
      tts: {},
      llm: {},
    },
  );
  const [selectedProviders, setSelectedProviders] = useState<{
    tts: string;
    llm: string;
  }>(
    draft?.selectedProviders ?? {
      tts: "local",
      llm: "local",
    },
  );
  const [fetchedScriptModels, setFetchedScriptModels] = useState<
    Record<string, string[]>
  >(draft?.fetchedScriptModels ?? {});
  const [fetchingScriptModels, setFetchingScriptModels] = useState(false);
  const [customScriptModel, setCustomScriptModel] = useState<string | null>(
    draft?.customScriptModel ?? null,
  );
  const [alignAdvisory, setAlignAdvisory] = useState(false);
  const [consentQueue, setConsentQueue] = useState<ConsentRequirement[] | null>(
    null,
  );
  const [consentIntent, setConsentIntent] = useState<StartIntent>("cast");
  // The consent sheet continues ONE confirmed start: it carries that token.
  const [consentToken, setConsentToken] = useState<string | null>(null);
  const [granting, setGranting] = useState(false);
  const consentRef = useRef<HTMLDivElement | null>(null);
  const [etaLabel, setEtaLabel] = useState("—");
  const [etaSeconds, setEtaSeconds] = useState<number | null>(null);
  const [estimating, setEstimating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const dialogRef = useRef<HTMLDivElement>(null);
  const confirmRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    let active = true;
    setWarningsPending(3);
    Promise.allSettled([
      invoke<ChapterSummary[]>("get_chapters", { bookId: book.id }),
      invoke<VoiceProfileSummary[]>("list_voice_profiles"),
      invoke<HardwareProfile>("get_hardware_profile"),
      invoke<FullCatalog>("get_catalog"),
      invoke<Record<string, string>>("get_default_models"),
      invoke<InstalledModel[]>("list_installed_models"),
    ]).then(
      ([
        chapterRead,
        voiceRead,
        hardwareRead,
        catalogRead,
        defaultRead,
        installRead,
      ]) => {
        if (!active) return;
        if (chapterRead.status === "fulfilled") {
          const result = chapterRead.value;
          setChapters(result);
          const first = !chaptersSeen.current;
          if (
            draft?.selected?.some(
              (id) => !result.some((chapter) => chapter.id === id),
            )
          )
            setDraftAdjusted(true);
          chaptersSeen.current = true;
          setSelected(
            (current) =>
              new Set(
                result
                  .filter((chapter) =>
                    first
                      ? draft?.selected == null ||
                        draft.selected.includes(chapter.id)
                      : current.has(chapter.id),
                  )
                  .map((chapter) => chapter.id),
              ),
          );
        }
        if (voiceRead.status === "fulfilled") setVoices(voiceRead.value);
        if (hardwareRead.status === "fulfilled")
          setHardware(hardwareRead.value);
        if (catalogRead.status === "fulfilled") setCatalog(catalogRead.value);
        if (defaultRead.status === "fulfilled") setDefaults(defaultRead.value);
        if (installRead.status === "fulfilled") setInstalled(installRead.value);
        setSetupReadError(
          [chapterRead, voiceRead, catalogRead, defaultRead, installRead].some(
            (read) => read.status === "rejected",
          ),
        );
        setWarningsPending(0);
      },
    );
    return () => {
      active = false;
    };
  }, [book.id, draft, readRevision]);

  useEffect(() => {
    let active = true;
    setProviderReading(true);
    invoke<CastProviderOptions>("cast_provider_options")
      .then((result) => {
        if (active) {
          setProviderOptions(result);
          setProviderReadError(false);
        }
      })
      .catch(() => active && setProviderReadError(true))
      .finally(() => active && setProviderReading(false));
    return () => {
      active = false;
    };
  }, [readRevision]);

  const pageRange = useMemo(
    () => validatePageRange(pageStart, pageEnd, book.pageCount),
    [pageStart, pageEnd, book.pageCount],
  );

  const words = useMemo(() => {
    if (!chapters) return 0;
    if (scopeKind === "chapters") {
      return selectedChapterWords(chapters, selected);
    }
    if (!pageRange.ok) return 0;
    return pageRangeWords(chapters, pageRange.start, pageRange.end);
  }, [chapters, scopeKind, selected, pageRange]);

  const scopeValid =
    scopeKind === "chapters" ? selected.size > 0 : pageRange.ok;

  const selectedPageCount = useMemo(() => {
    if (scopeKind === "chapters") {
      if (!chapters) return 0;
      return chapters
        .filter((c) => selected.has(c.id))
        .reduce((acc, c) => acc + Math.max(1, c.pageEnd - c.pageStart + 1), 0);
    }
    return pageRange.ok ? Math.max(0, pageRange.end - pageRange.start + 1) : 0;
  }, [scopeKind, chapters, selected, pageRange]);

  const isLongJob =
    (etaSeconds != null && etaSeconds > 900) || selectedPageCount > 40;

  const installedIds = useMemo(() => installedSet(installed), [installed]);
  const voiceReadyIds = useMemo(() => completeReadySet(installed), [installed]);
  const llmReadyIds = useMemo(() => textReadySet(installed), [installed]);
  const visionReadyIds = useMemo(() => visionReadySet(installed), [installed]);
  const projectorMissingIds = useMemo(
    () => projectorMissingSet(installed),
    [installed],
  );
  const engineLanguages = useCallback(
    (engine: string): string[] =>
      catalog?.models.find((model) => model.id === engine)?.languages ?? [],
    [catalog],
  );
  const voiceMismatch = useCallback(
    (voice: VoiceProfileSummary) =>
      languageAdvisory(
        book.language,
        voice.presetVoiceId,
        engineLanguages(voice.engine),
      ),
    [book.language, engineLanguages],
  );
  const usableVoices = useMemo(
    () => voices.filter((voice) => voiceReadyIds.has(voice.engine)),
    [voices, voiceReadyIds],
  );
  const pickedUsableVoiceId =
    pickedVoiceId != null &&
    usableVoices.some((voice) => voice.id === pickedVoiceId)
      ? pickedVoiceId
      : null;
  const voiceId =
    pickedUsableVoiceId ??
    resolveDefaultVoiceId(
      usableVoices,
      voiceReadyIds,
      lastVoiceId,
      (voice) => voiceMismatch(voice) == null,
      (engine) =>
        badgeSeverity(
          catalog?.models.find((model) => model.id === engine)?.badge,
        ),
      defaults.tts,
    );
  const selectedVoice = useMemo(() => {
    const ttsSel = providerSelection.tts;
    if (ttsSel.kind === "cloud") {
      const cVoiceId = ttsSel.voiceProfileId;
      return voices.find((v) => v.id === cVoiceId) ?? null;
    }
    return voices.find((v) => v.id === voiceId) ?? null;
  }, [providerSelection.tts, voices, voiceId]);
  const voiceEngine = selectedVoice?.engine ?? null;

  const localEngines = useMemo<LocalEngineOption[]>(() => {
    if (!catalog) return [];
    return catalog.models
      .filter((m) => m.task === "tts" && voiceReadyIds.has(m.id))
      .map((m) => ({
        id: m.id,
        name: modelDisplayName(m, catalog.models),
        badge: m.badge,
        nonCommercial: !m.license.commercial_ok,
      }));
  }, [catalog, voiceReadyIds]);

  const cloningEngines = useMemo<CloningEngineOption[]>(() => {
    if (!catalog) return [];
    return catalog.models
      .filter(
        (m) =>
          m.task === "tts" &&
          (m.capabilities ?? []).includes("voice_cloning") &&
          voiceReadyIds.has(m.id),
      )
      .map((m) => ({
        id: m.id,
        name: modelDisplayName(m, catalog.models),
        diskBytes: m.disk_bytes,
        badge: m.badge,
        nonCommercial: !m.license.commercial_ok,
      }));
  }, [catalog, voiceReadyIds]);

  const effectiveLanguage = useMemo(
    () => normalizeLang(book.language) ?? "en",
    [book.language],
  );
  const selectedMismatch = useMemo(() => {
    const ttsSel = providerSelection.tts;
    if (voiceEngineMode === "cloud") {
      if (ttsSel.kind !== "cloud") return null;
      const cVoiceId = ttsSel.voiceProfileId;
      const v = voices.find((voice) => voice.id === cVoiceId);
      if (v) return voiceMismatch(v);
      return null;
    }
    return selectedVoice != null ? voiceMismatch(selectedVoice) : null;
  }, [
    voiceEngineMode,
    providerSelection.tts,
    voices,
    selectedVoice,
    voiceMismatch,
  ]);

  const cloningReady = useMemo(
    () =>
      catalog != null &&
      cloningInstalled([...installedIds], cloningEngineIds(catalog.models)),
    [catalog, installedIds],
  );
  // Hermes R2 fold-in 8: a phase replaced by a cloud selection contributes
  // NO local missing-model demand and no local fit claim.
  const ttsIsCloud = providerSelection.tts.kind === "cloud";
  const scriptIsCloud = providerSelection.script.kind === "cloud";
  const fit = ttsIsCloud ? null : voiceFit(catalog, voiceEngine);
  const missing = missingModels({
    mode,
    voiceEngine,
    catalog,
    installedIds: llmReadyIds,
    ttsIsCloud,
    scriptIsCloud,
  });

  // F10: narrator + cloud scripting is a stage-time refusal
  // (scripting_cloud.rs "teacher mode only") — the form makes the combination
  // unrepresentable. Switching to Narrator with a cloud script selected
  // snaps scripting back to this device; the section's inline note says why.
  useEffect(() => {
    if (mode === "narrator" && providerSelection.script.kind === "cloud") {
      setProviderSelection((selection) => ({
        ...selection,
        script: { kind: "local" },
      }));
      setSelectedProviders((prev) => ({ ...prev, llm: "local" }));
    }
  }, [mode, providerSelection.script.kind]);

  useEffect(() => {
    const cloudRequest = buildCloudAwareEtaRequest({
      mode,
      words,
      voiceEngine,
      scriptIsCloud: providerSelection.script.kind === "cloud",
      ttsIsCloud: providerSelection.tts.kind === "cloud",
    });
    if (!scopeValid || words <= 0 || cloudRequest == null) {
      setEtaLabel("—");
      setEtaSeconds(null);
      setEstimating(false);
      return;
    }
    setEstimating(true);
    let active = true;
    const timer = setTimeout(() => {
      const request = cloudRequest;
      invoke<EtaBreakdown>("estimate_job_eta", { request })
        .then((breakdown) => {
          if (active) {
            setEtaLabel(formatEta(breakdown.totalS));
            setEtaSeconds(breakdown.totalS);
          }
        })
        .catch(() => {
          if (active) {
            setEtaLabel("—");
            setEtaSeconds(null);
          }
        })
        .finally(() => {
          if (active) setEstimating(false);
        });
    }, 300);
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [scopeValid, words, mode, hardware, voiceEngine, providerSelection]);

  const toggleChapter = useCallback((id: string) => {
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const buildScope = useCallback((): Scope => {
    if (scopeKind === "chapters") {
      return {
        kind: "chapters",
        chapter_ids: (chapters ?? [])
          .filter((chapter) => selected.has(chapter.id))
          .map((chapter) => chapter.id),
      };
    }
    if (!pageRange.ok) throw new Error("invalid page range");
    return {
      kind: "pages",
      page_start: pageRange.start,
      page_end: pageRange.end,
    };
  }, [scopeKind, chapters, selected, pageRange]);

  const scopeArg = useMemo(
    () =>
      scopeValid
        ? buildScope()
        : { kind: "pages" as const, page_start: 1, page_end: 1 },
    [scopeValid, buildScope],
  );

  const warningsReady = warningsPending <= 0;

  const catalogModels = useMemo(() => catalog?.models ?? [], [catalog]);
  const installedLlmModels = useMemo(
    () => usableInstalledModels(catalogModels, "llm", llmReadyIds),
    [catalogModels, llmReadyIds],
  );
  const baseResolvedModels = useMemo(
    () => resolveJobModels(catalogModels, defaults, llmReadyIds),
    [catalogModels, defaults, llmReadyIds],
  );
  const effectiveLocalLlmId =
    (installedLlmModels.some((model) => model.id === selectedLocalLlmId)
      ? selectedLocalLlmId
      : null) ??
    baseResolvedModels.llm?.id ??
    null;
  const llmCloudProviders = useMemo(
    () => getCloudProvidersForCategory(providerOptions, "llm"),
    [providerOptions],
  );
  const localLlmOptions: ModelOption[] = useMemo(
    () =>
      installedLlmModels.map((m) => ({
        id: m.id,
        name: modelDisplayName(m, catalogModels),
        sublabel: m.badge ? `badge: ${m.badge}` : undefined,
        badge: t("cast.engine.local"),
        providerId: "local",
        providerName: t("cast.onDeviceLocal"),
        isLocal: true,
        rawOption: m,
      })),
    [installedLlmModels, catalogModels, t],
  );
  const cloudLlmOptions: ModelOption[] = useMemo(
    () =>
      buildCloudLlmOptions(
        providerOptions?.scriptConnections ?? [],
        fetchedScriptModels,
      ),
    [providerOptions, fetchedScriptModels],
  );

  const fetchScriptModels = useCallback(
    async (connectionId: string) => {
      if (fetchingScriptModels) return;
      setFetchingScriptModels(true);
      try {
        const result = await invoke<{
          models: string[];
          suggestedDefault: string | null;
        }>("provider_fetch_models", { connectionId });
        setFetchedScriptModels((current) => ({
          ...current,
          [connectionId]: result.models,
        }));
        // F10: a fetch resolving after a flip to Narrator must not re-arm a
        // cloud selection the mode forbids.
        if (result.suggestedDefault != null && mode !== "narrator") {
          setStickyMemory((prev) => ({
            ...prev,
            llm: { ...prev.llm, [connectionId]: result.suggestedDefault! },
          }));
          setProviderSelection((prev) => ({
            ...prev,
            script: {
              ...(prev.script.kind === "cloud" ? prev.script : {}),
              kind: "cloud",
              connectionId,
              model: result.suggestedDefault!,
            },
          }));
          setCustomScriptModel(null);
        }
      } catch {
        // An explicit fetch that fails leaves manual entry exactly as it was.
      } finally {
        setFetchingScriptModels(false);
      }
    },
    [fetchingScriptModels, mode],
  );
  const scriptSelection = providerSelection.script;
  const cloudScriptConn =
    scriptSelection.kind === "cloud"
      ? (providerOptions?.scriptConnections.find(
          (c) => c.connectionId === scriptSelection.connectionId,
        ) ?? null)
      : null;
  const cloudScriptModelValue =
    providerSelection.script.kind === "cloud"
      ? `cloud:${providerSelection.script.connectionId}:${providerSelection.script.model}`
      : "";
  const cloudScriptEffort =
    providerSelection.script.kind === "cloud"
      ? (providerSelection.script.effort ?? DEFAULT_EFFORT)
      : DEFAULT_EFFORT;

  const [scriptWherePick, setScriptWherePick] = useState<
    "local" | "cloud" | null
  >(draft?.scriptWherePick ?? null);
  const scriptWhere: "local" | "cloud" =
    providerSelection.script.kind === "cloud"
      ? "cloud"
      : (scriptWherePick ??
        (warningsReady &&
        localLlmOptions.length === 0 &&
        llmCloudProviders.length > 0
          ? "cloud"
          : "local"));

  const llmPlan = useMemo(
    () =>
      computeCategoryRenderPlan({
        category: "llm",
        mode,
        localOptions: scriptWhere === "cloud" ? [] : localLlmOptions,
        cloudOptions: scriptWhere === "cloud" ? cloudLlmOptions : [],
        cloudProviders: scriptWhere === "cloud" ? llmCloudProviders : [],
        selectedProviderId:
          providerSelection.script.kind === "cloud"
            ? providerSelection.script.connectionId
            : selectedProviders.llm,
        selectedModelId:
          providerSelection.script.kind === "cloud"
            ? `cloud:${providerSelection.script.connectionId}:${providerSelection.script.model}`
            : (effectiveLocalLlmId ?? undefined),
        smartDefaultLocalId: baseResolvedModels.llm?.id ?? null,
        memory: stickyMemory.llm,
      }),
    [
      mode,
      scriptWhere,
      localLlmOptions,
      cloudLlmOptions,
      llmCloudProviders,
      providerSelection.script,
      selectedProviders.llm,
      effectiveLocalLlmId,
      baseResolvedModels.llm?.id,
      stickyMemory.llm,
    ],
  );

  const cloudVoices = useMemo(
    () => providerOptions?.ttsVoices ?? [],
    [providerOptions],
  );
  const ttsCloudProviders = useMemo(
    () => getCloudProvidersForCategory(providerOptions, "tts"),
    [providerOptions],
  );

  const effectiveCloudProviderId = useMemo(() => {
    if (
      selectedCloudProviderId &&
      ttsCloudProviders.some((p) => p.id === selectedCloudProviderId)
    ) {
      return selectedCloudProviderId;
    }
    return ttsCloudProviders[0]?.id ?? null;
  }, [selectedCloudProviderId, ttsCloudProviders]);

  const effectiveCloudVoiceId = useMemo(() => {
    if (!effectiveCloudProviderId) return null;
    const pVoices = cloudVoices.filter(
      (v) => v.connectionId === effectiveCloudProviderId,
    );
    const remembered = cloudVoiceMemory[effectiveCloudProviderId];
    if (remembered && pVoices.some((v) => v.voiceProfileId === remembered)) {
      return remembered;
    }
    return pVoices[0]?.voiceProfileId ?? null;
  }, [effectiveCloudProviderId, cloudVoices, cloudVoiceMemory]);

  useEffect(() => {
    if (
      draft != null &&
      voiceEngineMode === "cloud" &&
      providerSelection.tts.kind === "local" &&
      draft.providerSelection.tts.kind === "local" &&
      effectiveCloudVoiceId != null
    ) {
      setProviderSelection((current) => ({
        ...current,
        tts: { kind: "cloud", voiceProfileId: effectiveCloudVoiceId },
      }));
    }
  }, [
    draft,
    voiceEngineMode,
    providerSelection.tts.kind,
    effectiveCloudVoiceId,
  ]);

  const activeCloudVoiceId =
    providerSelection.tts.kind === "cloud"
      ? providerSelection.tts.voiceProfileId
      : null;
  const activeCloudVoice =
    activeCloudVoiceId != null
      ? (providerOptions?.ttsVoices.find(
          (v) => v.voiceProfileId === activeCloudVoiceId,
        ) ?? null)
      : null;

  useEffect(() => {
    let live = true;
    if (activeCloudVoiceId == null) {
      setAlignAdvisory(false);
      return;
    }
    invoke<boolean>("cast_align_advisory", {
      voiceProfileId: activeCloudVoiceId,
    })
      .then((value) => live && setAlignAdvisory(value))
      .catch(() => live && setAlignAdvisory(false));
    return () => {
      live = false;
    };
  }, [activeCloudVoiceId]);

  useEffect(() => {
    if (!draft || !warningsReady || setupReadError || providerOptions == null)
      return;
    const unavailableVoice =
      providerSelection.tts.kind === "cloud" &&
      !providerOptions.ttsVoices.some(
        (voice) =>
          voice.voiceProfileId ===
          (providerSelection.tts.kind === "cloud"
            ? providerSelection.tts.voiceProfileId
            : null),
      );
    const unavailableScript =
      providerSelection.script.kind === "cloud" &&
      !providerOptions.scriptConnections.some(
        (connection) =>
          connection.connectionId ===
          (providerSelection.script.kind === "cloud"
            ? providerSelection.script.connectionId
            : null),
      );
    if (unavailableVoice || unavailableScript) {
      setDraftAdjusted(true);
      setProviderSelection((current) => ({
        tts: unavailableVoice ? { kind: "local" } : current.tts,
        script: unavailableScript ? { kind: "local" } : current.script,
      }));
      if (unavailableVoice) setVoiceEngineMode("local");
      if (unavailableScript) setScriptWherePick("local");
    }
    if (
      (pickedVoiceId != null &&
        !usableVoices.some((voice) => voice.id === pickedVoiceId)) ||
      (selectedLocalLlmId != null &&
        !installedLlmModels.some((model) => model.id === selectedLocalLlmId))
    )
      setDraftAdjusted(true);
  }, [
    draft,
    warningsReady,
    setupReadError,
    providerOptions,
    providerSelection,
    pickedVoiceId,
    selectedLocalLlmId,
    usableVoices,
    installedLlmModels,
  ]);

  const handleSelectLocalVoice = useCallback((val: string) => {
    setPickedVoiceId(val);
    writeLastVoiceId(val);
    setProviderSelection((prev) => ({ ...prev, tts: { kind: "local" } }));
  }, []);

  const handleSelectEngine = useCallback(
    (engineId: string) => {
      const engineVoices = usableVoices.filter((v) => v.engine === engineId);
      const nextVoiceId = resolveDefaultVoiceId(
        engineVoices.length > 0 ? engineVoices : usableVoices,
        voiceReadyIds,
        null,
        (voice) => voiceMismatch(voice) == null,
        (engine) =>
          badgeSeverity(
            catalog?.models.find((model) => model.id === engine)?.badge,
          ),
        engineId,
      );
      if (nextVoiceId != null) {
        setPickedVoiceId(nextVoiceId);
        writeLastVoiceId(nextVoiceId);
        setProviderSelection((prev) => ({ ...prev, tts: { kind: "local" } }));
      }
    },
    [usableVoices, voiceReadyIds, voiceMismatch, catalog],
  );

  const handleSelectCloudProvider = useCallback(
    (newProvId: string) => {
      setSelectedCloudProviderId(newProvId);
      const pVoices = cloudVoices.filter((v) => v.connectionId === newProvId);
      const remembered = cloudVoiceMemory[newProvId];
      const targetVoice =
        remembered && pVoices.some((v) => v.voiceProfileId === remembered)
          ? remembered
          : (pVoices[0]?.voiceProfileId ?? null);
      if (targetVoice) {
        setCloudVoiceMemory((prev) => ({ ...prev, [newProvId]: targetVoice }));
      }
      if (voiceEngineMode === "cloud" && targetVoice) {
        setProviderSelection((prev) => ({
          ...prev,
          tts: { kind: "cloud", voiceProfileId: targetVoice },
        }));
      }
    },
    [cloudVoices, cloudVoiceMemory, voiceEngineMode],
  );

  const handleSelectCloudVoice = useCallback(
    (newVoiceProfileId: string) => {
      if (effectiveCloudProviderId) {
        setCloudVoiceMemory((prev) => ({
          ...prev,
          [effectiveCloudProviderId]: newVoiceProfileId,
        }));
      }
      setProviderSelection((prev) => ({
        ...prev,
        tts: { kind: "cloud", voiceProfileId: newVoiceProfileId },
      }));
    },
    [effectiveCloudProviderId],
  );

  const handleSwitchVoiceEngine = useCallback(
    (newMode: "local" | "cloud") => {
      setVoiceEngineMode(newMode);
      if (newMode === "local") {
        setProviderSelection((prev) => ({ ...prev, tts: { kind: "local" } }));
      } else {
        const provId = effectiveCloudProviderId;
        const targetVoice = effectiveCloudVoiceId;
        if (provId && targetVoice) {
          setProviderSelection((prev) => ({
            ...prev,
            tts: { kind: "cloud", voiceProfileId: targetVoice },
          }));
          if (!cloudVoiceMemory[provId]) {
            setCloudVoiceMemory((prev) => ({ ...prev, [provId]: targetVoice }));
          }
        }
      }
    },
    [effectiveCloudProviderId, effectiveCloudVoiceId, cloudVoiceMemory],
  );

  const handleSelectLlmProvider = useCallback(
    (newProvId: string) => {
      setSelectedProviders((prev) => ({ ...prev, llm: newProvId }));
      if (newProvId === "local") {
        setCustomScriptModel(null);
        setProviderSelection((prev) => ({
          ...prev,
          script: { kind: "local" },
        }));
        const remembered = stickyMemory.llm?.["local"];
        if (remembered && installedLlmModels.some((m) => m.id === remembered)) {
          setSelectedLocalLlmId(remembered);
        }
      } else {
        const conn = providerOptions?.scriptConnections.find(
          (c) => c.connectionId === newProvId,
        );
        if (conn) {
          const remembered = stickyMemory.llm?.[newProvId];
          const targetModel = remembered ?? conn.defaultModel;
          setProviderSelection((prev) => ({
            ...prev,
            script: {
              kind: "cloud",
              connectionId: conn.connectionId,
              model: targetModel,
              effort:
                prev.script.kind === "cloud" ? prev.script.effort : undefined,
            },
          }));
          setCustomScriptModel(null);
        }
      }
    },
    [installedLlmModels, providerOptions, stickyMemory.llm],
  );

  const handleSelectLlmModel = useCallback(
    (val: string) => {
      if (val === "__custom__") {
        if (providerSelection.script.kind === "cloud") {
          setCustomScriptModel(providerSelection.script.model);
        }
        return;
      }
      if (val.startsWith("cloud:")) {
        const parts = val.split(":");
        const connId = parts[1];
        const conn = providerOptions?.scriptConnections.find(
          (c) => c.connectionId === connId,
        );
        if (conn) {
          const model = parts[2] ?? conn.defaultModel;
          setStickyMemory((prev) => ({
            ...prev,
            llm: { ...prev.llm, [connId]: model },
          }));
          setProviderSelection((prev) => ({
            ...prev,
            script: {
              kind: "cloud",
              connectionId: conn.connectionId,
              model,
              effort:
                prev.script.kind === "cloud" &&
                prev.script.connectionId === conn.connectionId
                  ? prev.script.effort
                  : undefined,
            },
          }));
          setCustomScriptModel(null);
        }
      } else {
        setStickyMemory((prev) => ({
          ...prev,
          llm: { ...prev.llm, local: val },
        }));
        setSelectedLocalLlmId(val);
        setProviderSelection((prev) => ({
          ...prev,
          script: { kind: "local" },
        }));
        setCustomScriptModel(null);
      }
    },
    [providerOptions, providerSelection.script],
  );

  const handleCustomScriptModel = useCallback((model: string) => {
    setCustomScriptModel(model);
    setProviderSelection((prev) =>
      prev.script.kind === "cloud"
        ? { ...prev, script: { ...prev.script, model } }
        : prev,
    );
  }, []);

  // A single cloud scripting model with no local alternative is the only
  // possibility: adopt it so effort, cost, and submission see the effective
  // selection instead of a local choice that does not exist.
  useEffect(() => {
    if (
      providerSelection.script.kind === "local" &&
      llmPlan.mode === "fixed-auto" &&
      llmPlan.selectedModelId?.startsWith("cloud:")
    ) {
      handleSelectLlmModel(llmPlan.selectedModelId);
    }
  }, [
    providerSelection.script.kind,
    llmPlan.mode,
    llmPlan.selectedModelId,
    handleSelectLlmModel,
  ]);

  const firstCloudScriptId = llmCloudProviders[0]?.id ?? null;
  useEffect(() => {
    if (
      mode === "teacher" &&
      scriptWhere === "cloud" &&
      providerSelection.script.kind === "local" &&
      firstCloudScriptId != null
    ) {
      handleSelectLlmProvider(firstCloudScriptId);
    }
  }, [
    mode,
    scriptWhere,
    providerSelection.script.kind,
    firstCloudScriptId,
    handleSelectLlmProvider,
  ]);

  const pickScriptWhere = (where: "local" | "cloud") => {
    setScriptWherePick(where);
    if (where === "local") handleSelectLlmProvider("local");
  };
  const pickMode = (next: JobMode) => {
    setMode(next);
    if (next === "narrator") setScriptWherePick("local");
  };
  const resolvedModels = useMemo(() => {
    if (effectiveLocalLlmId) {
      const match = catalogModels.find((m) => m.id === effectiveLocalLlmId);
      if (match) return { ...baseResolvedModels, llm: match };
    }
    return baseResolvedModels;
  }, [baseResolvedModels, effectiveLocalLlmId, catalogModels]);
  const jobModels = useMemo(
    () => jobModelsInUse(resolvedModels),
    [resolvedModels],
  );
  const castFigures = useMemo(() => {
    const script = providerSelection.script;
    const cloudScript =
      script.kind === "cloud"
        ? {
            connectionName:
              providerOptions?.scriptConnections.find(
                (c) => c.connectionId === script.connectionId,
              )?.name ?? script.connectionId,
            model: script.model,
            readsImages: ((): boolean | "unknown" => {
              const support = providerOptions?.scriptConnections.find(
                (connection) => connection.connectionId === script.connectionId,
              )?.modelVision?.[script.model];
              return support === "supported"
                ? true
                : support === "unsupported"
                  ? false
                  : "unknown";
            })(),
          }
        : null;
    return figureState({
      explainFigures,
      models: catalogModels,
      castModelId: castFigureModelId({
        models: catalogModels,
        installedIds,
        llmOverrideId: mode === "teacher" ? effectiveLocalLlmId : null,
      }),
      visionReadyIds,
      projectorMissingIds,
      cloudScript,
    });
  }, [
    providerSelection.script,
    providerOptions,
    explainFigures,
    catalogModels,
    installedIds,
    mode,
    effectiveLocalLlmId,
    visionReadyIds,
    projectorMissingIds,
  ]);
  const visionModel = useMemo(
    () => visionBadgeModel(catalogModels, figureModel(castFigures)?.id ?? null),
    [catalogModels, castFigures],
  );
  const cloudFilteredJobModels = activeJobModels({
    models: jobModels,
    mode,
    ttsIsCloud,
    scriptIsCloud,
  });
  const badgeModels = useMemo(() => {
    if (!visionModel) return cloudFilteredJobModels;
    const existingIndex = cloudFilteredJobModels.findIndex(
      (m) => m.id === visionModel.id,
    );
    if (existingIndex < 0) return [...cloudFilteredJobModels, visionModel];
    const existing = cloudFilteredJobModels[existingIndex]!;
    if (badgeSeverity(visionModel.badge) > badgeSeverity(existing.badge)) {
      const copy = [...cloudFilteredJobModels];
      copy[existingIndex] = visionModel;
      return copy;
    }
    return cloudFilteredJobModels;
  }, [cloudFilteredJobModels, visionModel]);
  const fitBadgeKind = warningsReady ? worstBadge(badgeModels) : null;
  const redModel = fitBadgeKind === "red" ? worstModel(badgeModels) : null;

  const isVisionCulprit =
    redModel != null &&
    visionModel != null &&
    redModel.id === visionModel.id &&
    visionModel.badge === "red" &&
    (mode === "narrator" ||
      scriptIsCloud ||
      cloudFilteredJobModels.every(
        (m) => m.id !== redModel.id || m.badge !== "red",
      ));
  const culpritRole: ModelRole | undefined = isVisionCulprit
    ? "figures"
    : undefined;

  const [verify, setVerify] = useState(draft?.verify ?? false);
  const cloudChoicesUnavailable =
    (providerReading || providerReadError) &&
    (voiceEngineMode === "cloud" ||
      (mode === "teacher" && scriptWhere === "cloud"));

  const phaseSummary = useMemo(
    () =>
      phaseSummaryLines({
        mode,
        selection: providerSelection,
        options: providerOptions,
        resolved: resolvedModels,
        figureState: castFigures,
        catalogModels,
        voiceName: selectedVoice?.name ?? null,
        voiceEngine,
      }),
    [
      mode,
      providerSelection,
      providerOptions,
      resolvedModels,
      castFigures,
      catalogModels,
      selectedVoice,
      voiceEngine,
    ],
  );

  // F8 D2 (was F7's Study hand-off): the tutor card carries THIS book to the
  // reader surface — the voice panel sits beside the visible book, with the
  // Study card only as the no-render fallback (tutorEntryRoute decides).
  const openStudy = () => {
    onClose();
    requestBookTutor(book.id);
  };

  // The start gate lives here, not on the buttons: every path that can start a
  // job (buttons, keyboard, consent re-submit) goes through this check.
  const submit = async (intent: StartIntent, token: string | null = null) => {
    if (
      !scopeValid ||
      submitting ||
      setupReadError ||
      cloudChoicesUnavailable ||
      (voiceEngineMode === "cloud" &&
        (providerSelection.tts.kind !== "cloud" || activeCloudVoice == null)) ||
      (providerSelection.script.kind === "cloud" && cloudScriptConn == null)
    )
      return;
    const submissionScript = providerSelection.script;
    if (
      explainFigures &&
      submissionScript.kind === "cloud" &&
      providerOptions?.scriptConnections.find(
        (connection) =>
          connection.connectionId === submissionScript.connectionId,
      )?.modelVision?.[submissionScript.model] === "unsupported"
    ) {
      setError(t("providers.figuresUnavailable"));
      return;
    }
    if (voiceId == null && providerSelection.tts.kind !== "cloud") return;
    const gate = startGate({
      badge: fitBadgeKind,
      ready: warningsReady,
      models: badgeModels,
      culprit: redModel,
      token,
    });
    if (gate.kind === "not-ready") return;
    if (gate.kind === "confirm-required") {
      setConfirmingRed(intent);
      return;
    }
    const overnight = intent === "overnight";
    setSubmitting(true);
    setError(null);
    setConfirmingRed(null);
    const args: Omit<CreateJobArgs, "voiceProfileId"> & {
      voiceProfileId?: string;
      scriptCloud?: {
        connectionId: string;
        model: string;
        effort?: "low" | "medium" | "high";
      };
      ttsCloud?: { voiceProfileId: string };
    } = {
      bookId: book.id,
      scope: buildScope(),
      mode,
      outputFormat: "mp3",
      voiceProfileId: voiceId ?? undefined,
      modelOverridesJson: JSON.stringify({
        ...figureOverride(explainFigures),
        ...(mode === "teacher" &&
        providerSelection.script.kind === "local" &&
        effectiveLocalLlmId != null
          ? { llm: effectiveLocalLlmId }
          : {}),
      }),
      overnight,
      verify,
      ...cloudArgs(providerSelection),
    };
    try {
      await invoke<string>("create_job", { args });
      if (voiceId != null) writeLastVoiceId(voiceId);
      onCreated();
      onClose();
    } catch (raw) {
      const consent = parseConsentRequired(String(raw));
      if (consent != null && consent.length > 0) {
        // The writer refused before any cloud request: consent first. The
        // sheet returns to the untouched form on decline; a grant resubmits
        // with the SAME overnight intent the user chose.
        setConsentQueue(consent);
        setConsentIntent(intent);
        setConsentToken(token);
        setSubmitting(false);
        return;
      }
      const engineNeed = engineRefusal(String(raw));
      if (engineNeed != null) {
        setSubmitting(false);
        openEngineSetup(engineNeed, () => void submit(intent, token));
        return;
      }
      setError(castStartErrorMessage(String(raw)));
      setSubmitting(false);
    }
  };

  const grantConsents = async () => {
    const intent = consentIntent;
    const token = consentToken;
    if (consentQueue == null || granting) return;
    setGranting(true);
    try {
      for (const requirement of consentQueue) {
        // The grant submits EXACTLY the tuple the sheet displayed; the
        // backend refuses if the connection's facts drifted since.
        await invoke("cast_consent_grant", {
          bookId: book.id,
          connectionId: requirement.connectionId,
          phase: requirement.phase,
          hosts: requirement.hosts,
          dataCategories: requirement.dataCategories,
        });
      }
      setConsentQueue(null);
      setGranting(false);
      setConsentToken(null);
      await submit(intent, token);
    } catch (raw) {
      setGranting(false);
      setConsentQueue(null);
      setError(castStartErrorMessage(String(raw)));
    }
  };

  useDialogFocus(dialogRef, true, {
    onEscape: onClose,
    fallbackSelector: `[data-testid="create-audio-${book.id}"]`,
  });
  useDialogFocus(confirmRef, confirmingRed != null, {
    onEscape: () => setConfirmingRed(null),
    initialFocusSelector: "[data-testid='red-confirm-choose-another']",
  });
  useDialogFocus(consentRef, consentQueue != null, {
    onEscape: () => setConsentQueue(null),
    initialFocusSelector: "[data-testid='cast-consent-decline']",
  });

  const confirmRedAndStart = (intent: StartIntent) => {
    void submit(intent, confirmToken(fitBadgeKind, badgeModels));
  };
  const primary = primaryAction(fitBadgeKind);
  const confirmCopy = redConfirmCopy();

  const allSelected =
    chapters != null &&
    chapters.length > 0 &&
    chapters.every((chapter) => selected.has(chapter.id));

  const selectAllRef = useRef<HTMLInputElement | null>(null);
  useEffect(() => {
    if (selectAllRef.current != null) {
      selectAllRef.current.indeterminate = selected.size > 0 && !allSelected;
    }
  });

  const shiftRef = useRef(false);
  const anchorRef = useRef<number | null>(null);
  const pickChapter = (index: number, shift: boolean) => {
    if (chapters == null) return;
    const anchor = anchorRef.current;
    anchorRef.current = index;
    if (!shift || anchor == null || anchor === index) {
      toggleChapter(chapters[index].id);
      return;
    }
    const keep = selected.has(chapters[anchor].id);
    const [from, to] = anchor < index ? [anchor, index] : [index, anchor];
    setSelected((current) => {
      const next = new Set(current);
      for (const chapter of chapters.slice(from, to + 1)) {
        if (keep) next.add(chapter.id);
        else next.delete(chapter.id);
      }
      return next;
    });
  };

  const teacherCloudUnset =
    mode === "teacher" &&
    scriptWhere === "cloud" &&
    providerSelection.script.kind !== "cloud";
  const cloudVoiceUnset =
    voiceEngineMode === "cloud" &&
    (providerSelection.tts.kind !== "cloud" || activeCloudVoice == null);
  const choicesUnready =
    !warningsReady ||
    setupReadError ||
    cloudChoicesUnavailable ||
    cloudVoiceUnset ||
    teacherCloudUnset ||
    (providerSelection.script.kind === "cloud" && cloudScriptConn == null);
  const startDisabled =
    !scopeValid ||
    choicesUnready ||
    submitting ||
    !warningsReady ||
    (voiceId == null && providerSelection.tts.kind !== "cloud") ||
    missing.length > 0 ||
    teacherCloudUnset;

  const authorText = book.author?.trim() ?? "";
  const showAuthor = authorText !== "" && !/^\d+$/.test(authorText);

  const [step, setStep] = useState<CastStep>(draft?.step ?? "scope");
  const openSetup = (destination: "models" | "providers") => {
    if (submitting || granting) return;
    saveCastDraft(book.id, book.title, {
      step,
      scopeKind,
      selected: chapters == null ? null : [...selected],
      pageStart,
      pageEnd,
      mode,
      voiceEngineMode,
      pickedVoiceId,
      selectedLocalLlmId,
      selectedCloudProviderId,
      cloudVoiceMemory,
      explainFigures,
      providerSelection,
      stickyMemory,
      selectedProviders,
      customScriptModel,
      fetchedScriptModels,
      scriptWherePick,
      verify,
    });
    dismiss();
    if (destination === "models") onPickModels();
    else goToView("providers");
  };
  const openModels = () => openSetup("models");
  const openProviders = () => openSetup("providers");
  const needsAttention =
    choicesUnready ||
    missing.length > 0 ||
    teacherCloudUnset ||
    fitBadgeKind === "yellow" ||
    fitBadgeKind === "orange" ||
    fitBadgeKind === "red";
  const scopeSummary =
    scopeKind === "pages"
      ? pageRange.ok
        ? t("cast.step.scopePages", {
            start: pageRange.start,
            end: pageRange.end,
          })
        : t("cast.step.scopeInvalid")
      : chapters == null || selected.size === 0
        ? t("cast.step.scopeNone")
        : allSelected
          ? t("cast.step.scopeAll", { total: chapters.length })
          : t("cast.step.scopeSome", {
              selected: selected.size,
              total: chapters.length,
            });
  const voiceName =
    voiceEngineMode === "cloud" ? activeCloudVoice?.name : selectedVoice?.name;
  const modeName =
    mode === "teacher" ? t("cast.modeTeacher") : t("cast.modeNarrator");
  const castSteps: {
    id: CastStep;
    label: string;
    summary: string;
    warn: boolean;
  }[] = [
    {
      id: "scope",
      label: t("cast.step.scope"),
      summary: scopeSummary,
      warn: false,
    },
    {
      id: "sound",
      label: t("cast.step.sound"),
      summary: voiceName != null ? `${modeName} · ${voiceName}` : modeName,
      warn: needsAttention,
    },
    {
      id: "review",
      label: t("cast.step.review"),
      summary:
        !scopeValid || choicesUnready
          ? t("cast.step.notReady")
          : estimating
            ? t("cast.step.estimating")
            : t("cast.step.reviewEta", { eta: etaLabel }),
      warn: needsAttention,
    },
  ];
  const stepTitle =
    step === "scope"
      ? t("cast.step.scopeTitle")
      : step === "sound"
        ? t("cast.step.soundTitle")
        : t("cast.step.reviewTitle");

  const modalContent = (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-4 sm:p-8 pt-16 sm:pt-8"
      data-testid="job-modal"
      data-modal-layer=""
    >
      <button
        type="button"
        aria-label={t("cast.closeDialog")}
        onClick={onClose}
        className="animate-fade-in absolute inset-0 cursor-default bg-ink-950/85 backdrop-blur-sm"
      />
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={t("cast.dialogLabel", { title: book.title })}
        tabIndex={-1}
        className="animate-modal-in relative flex h-[min(680px,92dvh)] w-full max-w-[56rem] flex-col overflow-hidden rounded-2xl border border-ink-700 bg-ink-900 shadow-overlay outline-none md:flex-row"
      >
        <aside className="flex shrink-0 flex-col gap-4 bg-ink-950 px-3.5 pt-5 pb-3 md:w-60 md:gap-7 md:pt-6 md:pb-5">
          <div className="min-w-0 px-3">
            <h2 className="line-clamp-2 font-serif text-title font-semibold text-ink-50 [overflow-wrap:anywhere] md:line-clamp-3">
              {book.title}
            </h2>
            <p className="mt-1.5 text-secondary text-ink-500">
              {showAuthor && (
                <span className="block truncate">{book.author}</span>
              )}
              {book.pageCount != null && (
                <span className="tabular-nums">
                  {t("cast.bookMeta", {
                    pages: book.pageCount,
                    chapters: book.chapterCount,
                  })}
                </span>
              )}
            </p>
          </div>
          <div
            role="tablist"
            aria-label={t("cast.stepsLabel")}
            aria-orientation="vertical"
            className="flex gap-1 overflow-x-auto md:flex-col md:overflow-visible"
          >
            {castSteps.map((item, index) => {
              const active = item.id === step;
              return (
                <button
                  key={item.id}
                  type="button"
                  role="tab"
                  id={`cast-step-${item.id}`}
                  aria-selected={active}
                  aria-controls="cast-step-panel"
                  data-testid={`cast-step-${item.id}`}
                  onClick={() => setStep(item.id)}
                  className={`grid min-w-36 shrink-0 grid-cols-[1.5rem_minmax(0,1fr)] items-start gap-x-3 rounded-xl px-3 py-2.5 text-start transition-colors md:min-w-0 ${
                    active
                      ? "bg-surface shadow-lift-2 ring-1 ring-ink-800"
                      : "hover:bg-ink-900"
                  }`}
                >
                  <span
                    className={`flex h-6 w-6 items-center justify-center rounded-full font-mono text-label ${
                      active
                        ? "bg-ink-50 text-ink-950"
                        : "text-ink-500 ring-1 ring-inset ring-ink-700"
                    }`}
                  >
                    {index + 1}
                  </span>
                  <span
                    className={`flex items-center gap-2 text-body leading-6 font-semibold ${
                      active ? "text-ink-50" : "text-ink-400"
                    }`}
                  >
                    {item.label}
                    {item.warn && (
                      <span
                        title={t("cast.needsLook")}
                        className="h-1.5 w-1.5 rounded-full bg-wash-red-ink"
                      />
                    )}
                  </span>
                  <span className="col-start-2 truncate text-label text-ink-500">
                    {item.summary}
                  </span>
                </button>
              );
            })}
          </div>
          <button
            type="button"
            data-testid="open-voice-tutor"
            onClick={openStudy}
            aria-label={t("cast.talkAbout")}
            title={t("cast.talkHint")}
            className="mt-auto flex w-full items-center gap-3 rounded-xl bg-ink-900 px-3 py-3 text-start ring-1 ring-ink-800 transition-colors hover:bg-ink-850 active:scale-[0.98]"
          >
            <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-ember-500 text-on-accent">
              <MicIcon className="h-4 w-4" />
            </span>
            <span className="min-w-0">
              <span className="block text-secondary font-semibold text-ink-100">
                {t("cast.talkAbout")}
              </span>
              <span className="block text-label text-ink-500">
                {t("cast.talkSub")}
              </span>
            </span>
          </button>
        </aside>

        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          <header className="flex shrink-0 items-center gap-4 ps-7 pe-4 pt-4">
            <h3 className="min-w-0 flex-1 truncate text-heading font-semibold text-ink-50">
              {stepTitle}
            </h3>
            <Button icon label={t("player.close")} onClick={onClose}>
              <CloseIcon className="h-4 w-4" />
            </Button>
          </header>

          <div
            id="cast-step-panel"
            role="tabpanel"
            aria-labelledby={`cast-step-${step}`}
            data-testid="job-modal-panes"
            className="flex min-h-0 flex-1 flex-col overflow-y-auto px-7 pt-4 pb-5"
          >
            {(setupReadError || providerReadError) && (
              <div
                role="alert"
                className="mb-3 rounded-xl border border-wash-red-ink/30 p-3 text-secondary text-wash-red-ink"
              >
                <p>
                  {t(
                    setupReadError
                      ? "cast.draft.readFailed"
                      : "cast.draft.providersReadFailed",
                  )}
                </p>
                <button
                  type="button"
                  disabled={warningsPending > 0 || providerReading}
                  onClick={() => {
                    setReadRevision((value) => value + 1);
                    dialogRef.current?.focus({ preventScroll: true });
                  }}
                  className="mt-2 underline"
                >
                  {t(
                    warningsPending > 0 || providerReading
                      ? "models.refreshing"
                      : "models.retry",
                  )}
                </button>
              </div>
            )}
            {draftAdjusted && (
              <p role="status" className="mb-3 text-secondary text-ember-300">
                {t("cast.draft.adjusted")}
              </p>
            )}
            {step === "scope" && (
              <div className="flex min-h-0 flex-1 flex-col gap-4">
                <div className="flex flex-wrap items-center justify-between gap-3">
                  <div className="w-56">
                    <SegmentedSwitch
                      label={t("cast.scope")}
                      value={scopeKind}
                      onChange={setScopeKind}
                      options={[
                        {
                          value: "chapters",
                          label: t("reader.chapters"),
                          testId: "scope-chapters-tab",
                        },
                        {
                          value: "pages",
                          label: t("companion.pages"),
                          testId: "scope-pages-tab",
                        },
                      ]}
                    />
                  </div>
                  {scopeKind === "chapters" &&
                    chapters != null &&
                    chapters.length > 1 && (
                      <span className="text-label text-ink-500">
                        {t("cast.rangeHint")}
                      </span>
                    )}
                </div>

                {scopeKind === "chapters" ? (
                  <div className="flex min-h-40 flex-1 flex-col overflow-hidden rounded-xl border border-ink-800">
                    <label className="flex shrink-0 cursor-pointer items-center gap-3 px-4 py-3">
                      <input
                        ref={selectAllRef}
                        type="checkbox"
                        data-testid="select-all-chapters"
                        checked={allSelected}
                        disabled={chapters == null || chapters.length === 0}
                        aria-label={
                          allSelected
                            ? t("cast.deselectAll")
                            : t("cast.selectAll")
                        }
                        onChange={() =>
                          setSelected(
                            allSelected
                              ? new Set()
                              : new Set(
                                  (chapters ?? []).map((chapter) => chapter.id),
                                ),
                          )
                        }
                        className="h-4 w-4 shrink-0 accent-ember-500"
                      />
                      <span className="text-body font-medium text-ink-100 tabular-nums">
                        {t("cast.selectedCount", {
                          selected: selected.size,
                          total: chapters?.length ?? 0,
                        })}
                      </span>
                      <span className="text-secondary text-ink-500 tabular-nums">
                        ·{" "}
                        {t("cast.selectedPages", { pages: selectedPageCount })}
                      </span>
                    </label>
                    {chapters == null ? (
                      <ChapterSkeleton />
                    ) : chapters.length === 0 ? (
                      <p className="border-t border-ink-800 px-4 py-6 text-center text-secondary text-ink-500">
                        {t("cast.noChapters")}
                      </p>
                    ) : (
                      <div
                        data-testid="chapter-list"
                        className="min-h-0 flex-1 overflow-y-auto border-t border-ink-800"
                      >
                        {chapters.map((chapter, index) => {
                          const on = selected.has(chapter.id);
                          return (
                            <label
                              key={chapter.id}
                              onMouseDown={(event) => {
                                shiftRef.current = event.shiftKey;
                              }}
                              className="flex cursor-pointer items-center gap-3 border-t border-ink-800/50 px-4 py-2.5 transition-colors select-none first:border-t-0 hover:bg-ink-850"
                            >
                              <input
                                type="checkbox"
                                data-testid={`chapter-check-${chapter.id}`}
                                checked={on}
                                onChange={() => {
                                  pickChapter(index, shiftRef.current);
                                  shiftRef.current = false;
                                }}
                                className="h-4 w-4 shrink-0 accent-ember-500"
                              />
                              <span
                                className={`min-w-0 flex-1 truncate text-body ${
                                  on ? "text-ink-100" : "text-ink-400"
                                }`}
                              >
                                {chapter.title}
                              </span>
                              <span className="shrink-0 font-mono text-label text-ink-500">
                                {chapterPages(chapter, locale)}
                              </span>
                            </label>
                          );
                        })}
                      </div>
                    )}
                  </div>
                ) : (
                  <div className="space-y-3 pt-1">
                    <div className="flex flex-wrap items-end gap-3">
                      <label className="flex flex-col gap-1.5">
                        <span className="text-secondary text-ink-400">
                          {t("cast.startPage")}
                        </span>
                        <input
                          type="number"
                          min={1}
                          value={pageStart}
                          data-testid="page-start-input"
                          onChange={(event) => setPageStart(event.target.value)}
                          className="w-28 rounded-lg border border-ink-800 bg-ink-850 px-3 py-2 font-mono text-body text-ink-100 transition-colors focus:border-ember-500/60 focus:outline-none"
                        />
                      </label>
                      <label className="flex flex-col gap-1.5">
                        <span className="text-secondary text-ink-400">
                          {t("cast.endPage")}
                        </span>
                        <input
                          type="number"
                          min={1}
                          value={pageEnd}
                          data-testid="page-end-input"
                          onChange={(event) => setPageEnd(event.target.value)}
                          className="w-28 rounded-lg border border-ink-800 bg-ink-850 px-3 py-2 font-mono text-body text-ink-100 transition-colors focus:border-ember-500/60 focus:outline-none"
                        />
                      </label>
                    </div>
                    {!pageRange.ok && (
                      <p
                        data-testid="page-range-error"
                        className="text-secondary text-wash-red-ink"
                      >
                        {pageRange.error}
                      </p>
                    )}
                    <p className="text-secondary text-ink-500">
                      {t("cast.pageSpanHint")}
                    </p>
                  </div>
                )}
              </div>
            )}

            {step === "sound" && (
              <div className="flex flex-col divide-y divide-ink-800">
                <SettingRow label={t("cast.style")}>
                  <div className="w-64">
                    <SegmentedSwitch
                      label={t("cast.style")}
                      value={mode}
                      onChange={pickMode}
                      options={[
                        {
                          value: "narrator",
                          label: t("cast.modeNarrator"),
                          testId: "mode-narrator",
                        },
                        {
                          value: "teacher",
                          label: t("cast.modeTeacher"),
                          testId: "mode-teacher",
                        },
                      ]}
                    />
                  </div>
                  <p className="mt-2 text-secondary text-ink-500">
                    {mode === "teacher"
                      ? t("cast.modeTeacherHint")
                      : t("cast.modeNarratorHint")}
                  </p>
                  {mode === "narrator" &&
                    (providerOptions?.scriptConnections.length ?? 0) > 0 && (
                      <div
                        id="cast-script-narrator-note"
                        data-testid="cast-script-narrator-note"
                        className="mt-3 max-w-md space-y-1 rounded-xl bg-ink-850 px-3.5 py-3 text-label"
                      >
                        <p className="font-medium text-ink-200">
                          {t("cast.narratorNote.title")}
                        </p>
                        <p className="text-ink-400">
                          {t("cast.narratorNote.body")}
                        </p>
                      </div>
                    )}
                  {mode === "teacher" && (
                    <div className="mt-4 max-w-md space-y-2">
                      <span className="block text-label font-medium text-ink-300">
                        {t("cast.runsOn")}
                      </span>
                      <div className="w-64">
                        <SegmentedSwitch
                          label={t("cast.runsOn")}
                          testId="teacher-where"
                          value={scriptWhere}
                          onChange={pickScriptWhere}
                          options={[
                            {
                              value: "local",
                              label: t("cast.engine.local"),
                              testId: "teacher-where-local",
                            },
                            {
                              value: "cloud",
                              label: t("cast.engine.cloud"),
                              testId: "teacher-where-cloud",
                            },
                          ]}
                        />
                      </div>
                      {scriptWhere === "cloud" &&
                      (providerReading || providerReadError) ? (
                        providerReading ? (
                          <p
                            role="status"
                            className="text-secondary text-ink-400"
                          >
                            {t("models.refreshing")}
                          </p>
                        ) : null
                      ) : scriptWhere === "cloud" &&
                        llmCloudProviders.length === 0 ? (
                        <div
                          data-testid="cast-no-cloud-script"
                          className="space-y-2 rounded-xl border border-ink-800 bg-ink-950 p-4 text-center"
                        >
                          <p className="text-secondary text-ink-400">
                            {t("cast.noCloudScript")}
                          </p>
                          <button
                            type="button"
                            data-testid="cast-open-providers-script"
                            onClick={openProviders}
                            className="text-secondary font-medium text-ember-400 underline underline-offset-2 transition-colors hover:text-ember-300"
                          >
                            {t("cast.connectProviders")}
                          </button>
                        </div>
                      ) : (
                        <>
                          {llmPlan.mode === "dropdown" ? (
                            <label
                              htmlFor="teacher-llm-select"
                              className="mt-3 block text-label font-medium text-ink-300 mb-1.5"
                            >
                              {t("cast.teacherBrain")}
                            </label>
                          ) : (
                            <p className="mt-3 block text-label font-medium text-ink-300 mb-1.5">
                              {t("cast.teacherBrain")}
                            </p>
                          )}
                          {llmPlan.mode === "hidden" ? null : llmPlan.mode ===
                            "fixed-auto" ? (
                            llmPlan.fixedOption?.isLocal === false ? (
                              <CustomDropdown
                                id="cast-cascade-model-llm"
                                testId="cast-cascade-model-llm"
                                label={t("cast.teacherBrain")}
                                value={
                                  customScriptModel != null
                                    ? "__custom__"
                                    : cloudScriptModelValue
                                }
                                onChange={handleSelectLlmModel}
                                options={[
                                  ...[llmPlan.fixedOption].flatMap((opt) =>
                                    opt == null
                                      ? []
                                      : [
                                          {
                                            value: opt.id,
                                            label: opt.name,
                                            sublabel: opt.sublabel,
                                            badge: opt.badge,
                                          },
                                        ],
                                  ),
                                  {
                                    value: "__custom__",
                                    label: t("cast.customModel"),
                                  },
                                ]}
                              />
                            ) : (
                              <div
                                data-testid="cast-fixed-llm"
                                className="flex items-center justify-between rounded-lg border border-ink-800 bg-ink-950 px-3 py-2 text-body text-ink-100"
                              >
                                <span className="truncate">
                                  {llmPlan.fixedOption?.name}
                                  <span className="ml-2 inline-flex items-center rounded px-1.5 py-0.5 text-label font-mono uppercase bg-ink-800 text-ink-300">
                                    {t("cast.default")}
                                  </span>
                                </span>
                                <button
                                  type="button"
                                  data-testid="cast-manage-models-llm"
                                  onClick={openModels}
                                  className="text-secondary font-medium text-ember-400 transition-colors hover:text-ember-300 underline underline-offset-2 shrink-0 ml-2"
                                >
                                  {t("cast.manageModels")}
                                </button>
                              </div>
                            )
                          ) : llmPlan.mode === "cascading" ? (
                            <div className="space-y-3">
                              <div>
                                <label
                                  htmlFor="cast-cascade-provider-llm"
                                  className="block text-label font-medium text-ink-400 mb-1"
                                >
                                  {t("cast.scriptProvider")}
                                </label>
                                <CustomDropdown
                                  id="cast-cascade-provider-llm"
                                  testId="cast-cascade-provider-llm"
                                  label={t("cast.scriptProvider")}
                                  value={llmPlan.selectedProviderId ?? "local"}
                                  onChange={handleSelectLlmProvider}
                                  options={llmPlan.providers.map((p) => ({
                                    value: p.id,
                                    label: p.name,
                                    badge: p.isLocal
                                      ? t("cast.engine.local")
                                      : t("cast.engine.cloud"),
                                  }))}
                                />
                              </div>
                              <div>
                                <label
                                  htmlFor="cast-cascade-model-llm"
                                  className="block text-label font-medium text-ink-400 mb-1"
                                >
                                  {t("cast.model")}
                                </label>
                                {llmPlan.cascadingModelIsFixed &&
                                llmPlan.fixedOption?.isLocal ? (
                                  <div
                                    data-testid="cast-fixed-llm"
                                    className="flex items-center justify-between rounded-lg border border-ink-800 bg-ink-950 px-3 py-2 text-body text-ink-100"
                                  >
                                    <span className="truncate">
                                      {llmPlan.fixedOption?.name}
                                      <span className="ml-2 inline-flex items-center rounded px-1.5 py-0.5 text-label font-mono uppercase bg-ink-800 text-ink-300">
                                        {t("cast.default")}
                                      </span>
                                    </span>
                                    <button
                                      type="button"
                                      data-testid="cast-manage-models-llm"
                                      onClick={openModels}
                                      className="text-secondary font-medium text-ember-400 transition-colors hover:text-ember-300 underline underline-offset-2 shrink-0 ml-2"
                                    >
                                      {t("cast.manageModels")}
                                    </button>
                                  </div>
                                ) : (
                                  <CustomDropdown
                                    id="cast-cascade-model-llm"
                                    testId="cast-cascade-model-llm"
                                    label={t("cast.teacherBrain")}
                                    value={
                                      customScriptModel != null
                                        ? "__custom__"
                                        : providerSelection.script.kind ===
                                            "cloud"
                                          ? cloudScriptModelValue
                                          : (effectiveLocalLlmId ?? "")
                                    }
                                    onChange={handleSelectLlmModel}
                                    options={[
                                      ...(llmPlan.cascadingModelIsFixed &&
                                      llmPlan.fixedOption
                                        ? [llmPlan.fixedOption]
                                        : llmPlan.cascadingModelOptions
                                      ).map((opt) => ({
                                        value: opt.id,
                                        label: opt.name,
                                        sublabel: opt.sublabel,
                                        badge: opt.badge,
                                      })),
                                      ...(llmPlan.selectedProviderId !== "local"
                                        ? [
                                            {
                                              value: "__custom__",
                                              label: t("cast.customModel"),
                                            },
                                          ]
                                        : []),
                                    ]}
                                  />
                                )}
                              </div>
                            </div>
                          ) : (
                            <CustomDropdown
                              id="teacher-llm-select"
                              testId="teacher-llm-select"
                              label={t("cast.teacherBrain")}
                              value={
                                customScriptModel != null
                                  ? "__custom__"
                                  : providerSelection.script.kind === "cloud"
                                    ? cloudScriptModelValue
                                    : (effectiveLocalLlmId ?? "")
                              }
                              onChange={handleSelectLlmModel}
                              options={[
                                ...llmPlan.dropdownOptions.map((opt) => ({
                                  value: opt.id,
                                  label: opt.name,
                                  sublabel: opt.sublabel,
                                  badge: opt.badge,
                                  group:
                                    llmPlan.providerCount === 1
                                      ? opt.providerName
                                      : undefined,
                                  testId: `cast-option-${opt.id}`,
                                })),
                                ...(providerSelection.script.kind === "cloud"
                                  ? [
                                      {
                                        value: "__custom__",
                                        label: t("cast.customModel"),
                                      },
                                    ]
                                  : []),
                              ]}
                              placeholder={t("cast.selectModelForTeacher")}
                            />
                          )}
                          {providerSelection.script.kind === "cloud" &&
                            cloudScriptConn != null && (
                              <div className="space-y-2 pt-1">
                                {customScriptModel != null && (
                                  <div>
                                    <label
                                      htmlFor="cast-script-model"
                                      className="block text-label font-medium text-ink-400 mb-1"
                                    >
                                      {t("cast.model")}
                                    </label>
                                    <input
                                      id="cast-script-model"
                                      data-testid="cast-script-model"
                                      value={customScriptModel}
                                      onChange={(event) =>
                                        handleCustomScriptModel(
                                          event.target.value,
                                        )
                                      }
                                      className="w-full rounded-lg border border-ink-800 bg-ink-950 px-3 py-2 font-mono text-body text-ink-100 transition-colors focus:border-ember-500/60 focus:outline-none"
                                      aria-label={t("cast.modelField")}
                                    />
                                  </div>
                                )}
                                <button
                                  type="button"
                                  data-testid="cast-fetch-models"
                                  disabled={fetchingScriptModels}
                                  onClick={() =>
                                    void fetchScriptModels(
                                      cloudScriptConn.connectionId,
                                    )
                                  }
                                  className="text-secondary font-medium text-ember-400 transition-colors hover:text-ember-300 disabled:opacity-60"
                                >
                                  {fetchingScriptModels
                                    ? t("cast.fetchingModels")
                                    : t("cast.fetchModels")}
                                </button>
                                <div>
                                  <label
                                    htmlFor="cast-script-effort"
                                    className="block text-label font-medium text-ink-400 mb-1"
                                  >
                                    {t("cast.effort")}
                                  </label>
                                  <CustomDropdown
                                    id="cast-script-effort"
                                    testId="cast-script-effort"
                                    label={t("cast.effort")}
                                    value={cloudScriptEffort}
                                    onChange={(val) =>
                                      setProviderSelection((prev) =>
                                        prev.script.kind === "cloud"
                                          ? {
                                              ...prev,
                                              script: {
                                                ...prev.script,
                                                effort: val as ReasoningEffort,
                                              },
                                            }
                                          : prev,
                                      )
                                    }
                                    options={EFFORT_CHOICES.map((choice) => ({
                                      value: choice.value,
                                      label: t(choice.label),
                                      sublabel: t(choice.hint),
                                    }))}
                                  />
                                  <span
                                    data-testid="cast-script-effort-hint"
                                    className="mt-1 block text-label text-ink-500"
                                  >
                                    {t(
                                      EFFORT_CHOICES.find(
                                        (choice) =>
                                          choice.value === cloudScriptEffort,
                                      )!.hint,
                                    )}
                                  </span>
                                </div>
                                {cloudScriptConn.loopback ? (
                                  <p
                                    data-testid="cast-loopback-script"
                                    className="text-label text-ink-500"
                                  >
                                    {LOOPBACK_FACT()}
                                  </p>
                                ) : (
                                  <CostFact
                                    phase="script"
                                    connectionId={cloudScriptConn.connectionId}
                                    model={
                                      providerSelection.script.kind === "cloud"
                                        ? providerSelection.script.model
                                        : null
                                    }
                                    bookId={book.id}
                                    scope={scopeArg}
                                    mode={mode}
                                  />
                                )}
                              </div>
                            )}
                        </>
                      )}
                    </div>
                  )}
                </SettingRow>

                <SettingRow label={t("cast.voice")}>
                  <VoiceSelector
                    usableVoices={usableVoices}
                    selectedLocalVoiceId={voiceId}
                    onSelectLocalVoice={handleSelectLocalVoice}
                    localEngines={localEngines}
                    selectedEngineId={voiceEngine}
                    onSelectEngine={handleSelectEngine}
                    preferredLanguage={effectiveLanguage}
                    cloudVoices={cloudVoices}
                    cloudStatus={
                      providerReading
                        ? "loading"
                        : providerReadError
                          ? "error"
                          : "ready"
                    }
                    cloudProviders={ttsCloudProviders}
                    selectedCloudProviderId={effectiveCloudProviderId}
                    selectedCloudVoiceId={effectiveCloudVoiceId}
                    onSelectCloudProvider={handleSelectCloudProvider}
                    onSelectCloudVoice={handleSelectCloudVoice}
                    engineMode={voiceEngineMode}
                    onSwitchEngine={handleSwitchVoiceEngine}
                    fit={fit}
                    voiceMismatch={voiceMismatch}
                    selectedMismatch={selectedMismatch}
                    alignAdvisory={alignAdvisory}
                    onOpenModels={openModels}
                    onCloneVoice={() => setCloneOpen(true)}
                    onOpenProviders={openProviders}
                    activeCloudVoice={activeCloudVoice}
                    bookId={book.id}
                    scopeArg={scopeArg}
                    mode={mode}
                  />
                </SettingRow>

                <SettingRow label={t("cast.figures")} testId="figure-section">
                  <label className="flex cursor-pointer items-start gap-5">
                    <span className="min-w-0 flex-1 sm:pt-1.5">
                      <span className="block text-body text-ink-100">
                        {t("cast.explainFigures")}
                      </span>
                      <span
                        data-testid="figure-sublabel"
                        className="mt-0.5 block text-secondary text-ink-500"
                      >
                        {figureLine(castFigures, catalogModels)}
                      </span>
                    </span>
                    <input
                      type="checkbox"
                      role="switch"
                      data-testid="cast-explain-figures"
                      checked={explainFigures}
                      onChange={(event) =>
                        setExplainFigures(event.target.checked)
                      }
                      className="ui-switch mt-2"
                    />
                  </label>
                </SettingRow>

                <SettingRow label={t("cast.accuracyCheck")}>
                  <label className="flex cursor-pointer items-start gap-5">
                    <span className="min-w-0 flex-1 text-secondary text-ink-400 sm:pt-1.5">
                      {t("cast.accuracyCheckHint")}
                    </span>
                    <input
                      type="checkbox"
                      role="switch"
                      data-testid="verify-toggle"
                      aria-label={t("cast.accuracyCheck")}
                      checked={verify}
                      onChange={(event) => setVerify(event.target.checked)}
                      className="ui-switch mt-2"
                    />
                  </label>
                </SettingRow>
              </div>
            )}

            {step === "review" &&
              (choicesUnready ? (
                <div className="flex flex-col gap-3">
                  <p role="status" className="text-body text-ink-300">
                    {t(
                      !warningsReady || providerReading
                        ? "models.refreshing"
                        : cloudVoiceUnset || teacherCloudUnset
                          ? "cast.draft.cloudChoiceRequired"
                          : "cast.step.notReady",
                    )}
                  </p>
                  {(cloudVoiceUnset || teacherCloudUnset) && (
                    <button
                      type="button"
                      data-testid="cast-finish-cloud-choices"
                      onClick={() => {
                        setStep("sound");
                        document.getElementById("cast-step-sound")?.focus();
                      }}
                      className="self-start rounded-lg border border-ink-700 px-3 py-2 text-body text-ink-200"
                    >
                      {t("cast.draft.finishChoices")}
                    </button>
                  )}
                </div>
              ) : (
                <div className="flex flex-col">
                  <p className="text-secondary text-ink-500">
                    {t("cast.estimatedTime")}
                  </p>
                  <p className="mt-0.5 flex flex-wrap items-baseline gap-x-3">
                    {estimating ? (
                      <span className="shimmer-bar inline-block h-7 w-32 rounded bg-ink-800 align-middle" />
                    ) : (
                      <span className="font-mono text-display font-medium text-ink-50 tabular-nums">
                        {etaLabel}
                      </span>
                    )}
                    <span className="text-secondary text-ink-500">
                      {ttsIsCloud || scriptIsCloud
                        ? t("cast.etaCloud")
                        : t("cast.etaLocal")}
                    </span>
                  </p>

                  <dl
                    data-testid="phase-summary"
                    className="mt-6 border-b border-ink-800"
                  >
                    {phaseSummary.map((line) => (
                      <div
                        key={line.phase}
                        data-testid={`phase-summary-${line.phase}`}
                        className="grid grid-cols-[6.5rem_minmax(0,1fr)_auto] items-baseline gap-4 border-t border-ink-800 py-3"
                      >
                        <dt className="text-secondary text-ink-500">
                          {line.name}
                        </dt>
                        <dd className="min-w-0">
                          <span className="block text-body text-ink-100">
                            {line.title}
                          </span>
                          {line.expert != null && (
                            <span className="block truncate font-mono text-label text-ink-500">
                              {line.expert}
                            </span>
                          )}
                        </dd>
                        <dd>
                          <button
                            type="button"
                            onClick={() => setStep("sound")}
                            className="text-secondary font-medium text-ember-400 transition-colors hover:text-ember-300"
                          >
                            {t("cast.change")}
                          </button>
                        </dd>
                      </div>
                    ))}
                  </dl>

                  {fitBadgeKind !== "orange" &&
                    fitBadgeKind !== "red" &&
                    isLongJob && (
                      <div
                        role="status"
                        data-testid="overnight-recommendation-tip"
                        className="mt-4 flex items-start gap-2.5 text-secondary text-ink-400"
                      >
                        <InfoIcon className="mt-0.5 h-4 w-4 shrink-0 text-ink-500" />
                        <span>{t("job.overnightRecommendation")}</span>
                      </div>
                    )}
                </div>
              ))}
          </div>

          <footer className="flex shrink-0 flex-col gap-3 border-t border-ink-800 px-7 py-3.5">
            {error && (
              <div
                role="alert"
                data-testid="cast-error"
                className="animate-rise-in flex items-start gap-2.5 rounded-lg border border-wash-red-ink/30 bg-wash-red px-4 py-3 text-body text-wash-red-ink"
              >
                <AlertIcon className="mt-0.5 h-4 w-4 shrink-0" />
                <span className="min-w-0">{error}</span>
              </div>
            )}
            {!choicesUnready && missing.length > 0 && (
              <div
                data-testid="missing-model-warning"
                className="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-xl bg-ember-500/[0.07] px-3.5 py-2.5"
              >
                <AlertIcon className="h-4 w-4 shrink-0 text-ember-300" />
                <div className="min-w-0 flex-1 space-y-0.5">
                  <p className="text-secondary font-medium text-ember-200">
                    {t("cast.missingModel")}
                  </p>
                  {missing.map((item) => (
                    <p
                      key={item.modelId || item.message}
                      className="text-secondary text-ink-300"
                    >
                      {item.message}
                    </p>
                  ))}
                </div>
                <Button
                  variant="soft"
                  size="sm"
                  data-testid="get-missing-model"
                  onClick={openModels}
                  leading={<DownloadIcon className="h-3.5 w-3.5" />}
                >
                  {t("cast.getModel")}
                </Button>
              </div>
            )}
            {!choicesUnready &&
              (fitBadgeKind === "yellow" || fitBadgeKind === "orange") && (
                <div
                  role="status"
                  data-testid="fit-warning-strip"
                  data-badge={fitBadgeKind}
                  className="flex flex-col gap-1 rounded-xl bg-ember-500/[0.07] px-3.5 py-2.5 text-secondary text-ember-200"
                >
                  <span data-testid="fit-warning-eta">
                    {etaWarningLine(etaLabel)}
                  </span>
                  <span
                    data-testid="fit-warning-detail"
                    className="text-ember-300/80"
                  >
                    {t(`fit.${fitBadgeKind}.detail`)}
                  </span>
                  {fitBadgeKind === "orange" && (
                    <span
                      data-testid="fit-overnight-hint"
                      className="text-ember-300/80"
                    >
                      {t("fit.orange.overnightHint")}
                    </span>
                  )}
                  <button
                    type="button"
                    data-testid="pick-faster-model"
                    onClick={openModels}
                    className="self-start text-ember-300 underline underline-offset-2 transition-colors hover:text-ember-200"
                  >
                    {t("fit.pickFaster")}
                  </button>
                </div>
              )}
            {!choicesUnready && fitBadgeKind === "red" && redModel && (
              <div
                role="alert"
                data-testid="fit-warning-strip"
                data-badge="red"
                className="flex flex-col gap-1 rounded-xl bg-wash-red px-3.5 py-2.5 text-secondary text-wash-red-ink"
              >
                <span data-testid="fit-red-copy">
                  {redInlineCopy(
                    redModel,
                    hardware ?? null,
                    badgeModels,
                    culpritRole,
                  )}
                </span>
                <button
                  type="button"
                  data-testid="pick-faster-model"
                  onClick={openModels}
                  className="self-start text-wash-red-ink underline underline-offset-2 transition-colors hover:text-wash-red-ink/80"
                >
                  {t("fit.pickFaster")}
                </button>
              </div>
            )}
            <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
              <p className="me-auto text-secondary text-ink-400">
                {choicesUnready ? (
                  t(
                    !warningsReady || providerReading
                      ? "models.refreshing"
                      : "cast.step.notReady",
                  )
                ) : (
                  <>
                    {t("cast.estimated")}{" "}
                    {estimating ? (
                      <span className="shimmer-bar inline-block h-3 w-16 rounded bg-ink-800 align-middle" />
                    ) : (
                      <span
                        data-testid="eta-label"
                        className="font-mono font-semibold text-ink-100 tabular-nums"
                      >
                        {etaLabel}
                      </span>
                    )}
                  </>
                )}
              </p>
              {step === "review" ? (
                <>
                  <Button
                    variant={primary === "overnight" ? "solid" : "soft"}
                    size="lg"
                    data-testid="queue-overnight"
                    data-primary={primary === "overnight"}
                    disabled={startDisabled}
                    onClick={() => void submit("overnight")}
                  >
                    {t("job.queueOvernight")}
                  </Button>
                  <Button
                    variant={primary === "overnight" ? "soft" : "solid"}
                    size="lg"
                    data-testid="start-job"
                    data-primary={primary === "cast"}
                    disabled={startDisabled}
                    onClick={() => void submit("cast")}
                    leading={<CastIcon className="h-4 w-4" />}
                  >
                    {submitting ? t("job.starting") : t("job.castNow")}
                  </Button>
                </>
              ) : (
                <>
                  <Button
                    variant="quiet"
                    size="lg"
                    data-testid="start-job"
                    data-primary={primary === "cast"}
                    disabled={startDisabled}
                    onClick={() => void submit("cast")}
                  >
                    {submitting ? t("job.starting") : t("job.castNow")}
                  </Button>
                  <Button
                    variant="solid"
                    size="lg"
                    data-testid="cast-next"
                    disabled={step === "scope" && !scopeValid}
                    onClick={() =>
                      setStep(step === "scope" ? "sound" : "review")
                    }
                    trailing={<ChevronIcon className="h-4 w-4" />}
                  >
                    {step === "scope"
                      ? t("cast.step.nextSound")
                      : t("cast.step.nextReview")}
                  </Button>
                </>
              )}
            </div>
          </footer>
        </div>
        {consentQueue != null && (
          <div
            ref={consentRef}
            role="alertdialog"
            aria-modal="true"
            aria-label={consentTitleCopy(consentQueue)}
            data-testid="cast-consent-sheet"
            data-modal-layer=""
            tabIndex={-1}
            className="animate-fade-in absolute inset-0 z-10 flex items-center justify-center bg-ink-950/85 p-6 backdrop-blur-sm"
          >
            <div className="flex w-full max-w-md flex-col gap-3 rounded-2xl border border-ink-800 bg-ink-900 p-5 shadow-xl">
              <h3 className="text-body font-medium text-ink-100">
                {t("cast.beforeStart")}
              </h3>
              {consentQueue.map((requirement) => (
                <div
                  key={`${requirement.connectionId}:${requirement.phase}`}
                  className="flex flex-col gap-1.5 rounded-xl border border-ink-800 bg-ink-950 p-3.5"
                >
                  <p className="text-secondary text-ink-200">
                    {t("cast.consentBefore", {
                      phase: consentPhaseCopy(requirement.phase),
                      data: requirement.dataCategories
                        .map(consentDataCategoryCopy)
                        .join(t("cast.dataJoin")),
                    })}{" "}
                    <span className="font-medium">
                      {requirement.connectionName}
                    </span>{" "}
                    {t("cast.consentAfter", { provider: requirement.provider })}
                  </p>
                  <p className="text-label text-ink-500">
                    {t("cast.destinationHosts", {
                      count: requirement.hosts.length,
                    })}{" "}
                    <span className="font-mono">
                      {requirement.hosts.join(", ")}
                    </span>
                  </p>
                  <p className="text-label text-ink-500">
                    {t("cast.consentKey")}
                    {requirement.retentionUrl ? (
                      <>
                        {" — "}
                        <a
                          href={requirement.retentionUrl}
                          target="_blank"
                          rel="noreferrer"
                          className="text-ember-400 underline underline-offset-2"
                        >
                          {t("cast.theirPolicy")}
                        </a>
                        .
                      </>
                    ) : (
                      t("cast.ownEndpoint")
                    )}
                  </p>
                </div>
              ))}
              <p className="text-label text-ink-500">
                {t("cast.consentScope")}
              </p>
              <div className="flex justify-end gap-2">
                <button
                  type="button"
                  data-testid="cast-consent-decline"
                  onClick={() => setConsentQueue(null)}
                  className="rounded-lg border border-ink-800 px-3.5 py-1.5 text-secondary text-ink-300 transition-colors hover:border-ink-700"
                >
                  {t("cast.notNow")}
                </button>
                <button
                  type="button"
                  data-testid="cast-consent-grant"
                  disabled={granting}
                  onClick={() => void grantConsents()}
                  className="rounded-lg bg-ember-500 px-3.5 py-1.5 text-secondary font-medium text-on-accent transition-colors hover:bg-ember-600 disabled:opacity-60"
                >
                  {granting ? t("cast.recording") : t("cast.agreeStart")}
                </button>
              </div>
            </div>
          </div>
        )}
        {confirmingRed != null && (
          <div
            ref={confirmRef}
            role="alertdialog"
            aria-modal="true"
            aria-label={confirmCopy.title}
            data-testid="red-confirm-dialog"
            data-modal-layer=""
            tabIndex={-1}
            className="animate-fade-in absolute inset-0 z-10 flex items-center justify-center bg-ink-950/85 p-6 backdrop-blur-sm"
          >
            <div className="animate-modal-in flex w-full max-w-sm flex-col gap-4 rounded-2xl border border-wash-red-ink/30 bg-ink-900 p-5 shadow-overlay">
              <p className="text-body text-ink-100">{confirmCopy.body}</p>
              <div className="flex justify-end gap-2">
                <button
                  type="button"
                  data-testid="red-confirm-choose-another"
                  onClick={() => setConfirmingRed(null)}
                  className="rounded-lg border border-ink-700 bg-ink-800 px-4 py-2 text-body font-medium text-ink-200 transition-colors hover:bg-ink-700 active:scale-[0.98]"
                >
                  {confirmCopy.choose}
                </button>
                <button
                  type="button"
                  data-testid="red-confirm-run-anyway"
                  onClick={() => confirmRedAndStart(confirmingRed)}
                  className="rounded-lg bg-wash-red-ink px-4 py-2 text-body font-semibold text-ink-950 transition-colors hover:bg-wash-red-ink/90 active:scale-[0.98]"
                >
                  {confirmCopy.run}
                </button>
              </div>
            </div>
          </div>
        )}
      </div>
      <CloneVoiceModal
        open={cloneOpen}
        cloningReady={cloningReady}
        existing={voices}
        cloningEngines={cloningEngines}
        initialEngine={voiceEngine}
        targetLanguage={effectiveLanguage}
        onClose={() => setCloneOpen(false)}
        onSaved={(profile) => {
          setVoices((prev) => [...prev, profile]);
          setPickedVoiceId(profile.id);
        }}
      />
    </div>
  );

  if (typeof document !== "undefined" && document.body) {
    return createPortal(modalContent, document.body);
  }
  return modalContent;
}

export default JobModal;
