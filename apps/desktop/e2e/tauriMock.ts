import type { Page } from "@playwright/test";
import providerDirectory from "../src-tauri/fixtures/providers/directory.json" with { type: "json" };
import type {
  ProviderCatalog,
  ProviderDefinition,
} from "../src/providerSetupModel";
import type { BookSummary } from "../src/libraryModel";
import type { ChapterSummary } from "../src/jobFormModel";
import type {
  CachedModels,
  CastConsentRow,
  DeleteImpact,
  ProviderFamilyView,
} from "../src/providersModel";

import readingGolden from "./fixtures/reading-timeline.golden.json" with { type: "json" };

export interface MockImportPlan {
  pageCount: number;
  reflowed: boolean;
  layoutAvailable: boolean;
  layoutSeconds: number;
  cpuSeconds?: number;
  offerBoth?: boolean;
  onGpu: boolean;
  silent: boolean;
  recommendLayout: boolean;
}

export interface ImportScenario {
  plan?: MockImportPlan;
  path?: string | null;
  progress?: Array<{ type: "progress"; page: number; total: number }>;
  progressDelayMs?: number;
  book?: BookSummary;
  warnings?: string[];
  error?: string;
  pickerError?: string;
}

export interface MockVoiceProfile {
  id: string;
  name: string;
  engine: string;
  kind: string;
  presetVoiceId?: string | null;
}

export interface MockSegment {
  id: string;
  chapterId: string;
  idx: number;
  pageNo: number;
  kind: string;
  text: string;
  flags: string[];
  hasAudio: boolean;
  durationMs: number | null;
  edited: boolean;
  wer: number | null;
  alignSource: string | null;
}

export interface CloneScenario {
  audioInputs?: Array<{ name: string; isDefault: boolean }>;
  audioInputsError?: string;
  recordSeconds?: number;
  recordError?: string;
  importPath?: string;
  importSeconds?: number;
  importError?: string;
  hitHardStop?: boolean;
  previewError?: string;
  saveError?: string;
}

export interface JobScenario {
  chapters: ChapterSummary[];
  stageDelayMs?: number;
  outputPath?: string;
  createError?: string;
  /** U5 F2: the FIRST create_job fails with this; later ones succeed. */
  createErrorOnce?: string;
  voiceProfiles?: MockVoiceProfile[];
  previewError?: string;
  localRetryError?: string;
  seededJobs?: Array<Record<string, unknown>>;
  /** F11-U6: keep the job rows across a document reload (the SQLite store's
   * role) so a restart pin can assert queue order survives. */
  persistJobs?: boolean;
  /** F11-U6 J7.4: casts whose tutor session is live — `job_delete_preview`
   * reports `live: true` and `delete_job` refuses, as the runtime does. */
  liveJobIds?: string[];
  reportJson?: string;
  segments?: MockSegment[];
  staleExport?: boolean;
  resynthesizeError?: string;
  /** `switch_job_to_overnight` refuses with this, as the runtime refuses
   * while it is shutting down. Exercises the refusal reaching the user. */
  switchToOvernightError?: string;
  cloneScenario?: CloneScenario;
}

export interface MockCatalogModel {
  id: string;
  task: string;
  display_name: string;
  family: string;
  quant?: string | null;
  format: string;
  disk_bytes: number;
  full_gpu_vram_mb?: number | null;
  min_free_ram_mb: number;
  hybrid_capable: boolean;
  quality_score: number;
  speed_score: number;
  capabilities?: string[];
  license: { spdx: string; commercial_ok: boolean };
  deprecated?: boolean;
  perf?: { rtf_gpu?: number; rtf_cpu?: number } | null;
  badge?: "green" | "yellow" | "orange" | "red";
}

export interface MockDevice {
  key: string;
  backend: "cpu" | "cuda" | "rocm" | "vulkan" | "metal";
  ordinal: number;
  vendor: string;
  name: string;
  memoryTotalMb: number | null;
  memoryFreeMb: number | null;
  unified: boolean;
  discrete: boolean;
  memorySource: "nvml" | "vk_heap" | "vk_budget" | "rocm" | "unknown";
}

export interface MockHardwareProfile {
  devices: MockDevice[];
  primaryKey: string | null;
  ram: { totalMb: number; freeMb: number };
  cpu: { coresPhysical: number; coresLogical: number };
  disk: { modelsFreeGb: number };
}

export function fixtureDevice(overrides: Partial<MockDevice> = {}): MockDevice {
  return {
    key: "pci:0000:01:00.0",
    backend: "cuda",
    ordinal: 0,
    vendor: "nvidia",
    name: "Fixture GPU",
    memoryTotalMb: 12_288,
    memoryFreeMb: 12_000,
    unified: false,
    discrete: true,
    memorySource: "nvml",
    ...overrides,
  };
}

export function fixtureDevices(devices: MockDevice[]): {
  devices: MockDevice[];
  primaryKey: string | null;
} {
  return { devices, primaryKey: devices[0]?.key ?? null };
}

export const NO_DEVICES = { devices: [] as MockDevice[], primaryKey: null };

export interface ModelScenario {
  catalogModels: MockCatalogModel[];
  hardware?: MockHardwareProfile;
  computeEstimate?: {
    computeScore: number;
    band: string;
    tokSEst: number | null;
  };
  installed?: Array<{ modelId: string; diskBytes: number }>;
  downloadTicks?: number;
  downloadDelayMs?: number;
  downloadError?: string;
}

export interface ExtractionScenario {
  plan?: {
    pageCount: number;
    reflowed: boolean;
    layoutAvailable: boolean;
    layoutSeconds: number;
    cpuSeconds?: number;
    offerBoth?: boolean;
    onGpu: boolean;
    silent: boolean;
    recommendLayout: boolean;
  };
  blocked?: string;
  runError?: string;
  runWarnings?: string[];
  layoutPassRan?: boolean;
  hangUntilStopped?: boolean;
  progress?: Array<{ bookId: string; page: number; pages: number }>;
  progressDelayMs?: number;
  activeOnMount?: { bookId: string; page: number; pages: number };
  finishOnStop?: boolean;
  deleteError?: string;
  finishBeforeRestore?: boolean;
}

export interface ReadingScenario {
  brokenPages?: number[];
  mediaDataUrl?: string;
  engineDown?: boolean;
}

export interface MockVoiceKeyEntry {
  baseUrl: string;
  provider: string;
  last4: string | null;
  state: "active" | "invalid" | "out_of_quota" | "unchecked";
  keyPresent: boolean;
  custom: boolean;
  /** Slot under the base URL; scenario entries default to the pre-slot 0. */
  slot?: number;
  label?: string | null;
}

/** The full KeyEntryView shape the backend serializes. The account is the
 * slot-qualified store account — `baseUrl` itself for slot 0, `baseUrl#slot`
 * otherwise — mirroring `slot_account` in voice/keys/manager.rs. */
interface MockVoiceKeyView extends Required<MockVoiceKeyEntry> {
  account: string;
  addedAt: number;
  lastUsed: number | null;
}

export interface VoiceKeysScenario {
  storage?: {
    level: "os_keychain" | "encrypted_file" | "session_only";
    locked: boolean;
  };
  entries?: MockVoiceKeyEntry[];
  probedStates?: Record<string, MockVoiceKeyEntry["state"]>;
  refreshedStates?: Record<string, MockVoiceKeyEntry["state"]>;
  consentedUrls?: string[];
  unlockPassphrase?: string;
}

export interface CastProviderScenario {
  scriptConnections: Array<{
    connectionId: string;
    name: string;
    provider: string;
    defaultModel: string;
    loopback: boolean;
  }>;
  ttsVoices: Array<{
    voiceProfileId: string;
    name: string;
    voiceId: string;
    connectionId: string;
    connectionName: string;
    provider: string;
    model: string;
    loopback: boolean;
  }>;
  visionConnections?: Array<{
    connectionId: string;
    name: string;
    provider: string;
    defaultModel: string;
    loopback: boolean;
  }>;
  cost?: Record<string, unknown>;
  fetchedModels?: { models: string[]; suggestedDefault: string | null };
  alignAdvisory?: boolean;
  consentRequired?: Array<Record<string, unknown>>;
}

export interface MockSessionRow {
  id: string;
  book_id: string | null;
  job_id: string | null;
  started_at: number;
  ended_at: number;
  end_reason: "ended" | "unexpected";
  duration_ms: number;
  turn_count: number;
  summary_excerpt: string | null;
  position: { kind: "ms" | "offset" | "none"; value: number | null };
  job_resolves: boolean;
  provider_changed: boolean;
}

export interface MockTranscriptTurn {
  turn_idx: number;
  role: "user" | "assistant";
  text: string;
  created_at: number;
  voiceprint?: unknown;
}

export interface VoiceSessionsScenario {
  rows: MockSessionRow[];
  turns?: Record<string, MockTranscriptTurn[]>;
}

export interface UpdateScenario {
  /** Partial updater snapshot the mock starts from (UPD-1). */
  snapshot: Record<string, unknown>;
  /** What the mocked install run ends in. */
  installOutcome?: "installing" | "failed";
}

export interface Scenario {
  providerDirectory?: ProviderDefinition[];
  books: BookSummary[];
  castProviders?: CastProviderScenario;
  /** S-5 Settings->Providers: families returned verbatim by
   * providers_overview. TYPED with the frontend contracts (shape-gate
   * philosophy at the mock layer): a backend/frontend drift makes the
   * spec fixtures a compile error, never a silently-green mock. */
  providersScenario?: {
    families: ProviderFamilyView[];
    consents?: CastConsentRow[];
    /** Backend-shaped delete impact; omit -> empty arrays. `null` -> the
     * command THROWS (fail-closed path). */
    deleteImpact?: DeleteImpact | null;
    /** Slot count provider_pool_test walks while emitting typed progress. */
    poolTestTotal?: number;
    cachedModels?: CachedModels | null;
    /** When set, provider_draft_models and provider_fetch_models THROW
     * this message (the F1 honest-failure path: inline error + free-text
     * fallback, never a dead dropdown). */
    modelListError?: string;
    setupError?: string;
    setupDelayMs?: number;
    /** Consent ids whose revocation the backend rejects. */
    revokeFails?: string[];
  };
  reading?: typeof readingGolden;
  readingScenario?: ReadingScenario;
  importScenario?: ImportScenario;
  /** Book agent probe. `"notInBuild"` makes `agent_session_probe` reject
   * the way a build without the agent commands does ("Command … not
   * found"), so the companion's Book tab shows the plain metadata panel —
   * the state the round-2 native replay (AT-2-1) was recorded in. */
  bookAgentProbe?: "notInBuild";
  extractionScenario?: ExtractionScenario;
  jobScenario?: JobScenario;
  modelScenario?: ModelScenario;
  onboardingComplete?: boolean;
  /** I18N-1: what the OS reports as its locale (`host_locale`); null = command has no answer. */
  hostLocale?: string | null;
  /** I18N-2: force the Book agent probe answer (default: the consent flow below). */
  agentProbeState?: "unsupported" | "ready";
  updateScenario?: UpdateScenario;
  cacheCapRaw?: string;
  voiceKeysScenario?: VoiceKeysScenario;
  voiceSessionsScenario?: VoiceSessionsScenario;
  activeVoiceSession?: {
    sessionId: string;
    bookId: string | null;
    jobId: string | null;
    purpose: "tutor" | "agent";
  };
  /** F11-U6 §6.1: transcript retention as the runtime stores it; `unreadable`
   * makes both retention commands throw (the no-claim path). */
  retentionScenario?: { value?: "keep" | "never"; unreadable?: boolean };
  pagePeeks?: Record<
    number,
    {
      pageNo: number;
      chapterTitle: string | null;
      text: string;
      clipped: boolean;
    }
  >;
  learningPreferences?: Array<{
    key: string;
    value: string;
    updatedAt: number;
  }>;
  seededExports?: Array<Record<string, unknown>>;
  suppressedBlocks?: Array<{
    id: string;
    jobId: string;
    pageNo: number;
    role: string;
    reason: string;
    certainty: string;
    text: string;
    words: number;
  }>;
}

export function fixtureHardware(
  overrides: Partial<MockHardwareProfile> = {},
): MockHardwareProfile {
  return {
    ...fixtureDevices([fixtureDevice()]),
    ram: { totalMb: 32_768, freeMb: 16_384 },
    cpu: { coresPhysical: 8, coresLogical: 16 },
    disk: { modelsFreeGb: 100 },
    ...overrides,
  };
}

export function fixtureCatalogModels(): MockCatalogModel[] {
  return [
    {
      id: "kokoro-82m",
      task: "tts",
      display_name: "Kokoro 82M",
      family: "kokoro",
      quant: null,
      format: "pytorch",
      disk_bytes: 355_000_000,
      full_gpu_vram_mb: 1200,
      min_free_ram_mb: 2048,
      hybrid_capable: false,
      quality_score: 78,
      speed_score: 95,
      license: { spdx: "Apache-2.0", commercial_ok: true },
      perf: { rtf_gpu: 0.03, rtf_cpu: 1.0 },
    },
    {
      id: "qwen3.5-9b-instruct-q4km",
      task: "llm",
      display_name: "Qwen 3.5 9B",
      family: "qwen",
      quant: "Q4_K_M",
      format: "gguf",
      disk_bytes: 5_700_000_000,
      full_gpu_vram_mb: 7200,
      min_free_ram_mb: 8192,
      hybrid_capable: true,
      quality_score: 92,
      speed_score: 60,
      license: { spdx: "Apache-2.0", commercial_ok: true },
    },
    {
      id: "qwen3.5-32b-class-q4km",
      task: "llm",
      display_name: "Qwen 3.5 27B",
      family: "qwen",
      quant: "Q4_K_M",
      format: "gguf",
      disk_bytes: 16_700_000_000,
      full_gpu_vram_mb: 18_500,
      min_free_ram_mb: 20_000,
      hybrid_capable: true,
      quality_score: 97,
      speed_score: 30,
      license: { spdx: "Apache-2.0", commercial_ok: true },
    },
    {
      id: "gemma4-e4b-q4km",
      task: "llm",
      display_name: "Gemma 4 E4B",
      family: "gemma",
      quant: "Q4_K_M",
      format: "gguf",
      disk_bytes: 6_000_000_000,
      full_gpu_vram_mb: 7000,
      min_free_ram_mb: 8192,
      hybrid_capable: true,
      quality_score: 80,
      speed_score: 55,
      capabilities: ["teacher_mode", "figure_explanation"],
      license: { spdx: "Gemma", commercial_ok: true },
    },
    {
      id: "studio-tts",
      task: "tts",
      display_name: "Studio TTS",
      family: "studio",
      quant: null,
      format: "pytorch",
      disk_bytes: 2_100_000_000,
      full_gpu_vram_mb: 2500,
      min_free_ram_mb: 4096,
      hybrid_capable: false,
      quality_score: 85,
      speed_score: 40,
      license: { spdx: "LicenseRef-NonCommercial", commercial_ok: false },
    },
  ];
}

export function fixtureChapters(): ChapterSummary[] {
  return [
    {
      id: "ch-1",
      idx: 0,
      title: "Getting Started",
      pageStart: 1,
      pageEnd: 4,
      isPseudo: false,
      wordCount: 1200,
    },
    {
      id: "ch-2",
      idx: 1,
      title: "Core Concepts",
      pageStart: 5,
      pageEnd: 9,
      isPseudo: false,
      wordCount: 2400,
    },
    {
      id: "ch-3",
      idx: 2,
      title: "Advanced Topics",
      pageStart: 10,
      pageEnd: 12,
      isPseudo: false,
      wordCount: 900,
    },
  ];
}

export function fixtureBook(overrides: Partial<BookSummary> = {}): BookSummary {
  return {
    format: "pdf",
    id: "book-fixture",
    title: "Fixture Book Title",
    author: "Fixture Author",
    pageCount: 12,
    chapterCount: 3,
    language: "en",
    isScanned: false,
    coverPath: null,
    createdAt: 1,
    hasPseudoChapters: false,
    layoutAnalysed: false,
    reflowed: false,
    blocksStale: false,
    ...overrides,
  };
}

export async function installTauriMock(
  page: Page,
  scenario: Scenario,
): Promise<void> {
  // I18N-1 gate B: I18N_E2E_LOCALE=ar makes every spec boot as if the OS
  // reported that locale (a spec's explicit hostLocale still wins).
  if (scenario.hostLocale === undefined && process.env.I18N_E2E_LOCALE) {
    scenario = { ...scenario, hostLocale: process.env.I18N_E2E_LOCALE };
  }
  // QA-DIAG-1 witness: the Vite dev client reloads the document when its HMR
  // socket closes ("server connection lost. Polling for restart..."), which
  // re-runs this init script and lands a fresh App on Study. Put that line in
  // the test's stderr so a detached-target failure names its cause.
  page.on("console", (message) => {
    if (message.text().includes("server connection lost")) {
      console.error(`[qa-witness] dev client: ${message.text()}`);
    }
  });
  await page.addInitScript(
    (config: Scenario) => {
      const callbacks = new Map<number, (payload: unknown) => void>();
      let updateState: Record<string, unknown> = {
        phase: "idle",
        enabled: true,
        current_version: "0.1.0-e2e",
        available_version: null,
        notes: null,
        release_url: null,
        install_kind: null,
        received: 0,
        total: 0,
        error_code: null,
        error_message: null,
        last_checked_unix: null,
        last_attempt_offline: false,
        dismissed: false,
        ...(config.updateScenario?.snapshot ?? {}),
      };
      const setUpdate = (patch: Record<string, unknown>) => {
        updateState = { ...updateState, ...patch };
        emitEvent("update-state", updateState);
        return updateState;
      };
      const listeners = new Map<string, Set<number>>();
      let nextId = 1;
      let importedWithLayout: boolean | undefined;
      const stopped = new Set<string>();
      let recordingSampleId: string | null = null;
      const cancelledRecordings: string[] = [];
      const voiceSamples: string[] = [];
      const savedVoiceProfiles: MockVoiceProfile[] = [];
      const grantedConsents: Array<Record<string, unknown>> = [];
      const callCounts: Record<string, number> = {};
      (
        window as unknown as { __mockCalls: Record<string, number> }
      ).__mockCalls = callCounts;
      const books = [...config.books];
      const voiceSessions: MockSessionRow[] = (
        config.voiceSessionsScenario?.rows ?? []
      ).map((row) => ({ ...row }));
      const voiceTurns = config.voiceSessionsScenario?.turns ?? {};
      const persistedJobs = config.jobScenario?.persistJobs
        ? sessionStorage.getItem("mock-jobs")
        : null;
      const jobs: Array<Record<string, unknown>> = persistedJobs
        ? (JSON.parse(persistedJobs) as Array<Record<string, unknown>>)
        : (config.jobScenario?.seededJobs ?? []).map((job) => ({ ...job }));
      const persistJobs = () => {
        if (config.jobScenario?.persistJobs)
          sessionStorage.setItem("mock-jobs", JSON.stringify(jobs));
      };
      const liveJobIds = config.jobScenario?.liveJobIds ?? [];
      let retention: string | null =
        sessionStorage.getItem("mock-retention") ??
        config.retentionScenario?.value ??
        null;
      const exports: Array<Record<string, unknown>> = (
        config.seededExports ?? []
      ).map((row) => ({ ...row }));
      const readingCalls: {
        index: string[];
        chapter: string[];
        pages: number[][];
        ensure: number[][];
        rendered: number[];
        figures: string[][];
        clock: string[];
        pageMode: string[];
        followState: string[];
        tap: string[];
      } = {
        index: [],
        chapter: [],
        pages: [],
        ensure: [],
        rendered: [],
        figures: [],
        clock: [],
        pageMode: [],
        followState: [],
        tap: [],
      };
      const readingPages: Array<{
        pageNo: number;
        widthPt: number | null;
        heightPt: number | null;
        imagePath: string | null;
        imageDataUrl?: string;
      }> = (config.reading?.pages ?? []).map((entry) => ({ ...entry }));
      (window as unknown as Record<string, unknown>).__readingCalls =
        readingCalls;
      let voiceKeyStorage = config.voiceKeysScenario?.storage ?? {
        level: "os_keychain" as const,
        locked: false,
      };
      // The account is the slot-qualified store account — the base URL itself
      // for slot 0, `baseUrl#slot` otherwise — mirroring `slot_account` in
      // voice/keys/manager.rs. (Defined inside the init script: this whole
      // body is serialized into the page, so outer helpers do not exist here.)
      const voiceKeyViewOf = (entry: MockVoiceKeyEntry): MockVoiceKeyView => {
        const slot = entry.slot ?? 0;
        return {
          ...entry,
          slot,
          account: slot === 0 ? entry.baseUrl : `${entry.baseUrl}#${slot}`,
          label: entry.label ?? null,
          addedAt: Date.now(),
          lastUsed: null,
        };
      };
      const voiceKeyEntries: MockVoiceKeyView[] = (
        config.voiceKeysScenario?.entries ?? []
      ).map(voiceKeyViewOf);
      const voiceKeyConsents = new Set<string>(
        config.voiceKeysScenario?.consentedUrls ?? [],
      );
      let engineDown = config.readingScenario?.engineDown === true;
      (window as unknown as Record<string, unknown>).__mockSetEngineDown = (
        value: boolean,
      ) => {
        engineDown = value;
      };
      (window as unknown as Record<string, unknown>).__mockMarkExportStale = (
        path?: string,
      ) => {
        for (const row of exports) {
          if (path == null || row.path === path) row.stale = true;
        }
      };
      let lanSendSnapshot: {
        phase: string;
        pairedDevices: number;
        deviceNames: string[];
        pairLocked: boolean;
        acceptedConnections: number;
        casts: Array<{
          castId: string;
          title: string;
          bytesSent: number;
          bytesTotal: number;
          sentComplete: boolean;
          missing: boolean;
        }>;
      } | null = null;
      (window as unknown as Record<string, unknown>).__mockLanSendAdvance = (
        phase: "paired" | "sending" | "done",
      ) => {
        if (lanSendSnapshot == null) return;
        lanSendSnapshot.pairedDevices = 1;
        lanSendSnapshot.deviceNames = ["Test Phone"];
        lanSendSnapshot.acceptedConnections = 1;
        lanSendSnapshot.phase = phase;
        for (const cast of lanSendSnapshot.casts) {
          if (phase === "sending") {
            cast.bytesSent = Math.floor(cast.bytesTotal / 2);
          }
          if (phase === "done") {
            cast.bytesSent = cast.bytesTotal;
            cast.sentComplete = true;
          }
        }
        emitEvent("lan-send-progress", { ...lanSendSnapshot });
      };
      const createJobCalls: Array<Record<string, unknown>> = [];
      (window as unknown as Record<string, unknown>).__createJobCalls =
        createJobCalls;
      const installedModels: Array<Record<string, unknown>> = (
        config.modelScenario?.installed ??
        (config.modelScenario
          ? []
          : [{ modelId: "kokoro-82m", diskBytes: 355_000_000, sha256: "e2e" }])
      ).map((install) => ({ ...install }));
      const defaultModels: Record<string, string> = {};
      const activeDownloads = new Set<string>();
      let onboardingComplete =
        sessionStorage.getItem("mock-onboarding-complete") === "true" ||
        (config.onboardingComplete ?? true);
      let themePreference =
        sessionStorage.getItem("mock-theme-preference") ?? "system";
      const bytesPerGb = 1024 * 1024 * 1024;
      const defaultCacheCapBytes = 20 * bytesPerGb;
      let cacheCapConfigured: string | null =
        sessionStorage.getItem("mock-cache-cap") ?? config.cacheCapRaw ?? null;
      let personalizeEnabled =
        sessionStorage.getItem("mock-personalize-learning") !== "off";
      let learningPreferences = [...(config.learningPreferences ?? [])];
      const pagePeeks = config.pagePeeks ?? {};

      const fallbackHardware: MockHardwareProfile = {
        devices: [
          {
            key: "pci:0000:01:00.0",
            backend: "cuda",
            ordinal: 0,
            vendor: "nvidia",
            name: "Fixture GPU",
            memoryTotalMb: 12_288,
            memoryFreeMb: 12_000,
            unified: false,
            discrete: true,
            memorySource: "nvml",
          },
        ],
        primaryKey: "pci:0000:01:00.0",
        ram: { totalMb: 32_768, freeMb: 16_384 },
        cpu: { coresPhysical: 8, coresLogical: 16 },
        disk: { modelsFreeGb: 100 },
      };

      function mockFitBadge(
        model: MockCatalogModel,
        hardware: MockHardwareProfile | undefined,
      ): "green" | "yellow" | "orange" | "red" {
        if (model.badge) return model.badge;
        const primary = hardware?.devices.find(
          (device) => device.key === hardware.primaryKey,
        );
        const vramFree = primary?.memoryFreeMb ?? 0;
        const ramFree = hardware?.ram.freeMb ?? 0;
        const vramBudget = vramFree * 0.8;
        const gpuNeed = model.full_gpu_vram_mb ?? null;
        if (gpuNeed == null) {
          if (model.min_free_ram_mb <= ramFree) return "green";
          if (model.min_free_ram_mb <= ramFree + vramFree) return "orange";
          return "red";
        }
        if (vramBudget >= gpuNeed) return "green";
        if (model.hybrid_capable && vramBudget >= gpuNeed * 0.3)
          return "yellow";
        if (model.min_free_ram_mb <= ramFree) return "orange";
        if (model.min_free_ram_mb <= ramFree + vramFree) return "orange";
        return "red";
      }

      let activeVoiceSession = config.activeVoiceSession ?? null;
      const engineState: {
        status: unknown;
        variant: string | null;
        cancelled: boolean;
        offerError: string | null;
        failNext: string | null;
      } = (window as unknown as { __mockEngine?: never }).__mockEngine ?? {
        status: { state: "ready", port: 1 },
        variant: null,
        cancelled: false,
        offerError: null,
        failNext: null,
      };
      (window as unknown as { __mockEngine: typeof engineState }).__mockEngine =
        engineState;

      function engineInstallInfo() {
        const offer = (variant: string, download: number, installed: number) => ({
          variant,
          downloadBytes: download,
          installedBytes: installed,
          spaceNeededBytes: installed + download,
        });
        return {
          installed: null,
          requiredApi: 1,
          platform: "linux-x86_64",
          offered: engineState.offerError
            ? []
            : [offer("lite", 90_000_000, 220_000_000), offer("cpu", 1_000_000_000, 2_600_000_000)],
          engineVersion: "1.0.0",
          recommended: { variant: "cpu", reason: "no_nvidia_gpu", gpuName: null },
          spaceAvailableBytes: 50_000_000_000,
          offerError: engineState.offerError,
          installing: false,
          liteAvailable: !engineState.offerError,
        };
      }

      function startEngineInstall(variant: string) {
        engineState.cancelled = false;
        const steps: unknown[] = [
          { phase: "downloading", bytesDone: 45_000_000, bytesTotal: 90_000_000, speedBps: 1 },
          { phase: "verifying" },
          { phase: "extracting", filesDone: 12 },
          { phase: "starting" },
        ];
        const advance = (index: number) => {
          if (engineState.cancelled) {
            emitEvent("engine-install", {
              phase: "failed",
              code: "ERR_ENGINE_CANCELLED",
              message: "cancelled",
            });
            return;
          }
          if (engineState.failNext != null) {
            const code = engineState.failNext;
            engineState.failNext = null;
            emitEvent("engine-install", { phase: "failed", code, message: code });
            return;
          }
          if (index < steps.length) {
            emitEvent("engine-install", steps[index]);
            window.setTimeout(() => advance(index + 1), 150);
            return;
          }
          engineState.status = { state: "ready", port: 1 };
          engineState.variant = variant === "lite" ? "lite" : null;
          emitEvent("engine-install", { phase: "installed", engine: { variant } });
        };
        window.setTimeout(() => advance(0), 50);
        return null;
      }

      function emitEvent(event: string, payload: unknown): void {
        if (
          event === "voice-session-event" &&
          payload != null &&
          typeof payload === "object"
        ) {
          const row = payload as Record<string, unknown>;
          const detail = row.event as Record<string, unknown> | undefined;
          if (typeof row.session_id === "string") {
            const reader = document.querySelector(
              '[data-testid="reading-view"]',
            );
            const job = reader?.getAttribute("data-job-id") || null;
            payload = {
              ...row,
              purpose: row.purpose ?? "tutor",
              job_id:
                row.job_id === undefined
                  ? activeVoiceSession?.sessionId === row.session_id
                    ? activeVoiceSession.jobId
                    : job
                  : row.job_id,
            };
            if (detail?.kind === "ended" || detail?.kind === "failed") {
              if (activeVoiceSession?.sessionId === row.session_id)
                activeVoiceSession = null;
            } else if (
              activeVoiceSession == null ||
              row.session_id >= activeVoiceSession.sessionId
            ) {
              const enriched = payload as Record<string, unknown>;
              activeVoiceSession = {
                sessionId: row.session_id,
                bookId: typeof row.book_id === "string" ? row.book_id : null,
                jobId:
                  typeof enriched.job_id === "string" ? enriched.job_id : null,
                purpose: enriched.purpose === "agent" ? "agent" : "tutor",
              };
            }
          }
        }

        const ids = listeners.get(event);
        if (!ids) return;
        for (const id of ids) {
          callbacks.get(id)?.({ event, id, payload });
        }
      }

      (window as unknown as Record<string, unknown>).__mockEmitEvent = (
        event: string,
        payload: unknown,
      ) => emitEvent(event, payload);

      function sleep(ms: number): Promise<void> {
        return new Promise((resolve) => setTimeout(resolve, ms));
      }

      let staleOnNextRender = config.jobScenario?.staleExport === true;
      const originalTexts = new Map<string, string>();

      // C17 mirror of the worker's dispatch: one cast at a time, the oldest
      // QUEUED by the shared key `(createdAt, id)`; a running cast is never
      // pre-empted (only `resume_job` dispatches here; `create_job` keeps its
      // own start so existing scenarios are unchanged).
      const byQueueKey = (
        a: Record<string, unknown>,
        b: Record<string, unknown>,
      ): number =>
        (a.createdAt as number) !== (b.createdAt as number)
          ? (a.createdAt as number) - (b.createdAt as number)
          : String(a.id) < String(b.id)
            ? -1
            : String(a.id) > String(b.id)
              ? 1
              : 0;
      const RUNNING_STAGES = [
        "INGESTING",
        "LAYOUT",
        "SCRIPTING",
        "SYNTHESIZING",
        "ALIGNING",
        "RENDERING",
      ];
      function dispatchOldestQueued(): void {
        if (jobs.some((job) => RUNNING_STAGES.includes(String(job.state))))
          return;
        const next = jobs
          .filter((job) => job.state === "QUEUED")
          .sort(byQueueKey)[0];
        if (next) void runJobLifecycle(next);
      }
      async function runJobLifecycle(
        job: Record<string, unknown>,
      ): Promise<void> {
        const scenario = config.jobScenario;
        if (!scenario) return;
        const delay = scenario.stageDelayMs ?? 120;
        const stages = [
          "INGESTING",
          "LAYOUT",
          "SCRIPTING",
          "SYNTHESIZING",
          "ALIGNING",
          "RENDERING",
        ];
        for (const stage of stages) {
          if (job.state === "CANCELLED" || job.state === "PAUSED") return;
          job.state = stage;
          persistJobs();
          emitEvent("job-state", { job_id: job.id, state: stage });
          for (const pct of [0.3, 0.7, 1.0]) {
            if (job.state === "CANCELLED" || job.state === "PAUSED") return;
            emitEvent("job-progress", {
              job_id: job.id,
              stage,
              pct,
              eta_seconds: 90,
              current_item: `${stage.toLowerCase()} item ${Math.round(pct * 100)}`,
            });
            await sleep(delay);
          }
        }
        if (job.state === "CANCELLED" || job.state === "PAUSED") return;
        job.state = "DONE";
        persistJobs();
        const exportPath = scenario.outputPath ?? "/exports/fixture-book.mp3";
        job.outputPathsJson = JSON.stringify([exportPath]);
        if (scenario.reportJson && job.verify === true) {
          job.reportJson = scenario.reportJson;
        }
        const row = {
          id: `export-${job.id as string}`,
          bookId: job.bookId,
          jobId: job.id,
          path: exportPath,
          bytes: 5 * 1024 * 1024,
          format: "mp3",
          mode: job.mode ?? "narrator",
          stale: staleOnNextRender,
          createdAt: Date.now(),
        };
        staleOnNextRender = false;
        const existing = exports.findIndex(
          (entry) => entry.path === exportPath,
        );
        if (existing >= 0) exports[existing] = row;
        else exports.unshift(row);
        emitEvent("job-state", { job_id: job.id, state: "DONE" });
      }

      async function runDownloadLifecycle(
        model: Record<string, unknown>,
      ): Promise<void> {
        const scenario = config.modelScenario;
        if (!scenario) return;
        const modelId = model.id as string;
        const ticks = scenario.downloadTicks ?? 4;
        const delay = scenario.downloadDelayMs ?? 80;
        const bytesTotal = model.disk_bytes as number;
        for (let tick = 1; tick <= ticks; tick += 1) {
          if (!activeDownloads.has(modelId)) return;
          emitEvent("model-download-progress", {
            modelId,
            fileIndex: 0,
            filesTotal: 1,
            destPath: `${modelId}/weights.bin`,
            bytesDone: Math.round((bytesTotal * tick) / ticks),
            bytesTotal,
            state: "downloading",
          });
          await sleep(delay);
        }
        if (!activeDownloads.has(modelId)) return;
        if (scenario.downloadError) {
          activeDownloads.delete(modelId);
          emitEvent("model-download-finished", {
            state: "failed",
            modelId,
            error: scenario.downloadError,
          });
          return;
        }
        emitEvent("model-download-progress", {
          modelId,
          fileIndex: 0,
          filesTotal: 1,
          destPath: `${modelId}/weights.bin`,
          bytesDone: bytesTotal,
          bytesTotal,
          state: "verifying",
        });
        await sleep(delay);
        if (!activeDownloads.has(modelId)) return;
        activeDownloads.delete(modelId);
        installedModels.push({ modelId, diskBytes: bytesTotal });
        emitEvent("model-download-finished", { state: "installed", modelId });
      }

      function storageSettings() {
        const raw = cacheCapConfigured;
        let capSource: "default" | "unlimited" | "custom" = "default";
        let capBytes: number | null = defaultCacheCapBytes;
        let capRefused: string | null = null;
        if (raw !== null) {
          const i64Max = 9223372036854775807n;
          let parsed: bigint | null = null;
          if (/^[+-]?\d+$/.test(raw.trim())) {
            const candidate = BigInt(raw.trim());
            if (candidate <= i64Max && candidate >= -i64Max - 1n) {
              parsed = candidate;
            }
          }
          if (parsed === null) {
            capRefused = raw;
          } else if (parsed > 0n) {
            capSource = "custom";
            capBytes = Number(parsed);
          } else {
            capSource = "unlimited";
            capBytes = null;
          }
        }
        const models = "/mock/data/models";
        const exportsDir = "/mock/data/exports";
        return {
          models: {
            path: models,
            defaultPath: models,
            isDefault: true,
            configured: null,
            rejected: null,
          },
          exports: {
            path: exportsDir,
            defaultPath: exportsDir,
            isDefault: true,
            configured: null,
            rejected: null,
          },
          cacheDir: "/mock/data/cache",
          cacheBytes: 3 * bytesPerGb,
          cacheCapBytes: capBytes,
          cacheCapSource: capSource,
          cacheCapRefused: capRefused,
          defaultCacheCapBytes,
        };
      }

      const invokedCommands: string[] = [];
      // S-5 Providers: management-command payload capture (shape asserts),
      // pool-test cancel flag, and the fetched-model cache mirror.
      const providerCalls: Array<{ cmd: string; args: unknown }> = [];
      (window as unknown as Record<string, unknown>).__providerCalls =
        providerCalls;
      let poolTestCancelled = false;
      let poolTestRunning = false;
      // Consents are MUTABLE state, exactly like the backend store:
      // revocation stamps revoked_unix and the list read reflects it.
      const consentRows: CastConsentRow[] = (
        config.providersScenario?.consents ?? []
      ).map((row) => ({ ...row }));
      const setupCatalogs: Record<string, ProviderCatalog> = {};
      const cancelledSetups = new Set<string>();
      let mockCachedModels: {
        models: string[];
        suggestedDefault: string | null;
        fetchedUnix: number;
      } | null = null;
      (window as unknown as Record<string, unknown>).__invokedCommands =
        invokedCommands;
      // Arguments of every voice_session_start, for specs that pin what the
      // tutor is started with (read-only record; behavior is unchanged).
      const voiceStarts: Array<Record<string, unknown>> = [];
      (window as unknown as Record<string, unknown>).__voiceStarts =
        voiceStarts;

      async function invoke(
        cmd: string,
        args: Record<string, unknown> = {},
      ): Promise<unknown> {
        invokedCommands.push(cmd);
        callCounts[cmd] = (callCounts[cmd] ?? 0) + 1;
        if (cmd === "voice_session_start") voiceStarts.push({ ...args });
        switch (cmd) {
          case "voice_session_active":
            return activeVoiceSession;
          case "update_status":
            return updateState;
          case "update_check_now":
            return setUpdate({
              phase: updateState.available_version ? "available" : "up_to_date",
              last_checked_unix: 1_757_000_000,
              last_attempt_offline: false,
            });
          case "update_install": {
            void (async () => {
              for (const received of [250, 500, 1000]) {
                await sleep(40);
                if (updateState.phase !== "downloading") return;
                setUpdate({
                  received,
                  phase: received === 1000 ? "verifying" : "downloading",
                });
              }
              await sleep(40);
              setUpdate(
                config.updateScenario?.installOutcome === "failed"
                  ? {
                      phase: "failed",
                      error_code: "digest",
                      error_message: "artifact digest mismatch",
                    }
                  : { phase: "installing" },
              );
            })();
            return setUpdate({
              phase: "downloading",
              received: 0,
              total: 1000,
            });
          }
          case "update_cancel":
            return setUpdate({ phase: "available", received: 0, total: 0 });
          case "update_dismiss":
            return setUpdate({
              dismissed: true,
              phase:
                updateState.phase === "failed"
                  ? "available"
                  : updateState.phase,
              error_code: null,
              error_message: null,
            });
          case "plugin:opener|open_url":
            return null;
          case "app_version":
            return "0.1.0-e2e";
          // Book Agent (B-4): consent is BOOK-SCOPED (T8: another book's
          // grant authorizes nothing) and the grant redeems the displayed
          // connection identity, mirroring the backend contract.
          case "agent_session_probe": {
            if (config.bookAgentProbe === "notInBuild") {
              throw "Command agent_session_probe not found";
            }
            const probeBook = String(args.bookId ?? "");
            if (config.agentProbeState != null) {
              return { state: config.agentProbeState };
            }
            const grants = JSON.parse(
              sessionStorage.getItem("mock-agent-consents") ?? "{}",
            ) as Record<string, string>;
            return grants[probeBook] === "conn-gem-1"
              ? { state: "ready" }
              : {
                  state: "consent_required",
                  connection_id: "conn-gem-1",
                  connection_name: "Gemini",
                  hosts: ["generativelanguage.googleapis.com"],
                  data_categories: [
                    "source_text",
                    "voice_audio",
                    "voice_transcript",
                    "reading_position",
                  ],
                  retention_url: "https://ai.google.dev/gemini-api/terms",
                };
          }
          case "agent_consent_grant": {
            if (args.connectionId !== "conn-gem-1") {
              throw new Error(
                "the connection shown at the prompt is gone; re-open the consent prompt",
              );
            }
            const grants = JSON.parse(
              sessionStorage.getItem("mock-agent-consents") ?? "{}",
            ) as Record<string, string>;
            grants[String(args.bookId ?? "")] = String(args.connectionId);
            sessionStorage.setItem(
              "mock-agent-consents",
              JSON.stringify(grants),
            );
            return null;
          }
          case "agent_session_start":
            return null;
          case "agent_session_stop":
            return null;
          case "agent_highlight_ack": {
            const acks = ((
              window as unknown as Record<string, unknown>
            ).__agentAcks ??= []) as unknown[];
            (acks as unknown[]).push(args);
            return null;
          }
          case "get_storage_settings":
            return storageSettings();
          case "get_storage_breakdown": {
            const win = window as unknown as {
              __mockStorageBreakdown?: {
                totalBytes: number;
                engine: { status: string; diskBytes: number; path: string };
                models: Array<{
                  modelId: string;
                  name: string;
                  task: string;
                  diskBytes: number;
                  installedAt: number;
                }>;
                cache: { diskBytes: number; activeJobLocks: number };
                books: { count: number; diskBytes: number };
                exports: { count: number; diskBytes: number };
              };
            };
            if (!win.__mockStorageBreakdown) {
              win.__mockStorageBreakdown = {
                totalBytes: 104857600,
                engine: {
                  status: "ready",
                  diskBytes: 52428800,
                  path: "/mock/data/engine",
                },
                models: [
                  {
                    modelId: "kokoro-82m",
                    name: "Kokoro TTS",
                    task: "tts",
                    diskBytes: 31457280,
                    installedAt: 1700000000000,
                  },
                ],
                cache: {
                  diskBytes: 10485760,
                  activeJobLocks: 0,
                },
                books: {
                  count: 1,
                  diskBytes: 5242880,
                },
                exports: {
                  count: 1,
                  diskBytes: 5242880,
                },
              };
            }
            return win.__mockStorageBreakdown;
          }
          case "uninstall_engine": {
            const win = window as unknown as {
              __mockStorageBreakdown?: {
                engine: { status: string; diskBytes: number };
                totalBytes: number;
              };
            };
            if (win.__mockStorageBreakdown) {
              win.__mockStorageBreakdown.totalBytes -=
                win.__mockStorageBreakdown.engine.diskBytes;
              win.__mockStorageBreakdown.engine.status = "notInstalled";
              win.__mockStorageBreakdown.engine.diskBytes = 0;
            }
            return null;
          }
          case "purge_audio_cache": {
            const win = window as unknown as {
              __mockStorageBreakdown?: {
                cache: { diskBytes: number };
                totalBytes: number;
              };
            };
            const freed =
              win.__mockStorageBreakdown?.cache.diskBytes ?? 10485760;
            if (win.__mockStorageBreakdown) {
              win.__mockStorageBreakdown.totalBytes -= freed;
              win.__mockStorageBreakdown.cache.diskBytes = 0;
            }
            return freed;
          }
          case "danger_self_purge": {
            const confirmation = args.confirmation as string;
            if (confirmation !== "UNINSTALL") {
              throw "Confirmation string must be 'UNINSTALL'";
            }
            return null;
          }
          case "set_cache_cap_bytes": {
            const requested = args.capBytes as number | null;
            if (requested === null || requested === undefined) {
              cacheCapConfigured = null;
              sessionStorage.removeItem("mock-cache-cap");
              return storageSettings();
            }
            if (requested < 0) {
              throw `a cache cap cannot be negative; ${requested} would be read as no limit`;
            }
            cacheCapConfigured = String(requested);
            sessionStorage.setItem("mock-cache-cap", cacheCapConfigured);
            return storageSettings();
          }
          case "log_ui_layout":
            return null;
          case "log_ui_error":
            return null;
          case "get_onboarding_complete":
            return onboardingComplete;
          case "set_onboarding_complete":
            onboardingComplete = true;
            sessionStorage.setItem("mock-onboarding-complete", "true");
            return null;
          case "get_theme_preference":
            return themePreference;
          case "set_theme_preference": {
            const preference = args.preference as string;
            if (!["system", "light", "dark"].includes(preference)) {
              throw `unknown theme preference '${preference}'; expected system, light or dark`;
            }
            themePreference = preference;
            sessionStorage.setItem("mock-theme-preference", preference);
            return null;
          }
          case "host_locale":
            return config.hostLocale ?? null;
          case "get_locale_preference":
            return sessionStorage.getItem("mock-locale-preference") ?? "system";
          case "set_locale_preference": {
            const preference = args.preference as string;
            if (!["system", "en", "ar"].includes(preference)) {
              throw `unknown locale preference '${preference}'; expected system, en or ar`;
            }
            sessionStorage.setItem("mock-locale-preference", preference);
            return null;
          }
          case "list_learning_preferences":
            return learningPreferences;
          case "forget_learning_preference":
            learningPreferences = learningPreferences.filter(
              (entry) => entry.key !== args.key,
            );
            return learningPreferences;
          case "get_learning_privacy":
            return { personalizeEnabled, neverPersist: retention === "never" };
          case "set_personalize_learning": {
            personalizeEnabled = args.enabled as boolean;
            sessionStorage.setItem(
              "mock-personalize-learning",
              personalizeEnabled ? "on" : "off",
            );
            return { personalizeEnabled, neverPersist: retention === "never" };
          }
          case "voice_retention_get": {
            if (config.retentionScenario?.unreadable)
              throw "retention setting unreadable";
            if (retention == null) {
              retention = "keep";
              sessionStorage.setItem("mock-retention", retention);
            }
            return retention;
          }
          case "voice_retention_set": {
            if (config.retentionScenario?.unreadable)
              throw "retention setting unreadable";
            const value = args.value as string;
            if (value !== "keep" && value !== "never")
              throw 'retention must be "keep" or "never"';
            retention = value;
            sessionStorage.setItem("mock-retention", retention);
            return retention;
          }
          case "set_window_background":
            return null;
          case "set_window_chrome":
            (window as unknown as Record<string, unknown>).__windowChrome =
              args.palette;
            return null;
          case "reveal_main_window":
            return null;
          case "engine_status":
            return engineState.status;
          case "engine_active_variant":
            return engineState.variant;
          case "engine_install_info":
            return engineInstallInfo();
          case "install_engine":
            return startEngineInstall(String(args.variant));
          case "cancel_engine_install":
            engineState.cancelled = true;
            return true;
          case "startup_failure":
            return (
              (window as unknown as { __mockStartupFailure?: unknown })
                .__mockStartupFailure ?? null
            );
          case "list_voice_providers":
            return [
              {
                id: "gemini",
                defaultBaseUrl: "https://generativelanguage.googleapis.com",
                capabilities: ["llm"],
              },
              {
                id: "deepgram",
                defaultBaseUrl: "https://api.deepgram.com",
                capabilities: ["stt"],
              },
              {
                id: "cartesia",
                defaultBaseUrl: "https://api.cartesia.ai",
                capabilities: ["tts"],
              },
              {
                id: "openai_compat",
                defaultBaseUrl: null,
                capabilities: ["llm"],
              },
            ];
          case "provider_directory":
            return config.providerDirectory ?? [];
          case "provider_cached_catalog":
            return setupCatalogs[String(args.providerId)] ?? null;
          case "provider_cancel_setup":
            cancelledSetups.add(String(args.operationId));
            return null;
          case "provider_connect":
          case "provider_refresh_catalog": {
            providerCalls.push({ cmd, args: { providerId: args.providerId } });
            const definition = config.providerDirectory?.find(
              (entry) => entry.id === args.providerId,
            );
            if (!definition) throw "PROVIDER_INVALID_CONFIGURATION";
            if (config.providersScenario?.setupError)
              throw config.providersScenario.setupError;
            if (config.providersScenario?.setupDelayMs)
              await sleep(config.providersScenario.setupDelayMs);
            if (cancelledSetups.has(String(args.operationId)))
              throw "PROVIDER_CANCELLED";
            if (
              cmd === "provider_connect" &&
              !definition.local &&
              !String(args.key ?? "").trim()
            )
              throw "PROVIDER_INVALID_KEY";
            const models = [
              ...definition.preferredScript,
              ...definition.preferredNarration,
            ].map((id) => ({
              id,
              name: id,
              script: definition.preferredScript.includes(id)
                ? ("supported" as const)
                : ("unsupported" as const),
              vision:
                definition.preferredScript.includes(id) &&
                definition.id !== "groq"
                  ? ("supported" as const)
                  : ("unsupported" as const),
              narration: definition.preferredNarration.includes(id)
                ? ("supported" as const)
                : ("unsupported" as const),
              active: true,
              preview: false,
              contextWindow: null,
              outputLimit: null,
              supportedParameters: [],
              retirement: null,
              source: definition.documentationUrl,
            }));
            const catalog: ProviderCatalog = {
              providerId: definition.id,
              models,
              voices: definition.voices.map((id) => ({
                id,
                name: id,
                language: null,
              })),
              recommendations: {
                script: definition.preferredScript[0] ?? null,
                narration: definition.preferredNarration[0] ?? null,
                voice: definition.voices[0] ?? null,
              },
              revision:
                cmd === "provider_connect" ? "connected-v1" : "refreshed-v2",
              fetchedUnix: Math.floor(Date.now() / 1000),
              complete: true,
              nextCursor: null,
              voicesComplete: true,
              credentialRequired: !definition.local || !!args.key,
            };
            catalog.configured = setupCatalogs[definition.id]?.configured ?? {
              ...catalog.recommendations,
            };
            setupCatalogs[definition.id] = catalog;
            if (cmd === "provider_connect") {
              config.providersScenario ??= { families: [] };
              let family = config.providersScenario.families.find(
                (entry) => entry.id === definition.custodyId,
              );
              if (!family) {
                family = {
                  id: definition.custodyId,
                  displayName: definition.name,
                  capabilities: [],
                  card: { kind: "healthy" },
                  counts: {
                    working: 1,
                    checking: 0,
                    unverified: 0,
                    invalid: 0,
                    rate_limited: 0,
                    unreachable: 0,
                    missing_bytes: 0,
                    disabled: 0,
                  },
                  configuredKeys: 1,
                  enabledKeys: 1,
                  connections: [],
                  disabledConnections: 0,
                  activeModels: [],
                  vaultLocked: false,
                  retentionLinks: [],
                  pools: [],
                };
                config.providersScenario.families.push(family);
              }
              if (
                definition.providerType &&
                !family.connections.some(
                  (entry) => entry.id === `setup-${definition.id}`,
                )
              ) {
                family.connections.push({
                  id: `setup-${definition.id}`,
                  name: definition.name,
                  providerType: definition.providerType,
                  hostname: null,
                  baseUrl:
                    definition.providerType === "openai_compatible"
                      ? definition.baseUrl
                      : null,
                  defaultModel:
                    catalog.recommendations.script ??
                    catalog.recommendations.narration ??
                    "fixture",
                  capabilities: catalog.recommendations.script
                    ? ["script_llm"]
                    : ["tts"],
                  keyRequired: !definition.local,
                  enabled: true,
                  keyPresent: true,
                  keyLast4: "1234",
                });
              }
              family.pools ??= [];
              if (
                !family.pools.some(
                  (pool) => pool.baseUrl === definition.baseUrl,
                )
              )
                family.pools.push({
                  baseUrl: definition.baseUrl!,
                  label: { kind: "healthy" },
                  counts: { ...family.counts },
                  configured: 1,
                  operational: 1,
                  voiceTutorBuiltIn: false,
                  slots: [],
                });
            }
            return catalog;
          }
          case "provider_apply_recommendations":
            providerCalls.push({ cmd, args: { ...args } });
            if (
              setupCatalogs[String(args.providerId)]?.revision !== args.revision
            )
              throw "PROVIDER_CATALOG_CHANGED";
            return null;
          case "providers_overview": {
            const hooks = window as unknown as {
              __failNextOverview?: boolean;
            };
            if (hooks.__failNextOverview === true) {
              hooks.__failNextOverview = false;
              throw "connection store: unavailable";
            }
            return config.providersScenario?.families ?? [];
          }
          case "provider_slot_set_disabled":
            return null;
          case "provider_pool_test": {
            // Typed progress stream, backend-shaped (PoolTestProgress):
            // initial event, one per probe, terminal done event —
            // cancellable between probes like the real sequential runner.
            const provider = String(args.provider);
            const baseUrl = String(args.baseUrl);
            const total = config.providersScenario?.poolTestTotal ?? 0;
            // The real command refuses a second concurrent run for the
            // same pool instead of clobbering the first run's truth.
            if (poolTestRunning) {
              throw `a pool test is already running for ${provider} at ${baseUrl}; wait for it to finish or cancel it first`;
            }
            poolTestRunning = true;
            poolTestCancelled = false;
            const progress = (
              completed: number,
              done: boolean,
              cancelled: boolean,
            ) =>
              emitEvent("provider-pool-test-progress", {
                provider,
                baseUrl,
                completed,
                total,
                done,
                cancelled,
              });
            progress(0, false, false);
            let completed = 0;
            for (; completed < total; completed += 1) {
              if (poolTestCancelled) break;
              await sleep(40);
              progress(completed + 1, false, false);
            }
            // Terminal accounting is truthful: exactly how many probes
            // settled — a cancelled run never reports `total`.
            poolTestRunning = false;
            progress(completed, true, poolTestCancelled);
            return null;
          }
          case "provider_pool_test_cancel":
            poolTestCancelled = true;
            return null;
          case "provider_connection_delete_impact": {
            const impact = config.providersScenario?.deleteImpact;
            if (impact === null) throw "connection store: unavailable";
            return (
              impact ?? { referencingJobs: [], referencingVoiceProfiles: [] }
            );
          }
          case "provider_connection_create":
          case "provider_connection_update": {
            // The backend validation the real command runs: fixed
            // providers refuse a base URL, custom requires one.
            providerCalls.push({ cmd, args: { ...args } });
            const fixedKinds = new Set([
              "gemini",
              "anthropic",
              "cartesia_tts",
              "deepgram",
            ]);
            const kind =
              cmd === "provider_connection_create"
                ? String(args.providerType)
                : ((
                    (config.providersScenario?.families ?? []) as Array<{
                      connections?: Array<{ id: string; providerType: string }>;
                    }>
                  )
                    .flatMap((family) => family.connections ?? [])
                    .find((row) => row.id === args.id)?.providerType ?? "");
            if (fixedKinds.has(kind) && args.baseUrl != null) {
              throw "this provider's endpoint is fixed and not editable";
            }
            if (!fixedKinds.has(kind) && args.baseUrl == null) {
              throw "an OpenAI-compatible connection needs a base URL";
            }
            if (String(args.defaultModel ?? "").trim() === "") {
              throw "the connection needs a default model";
            }
            return { id: args.id ?? "c-new" };
          }
          case "provider_connection_delete":
            providerCalls.push({ cmd, args: { ...args } });
            return null;
          case "provider_cached_models":
            return (
              mockCachedModels ?? config.providersScenario?.cachedModels ?? null
            );
          case "create_cloud_voice_profile": {
            providerCalls.push({ cmd, args: { ...args } });
            if (String(args.name ?? "").trim() === "")
              throw "Give this voice a name so you can find it later.";
            if (String(args.voiceId ?? "").trim() === "")
              throw "Enter the provider's voice id.";
            return {
              id: "vp-cloud-1",
              name: args.name,
              engine: "cloud",
              kind: "preset",
              preset_voice_id: args.voiceId,
            };
          }
          case "import_voice_keys_text": {
            providerCalls.push({ cmd, args: { ...args } });
            const lines = String(args.text)
              .split("\n")
              .map((line) => line.trim())
              .filter((line) => line !== "" && !line.startsWith("#"));
            const custom = ![
              "gemini",
              "anthropic",
              "cartesia",
              "deepgram",
            ].includes(String(args.provider));
            if (custom) {
              if (args.baseUrl == null)
                throw "an OpenAI-compatible key needs its base URL";
              const canonical = String(args.baseUrl).replace(/\/+$/, "");
              if (!voiceKeyConsents.has(canonical))
                throw "this endpoint has not been consented to yet";
            } else if (args.baseUrl != null) {
              throw "this provider's endpoint is fixed; only OpenAI-compatible keys take a custom base URL";
            }
            return { imported: lines, rejected: [], duplicates: [] };
          }
          case "cast_consent_list": {
            const hooks = window as unknown as {
              __failNextConsentList?: boolean;
            };
            if (hooks.__failNextConsentList === true) {
              hooks.__failNextConsentList = false;
              throw "consent store: unavailable";
            }
            return consentRows.map((row) => ({ ...row }));
          }
          case "cast_consent_revoke": {
            if (
              config.providersScenario?.revokeFails?.includes(
                String(args.consentId),
              )
            ) {
              throw "consent store: the revocation could not be recorded";
            }
            const row = consentRows.find(
              (entry) => entry.id === args.consentId,
            );
            if (row == null) throw `no consent ${String(args.consentId)}`;
            row.revoked_unix = Math.floor(Date.now() / 1000);
            return true;
          }
          case "voice_tutor_readiness": {
            const entries = voiceKeyEntries.filter(
              (entry) =>
                entry.provider === "gemini" &&
                entry.baseUrl.replace(/\/$/, "") ===
                  "https://generativelanguage.googleapis.com",
            );
            if (entries.length === 0) return "missing";
            if (voiceKeyStorage.locked) return "locked";
            return entries.some(
              (entry) =>
                entry.keyPresent &&
                (entry.state === "active" || entry.state === "unchecked"),
            )
              ? "present"
              : "unavailable";
          }
          case "voice_key_storage_status":
            return { ...voiceKeyStorage };
          case "unlock_voice_key_vault": {
            const expected =
              config.voiceKeysScenario?.unlockPassphrase ?? "letmein";
            if (args.passphrase !== expected) {
              throw "key vault operation failed: wrong passphrase, or the vault file was modified";
            }
            voiceKeyStorage = { level: "encrypted_file", locked: false };
            return { ...voiceKeyStorage };
          }
          case "use_session_only_voice_keys":
            if (voiceKeyStorage.level === "os_keychain") {
              throw "the OS keychain is available, so lower-ranked storage cannot be selected";
            }
            voiceKeyStorage = { level: "session_only", locked: false };
            return { ...voiceKeyStorage };
          case "voice_session_enter_interactive":
          case "voice_session_leave_interactive":
            // The mode change itself rides runtime events (`mode`), which
            // specs emit; the command only has to be accepted.
            return null;
          case "voice_session_list": {
            const bookId = (args.bookId as string | null | undefined) ?? null;
            return voiceSessions
              .filter((row) => row.book_id === bookId)
              .map((row) => ({ ...row }));
          }
          case "voice_session_transcript": {
            const row = voiceSessions.find(
              (entry) => entry.id === args.sessionId,
            );
            if (row == null) throw "session not found";
            return {
              row: { ...row },
              turns: (voiceTurns[row.id] ?? []).map((turn) => ({ ...turn })),
            };
          }
          case "voice_page_peek": {
            (
              window as unknown as { __pagePeekCalls?: unknown[] }
            ).__pagePeekCalls = [
              ...((window as unknown as { __pagePeekCalls?: unknown[] })
                .__pagePeekCalls ?? []),
              { ...args },
            ];
            return pagePeeks[args.pageNo as number] ?? null;
          }
          case "voice_session_delete": {
            const at = voiceSessions.findIndex(
              (entry) => entry.id === args.sessionId,
            );
            if (at < 0) throw "session not found";
            voiceSessions.splice(at, 1);
            return null;
          }
          case "voice_session_forget_scope": {
            const bookId = (args.bookId as string | null | undefined) ?? null;
            const before = voiceSessions.length;
            for (let at = voiceSessions.length - 1; at >= 0; at -= 1) {
              if (voiceSessions[at].book_id === bookId) {
                voiceSessions.splice(at, 1);
              }
            }
            return before - voiceSessions.length;
          }
          case "job_delete_preview":
            return {
              session_count: voiceSessions.filter(
                (row) => row.job_id === args.jobId,
              ).length,
              live: liveJobIds.includes(String(args.jobId)),
            };
          case "list_voice_keys":
            return voiceKeyEntries.map((entry) => ({ ...entry }));
          case "probe_voice_keys": {
            const probed = config.voiceKeysScenario?.probedStates ?? {};
            for (const entry of voiceKeyEntries) {
              const next = probed[entry.baseUrl];
              if (next && entry.keyPresent) entry.state = next;
            }
            return voiceKeyEntries.map((entry) => ({ ...entry }));
          }
          case "refresh_voice_key": {
            // The argument is the row's slot-qualified account, matched
            // exactly — the same contract as KeyManager::refresh_one.
            const entry = voiceKeyEntries.find(
              (candidate) => candidate.account === args.baseUrl,
            );
            if (!entry) throw "no key is configured for that endpoint";
            const next =
              config.voiceKeysScenario?.refreshedStates?.[entry.baseUrl] ??
              config.voiceKeysScenario?.probedStates?.[entry.baseUrl];
            if (next && entry.keyPresent) entry.state = next;
            return { ...entry };
          }
          case "check_voice_url": {
            const canonical = String(args.url).replace(/\/+$/, "");
            return {
              canonicalUrl: canonical,
              consented: voiceKeyConsents.has(canonical),
            };
          }
          case "record_voice_url_consent": {
            const canonical = String(args.url).replace(/\/+$/, "");
            voiceKeyConsents.add(canonical);
            return canonical;
          }
          case "add_voice_key": {
            const provider = String(args.provider);
            const key = String(args.key);
            if (key.trim() === "") throw "the key is empty";
            const fixed: Record<string, string> = {
              gemini: "https://generativelanguage.googleapis.com",
              deepgram: "https://api.deepgram.com",
              cartesia: "https://api.cartesia.ai",
            };
            let baseUrl: string;
            let custom = false;
            if (provider in fixed) {
              if (args.baseUrl != null) {
                throw "this provider's endpoint is fixed; only OpenAI-compatible keys take a custom base URL";
              }
              baseUrl = fixed[provider]!;
            } else {
              if (args.baseUrl == null)
                throw "an OpenAI-compatible key needs its base URL";
              baseUrl = String(args.baseUrl).replace(/\/+$/, "");
              if (!voiceKeyConsents.has(baseUrl)) {
                throw "this endpoint has not been consented to yet";
              }
              custom = true;
            }
            const entry = voiceKeyViewOf({
              baseUrl,
              provider,
              last4: key.slice(-4),
              state: "unchecked",
              keyPresent: true,
              custom,
            });
            const at = voiceKeyEntries.findIndex(
              (candidate) => candidate.account === entry.account,
            );
            if (at >= 0) voiceKeyEntries[at] = entry;
            else voiceKeyEntries.push(entry);
            return { ...entry };
          }
          case "list_cast_key_pool": {
            const capabilityArms: Record<string, Array<"llm" | "tts">> = {
              gemini: ["llm"],
              deepgram: [],
              cartesia: ["tts"],
              openai_compat: ["llm"],
            };
            const grouped = { llm: [], tts: [] } as Record<
              "llm" | "tts",
              Array<Record<string, unknown>>
            >;
            for (const entry of voiceKeyEntries) {
              for (const arm of capabilityArms[entry.provider] ?? []) {
                grouped[arm].push({
                  baseUrl: entry.baseUrl,
                  provider: entry.provider,
                  state: entry.state,
                  keyPresent: entry.keyPresent,
                });
              }
            }
            return grouped;
          }
          case "remove_voice_key": {
            const at = voiceKeyEntries.findIndex(
              (candidate) =>
                candidate.baseUrl === args.baseUrl &&
                candidate.slot === (args.slot ?? 0),
            );
            if (at < 0) throw "no key is configured for that endpoint";
            voiceKeyEntries.splice(at, 1);
            return null;
          }
          case "list_books":
            return [...books];
          case "plugin:event|listen": {
            const event = args.event as string;
            const handler = args.handler as number;
            const set = listeners.get(event) ?? new Set<number>();
            set.add(handler);
            listeners.set(event, set);
            return nextId++;
          }
          case "plugin:event|unlisten":
            return null;
          case "plugin:dialog|open": {
            const filters = (
              args.options as { filters?: Array<{ name: string }> } | undefined
            )?.filters;
            if (filters?.some((filter) => filter.name === "Audio")) {
              return config.jobScenario?.cloneScenario?.importPath ?? null;
            }
            if (config.importScenario?.pickerError != null) {
              throw config.importScenario.pickerError;
            }
            return config.importScenario?.path ?? null;
          }
          case "plan_import": {
            const scenario = config.importScenario;
            if (!scenario) throw "unexpected plan_import call";
            return (
              scenario.plan ?? {
                pageCount: 20,
                reflowed: false,
                layoutAvailable: false,
                layoutSeconds: 0,
                onGpu: true,
                silent: true,
                recommendLayout: false,
              }
            );
          }
          case "import_book": {
            const scenario = config.importScenario;
            if (!scenario) throw "unexpected import_book call";
            importedWithLayout = args.layout as boolean;
            (
              window as unknown as Record<string, unknown>
            ).__importedWithLayout = importedWithLayout;
            const delay = scenario.progressDelayMs ?? 100;
            for (const event of scenario.progress ?? []) {
              emitEvent("import-progress", event);
              await new Promise((resolve) => setTimeout(resolve, delay));
            }
            if (scenario.error) throw scenario.error;
            if (!scenario.book) throw "import scenario missing book";
            emitEvent("import-progress", { type: "done" });
            books.unshift(scenario.book);
            return { book: scenario.book, warnings: scenario.warnings ?? [] };
          }
          case "plan_extraction": {
            const scenario = config.extractionScenario;
            if (!books.some((book) => book.id === args.bookId)) {
              throw `ERR_NO_BOOK: book '${String(args.bookId)}' is not in the library`;
            }
            return {
              ...(scenario?.plan ?? {
                pageCount: 613,
                reflowed: false,
                layoutAvailable: true,
                layoutSeconds: 6982,
                cpuSeconds: 40,
                offerBoth: true,
                onGpu: false,
                silent: false,
                recommendLayout: false,
              }),
              blocked: scenario?.blocked ?? null,
            };
          }
          case "improve_extraction": {
            const scenario = config.extractionScenario;
            const target = books.findIndex((book) => book.id === args.bookId);
            if (target < 0) {
              throw `ERR_NO_BOOK: book '${String(args.bookId)}' is not in the library`;
            }
            (window as unknown as Record<string, unknown>).__improvedBookId =
              args.bookId;
            const layoutRequested = args.layout !== false;
            (
              window as unknown as Record<string, unknown>
            ).__improvedWithLayout = layoutRequested;
            const delay = scenario?.progressDelayMs ?? 20;
            for (const event of scenario?.progress ?? []) {
              emitEvent("improve-extraction-progress", event);
              await new Promise((resolve) => setTimeout(resolve, delay));
            }
            if (scenario?.hangUntilStopped) {
              while (!stopped.has(String(args.bookId))) {
                await new Promise((resolve) => setTimeout(resolve, 20));
              }
              emitEvent("improve-extraction-finished", { bookId: args.bookId });
              throw "ERR_CANCELLED: re-extraction stopped";
            }
            if (scenario?.runError) throw scenario.runError;
            const warnings = scenario?.runWarnings ?? [];
            const layoutPassRan = !layoutRequested
              ? false
              : (scenario?.layoutPassRan ??
                !warnings.some((warning) => warning.startsWith("layout_")));
            if (layoutPassRan) {
              books[target] = { ...books[target]!, layoutAnalysed: true };
            }
            emitEvent("improve-extraction-finished", { bookId: args.bookId });
            return {
              pages: scenario?.plan?.pageCount ?? 613,
              blocksVersion: "extract=2;roles=1",
              warnings,
              layoutPassRan,
            };
          }
          case "active_extraction": {
            const running = config.extractionScenario?.activeOnMount ?? null;
            if (running && config.extractionScenario?.finishBeforeRestore) {
              emitEvent("improve-extraction-finished", {
                bookId: running.bookId,
              });
            }
            return running;
          }
          case "cancel_extraction": {
            stopped.add(String(args.bookId));
            if (config.extractionScenario?.finishOnStop) {
              emitEvent("improve-extraction-finished", { bookId: args.bookId });
            }
            return true;
          }
          case "delete_book": {
            if (config.extractionScenario?.deleteError) {
              throw config.extractionScenario.deleteError;
            }
            const index = books.findIndex((book) => book.id === args.bookId);
            if (index < 0) throw `book '${String(args.bookId)}' not found`;
            books.splice(index, 1);
            for (let i = exports.length - 1; i >= 0; i -= 1) {
              if (exports[i]!.bookId === args.bookId) exports.splice(i, 1);
            }
            return null;
          }
          case "get_chapters":
            return config.jobScenario?.chapters ?? [];
          case "list_audio_inputs": {
            const clone = config.jobScenario?.cloneScenario;
            if (clone?.audioInputsError) throw clone.audioInputsError;
            return (
              clone?.audioInputs ?? [
                { name: "Built-in Microphone", isDefault: true },
              ]
            );
          }
          case "start_voice_recording": {
            const clone = config.jobScenario?.cloneScenario;
            if (clone?.recordError) throw clone.recordError;
            recordingSampleId = `sample-${voiceSamples.length + 1}`;
            return recordingSampleId;
          }
          case "stop_voice_recording": {
            const clone = config.jobScenario?.cloneScenario;
            if (recordingSampleId == null) throw "No recording is running.";
            const sampleId = recordingSampleId;
            recordingSampleId = null;
            voiceSamples.push(sampleId);
            return {
              sampleId,
              seconds: clone?.recordSeconds ?? 6.4,
              hitHardStop: clone?.hitHardStop ?? false,
            };
          }
          case "cancel_voice_recording":
            if (recordingSampleId != null)
              cancelledRecordings.push(recordingSampleId);
            recordingSampleId = null;
            return null;
          case "__cancelled_recordings":
            return [...cancelledRecordings];
          case "import_voice_sample": {
            const clone = config.jobScenario?.cloneScenario;
            if (clone?.importError) throw clone.importError;
            const sampleId = `sample-${voiceSamples.length + 1}`;
            voiceSamples.push(sampleId);
            return {
              sampleId,
              seconds: clone?.importSeconds ?? 8.1,
              hitHardStop: false,
            };
          }
          case "discard_voice_sample": {
            const index = voiceSamples.indexOf(String(args.sampleId));
            if (index >= 0) voiceSamples.splice(index, 1);
            return null;
          }
          case "preview_voice_sample": {
            const clone = config.jobScenario?.cloneScenario;
            if (clone?.previewError) throw clone.previewError;
            if (!voiceSamples.includes(String(args.sampleId))) {
              throw "that recording is no longer available";
            }
            return "/cache/previews/clone-preview.wav";
          }
          case "save_voice_profile": {
            const clone = config.jobScenario?.cloneScenario;
            if (clone?.saveError) throw clone.saveError;
            const sampleId = String(args.sampleId);
            if (!voiceSamples.includes(sampleId)) {
              throw "that recording is no longer available";
            }
            voiceSamples.splice(voiceSamples.indexOf(sampleId), 1);
            const saved: MockVoiceProfile = {
              id: `cloned-${sampleId}`,
              name: String(args.name),
              engine: "chatterbox-0.5b",
              kind: "cloned",
              presetVoiceId: null,
            };
            savedVoiceProfiles.push(saved);
            return saved;
          }
          case "delete_voice_profile": {
            const index = savedVoiceProfiles.findIndex(
              (p) => p.id === args.voiceProfileId,
            );
            if (index < 0) throw "voice profile not found";
            savedVoiceProfiles.splice(index, 1);
            return null;
          }
          case "list_voice_profiles":
            return [
              ...(config.jobScenario?.voiceProfiles ?? [
                {
                  id: "preset-kokoro-af-heart",
                  name: "Heart · American English (female)",
                  engine: "kokoro-82m",
                  kind: "preset",
                  presetVoiceId: "af_heart",
                },
              ]),
              ...savedVoiceProfiles,
            ];
          case "preview_voice": {
            if (config.jobScenario?.previewError) {
              throw config.jobScenario.previewError;
            }
            return "/cache/previews/abcdef-preview.wav";
          }
          case "get_media_url": {
            const wanted = String(args.path ?? "");
            const row = exports.find((entry) => entry.path === wanted);
            if (row?.stale === true) {
              throw "this audio no longer matches the book's chapters; re-render it before playing";
            }
            const served = config.readingScenario?.mediaDataUrl;
            if (served != null) return served;
            return `http://127.0.0.1:9/media/mock-${wanted.replace(/\//g, "-")}?t=mock`;
          }
          case "get_reading_index": {
            readingCalls.index.push(String(args.jobId ?? ""));
            const payload = config.reading?.index;
            if (!payload) throw "no reading payload in this scenario";
            return payload;
          }
          case "get_reading_chapter": {
            const chapterId = String(args.chapterId ?? "");
            readingCalls.chapter.push(chapterId);
            const found = (config.reading?.chapters ?? []).find(
              (entry) => entry.chapterId === chapterId,
            );
            if (!found) throw `chapter '${chapterId}' has no audio in this job`;
            return found;
          }
          case "get_reading_pages": {
            const wanted = (args.pages ?? []) as number[];
            readingCalls.pages.push([...wanted]);
            if (wanted.length > 64) {
              throw `a reading request asked for ${wanted.length} items; the view fetches a page and its neighbour, so anything past 64 is a caller defect rather than a workload`;
            }
            return readingPages.filter((entry) =>
              wanted.includes(entry.pageNo),
            );
          }
          case "ensure_reading_pages": {
            const wanted = (args.pages ?? []) as number[];
            readingCalls.ensure.push([...wanted]);
            if (wanted.length > 64) {
              throw `a reading request asked for ${wanted.length} items; the view fetches a page and its neighbour, so anything past 64 is a caller defect rather than a workload`;
            }
            const rows = readingPages.filter((entry) =>
              wanted.includes(entry.pageNo),
            );
            if (!engineDown) {
              for (const row of rows) {
                if (row.imagePath != null) continue;
                const served = (config.reading?.rendered ?? []).find(
                  (entry) => entry.pageNo === row.pageNo,
                );
                if (served?.imagePath == null) continue;
                row.imagePath = served.imagePath;
                readingCalls.rendered.push(row.pageNo);
              }
            }
            return rows;
          }
          case "get_reading_figures": {
            const wanted = (args.figureIds ?? []) as string[];
            readingCalls.figures.push([...wanted]);
            return (config.reading?.figures ?? []).filter((entry) =>
              wanted.includes(entry.figureId),
            );
          }
          case "list_suppressed_blocks":
            return config.suppressedBlocks ?? [];
          case "log_reading_clock":
            readingCalls.clock.push(String(args.line ?? ""));
            return null;
          case "log_page_mode":
            readingCalls.pageMode.push(String(args.line ?? ""));
            return null;
          case "log_follow_state":
            readingCalls.followState.push(String(args.line ?? ""));
            return null;
          case "log_reading_tap":
            readingCalls.tap.push(String(args.line ?? ""));
            return null;
          case "get_hardware_profile":
            return config.modelScenario?.hardware ?? fallbackHardware;
          case "get_compute_estimate":
            return (
              config.modelScenario?.computeEstimate ?? {
                computeScore: 60,
                band: "MEDIUM",
                tokSEst: 30,
              }
            );
          case "get_catalog":
            if (config.modelScenario) {
              return {
                catalog_version: "2026.07-e2e",
                models: config.modelScenario.catalogModels.map((model) => ({
                  ...model,
                  badge: mockFitBadge(
                    model,
                    config.modelScenario?.hardware ?? fallbackHardware,
                  ),
                })),
              };
            }
            return {
              models: [
                {
                  id: "kokoro-82m",
                  task: "tts",
                  perf: { rtf_gpu: 0.03, rtf_cpu: 1.0 },
                  license: { spdx: "Apache-2.0", commercial_ok: true },
                  badge: "green",
                },
              ],
            };
          case "list_installed_models":
            return installedModels.map((install) => ({ ...install }));
          case "get_default_models":
            return { ...defaultModels };
          case "set_default_model": {
            const task = args.task as string;
            const modelId = args.modelId as string | null;
            if (modelId == null) {
              delete defaultModels[task];
            } else {
              defaultModels[task] = modelId;
            }
            return null;
          }
          case "delete_model": {
            const index = installedModels.findIndex(
              (install) => install.modelId === args.modelId,
            );
            if (index >= 0) {
              installedModels.splice(index, 1);
            }
            const win = window as unknown as {
              __mockStorageBreakdown?: {
                totalBytes: number;
                models: Array<{ modelId: string; diskBytes: number }>;
              };
            };
            if (win.__mockStorageBreakdown) {
              const bIdx = win.__mockStorageBreakdown.models.findIndex(
                (m) => m.modelId === args.modelId,
              );
              if (bIdx >= 0) {
                const [removed] = win.__mockStorageBreakdown.models.splice(
                  bIdx,
                  1,
                );
                win.__mockStorageBreakdown.totalBytes -= removed.diskBytes;
              }
            } else if (index < 0) {
              throw `model '${String(args.modelId)}' not installed`;
            }
            return null;
          }
          case "download_model": {
            const scenario = config.modelScenario;
            if (!scenario) throw "unexpected download_model call";
            const model = scenario.catalogModels.find(
              (entry) => entry.id === args.modelId,
            );
            if (!model) throw `model '${String(args.modelId)}' not found`;
            if (activeDownloads.has(model.id)) {
              throw `download already in progress for '${model.id}'`;
            }
            activeDownloads.add(model.id);
            void runDownloadLifecycle(
              model as unknown as Record<string, unknown>,
            );
            return null;
          }
          case "cancel_model_download": {
            const modelId = args.modelId as string;
            if (!activeDownloads.has(modelId)) {
              throw `no active download for '${modelId}'`;
            }
            activeDownloads.delete(modelId);
            emitEvent("model-download-finished", {
              state: "failed",
              modelId,
              error: "download cancelled",
            });
            return null;
          }
          case "estimate_job_eta": {
            const request = args.request as { words: number };
            const profile = config.modelScenario?.hardware ?? fallbackHardware;
            const primary = profile.devices.find(
              (device) => device.key === profile.primaryKey,
            );
            const rtf = primary != null ? 0.1 : 1.0;
            const audioS = request.words / 2.6;
            const totalS = audioS * rtf + request.words / 30 + 60;
            return {
              scriptS: 0,
              ttsS: audioS * rtf,
              alignS: 0,
              renderS: 60,
              totalS,
            };
          }
          case "retry_failed_job": {
            if (args.switchToLocal === true && config.jobScenario?.localRetryError) {
              throw config.jobScenario.localRetryError;
            }
            const target = jobs.find((job) => job.id === args.jobId);
            if (target) target.state = "QUEUED";
            return null;
          }
          case "provider_draft_models": {
            providerCalls.push({ cmd, args: { ...args } });
            const failure = config.providersScenario?.modelListError;
            if (failure != null) throw failure;
            // The real command validates the draft first: custom kinds
            // need a base URL, keyless stays loopback-only. This mock
            // checks only the null-URL case — the full law (canonical
            // URL, loopback-only keyless) is pinned by Rust unit tests
            // (connection.rs drafts_pass_the_same_law_as_saved_connections);
            // do not rely on this mock to enforce it.
            if (
              args.providerType === "openai_compatible" &&
              args.baseUrl == null
            )
              throw "an OpenAI-compatible connection needs a base URL";
            return (
              config.castProviders?.fetchedModels ?? {
                models: [],
                suggestedDefault: null,
              }
            );
          }
          case "provider_fetch_models": {
            const failure = config.providersScenario?.modelListError;
            if (failure != null) throw failure;
            const fetched = config.castProviders?.fetchedModels ?? {
              models: [],
              suggestedDefault: null,
            };
            // The real command persists the cache the sub-page reads back.
            mockCachedModels = {
              ...fetched,
              fetchedUnix: Math.floor(Date.now() / 1000),
            };
            return fetched;
          }
          case "cast_provider_options":
            return (
              config.castProviders ?? {
                scriptConnections: [],
                ttsVoices: [],
                visionConnections: [],
              }
            );
          case "estimate_cast_cost":
            return (
              config.castProviders?.cost ?? {
                state: "unknown",
                message: "Cost unavailable from this app - check your provider",
              }
            );
          case "cast_align_advisory":
            return config.castProviders?.alignAdvisory === true;
          case "cast_consent_grant": {
            grantedConsents.push({ ...(args as Record<string, unknown>) });
            return { id: `consent-${grantedConsents.length}` };
          }
          case "create_job": {
            const scenario = config.jobScenario;
            if (!scenario) throw "unexpected create_job call";
            if (scenario.createError) throw scenario.createError;
            if (scenario.createErrorOnce) {
              const once = scenario.createErrorOnce;
              scenario.createErrorOnce = undefined;
              throw once;
            }
            {
              const jobArgs = args.args as Record<string, unknown>;
              const wantsCloud =
                jobArgs.scriptCloud != null || jobArgs.ttsCloud != null;
              const pendingConsent =
                config.castProviders?.consentRequired ?? [];
              if (
                wantsCloud &&
                pendingConsent.length > 0 &&
                grantedConsents.length === 0
              ) {
                throw `CONSENT_REQUIRED:${JSON.stringify(pendingConsent)}`;
              }
            }
            const jobArgs = args.args as Record<string, unknown>;
            createJobCalls.push({ ...jobArgs });
            (
              window as unknown as {
                __createJobCalls: Array<Record<string, unknown>>;
              }
            ).__createJobCalls = createJobCalls;
            const job: Record<string, unknown> = {
              id: `job-${jobs.length + 1}`,
              bookId: jobArgs.bookId,
              scope: jobArgs.scope,
              mode: jobArgs.mode,
              voiceProfileId: jobArgs.voiceProfileId,
              outputFormat: jobArgs.outputFormat,
              modelOverridesJson: jobArgs.modelOverridesJson ?? null,
              state: jobArgs.overnight ? "QUEUED" : "CREATED",
              overnight: jobArgs.overnight === true,
              verify: jobArgs.verify === true,
              etaSeconds: null,
              reportJson: null,
              outputPathsJson: null,
              createdAt: Date.now(),
              updatedAt: Date.now(),
            };
            jobs.push(job);
            persistJobs();
            if (!jobArgs.overnight) void runJobLifecycle(job);
            return job.id;
          }
          case "list_jobs":
            // The runtime reads `ORDER BY created_at, id`; the mock returns
            // the same order so a tie in the seed cannot hide behind it.
            return [...jobs]
              .sort((a, b) =>
                (a.createdAt as number) !== (b.createdAt as number)
                  ? (a.createdAt as number) - (b.createdAt as number)
                  : String(a.id) < String(b.id)
                    ? -1
                    : String(a.id) > String(b.id)
                      ? 1
                      : 0,
              )
              .map((job) => ({ ...job }));
          case "list_reviewable_segments": {
            const scenario = config.jobScenario;
            return (scenario?.segments ?? []).map((row) => ({ ...row }));
          }
          case "switch_job_to_overnight": {
            const scenario = config.jobScenario;
            if (scenario?.switchToOvernightError)
              throw scenario.switchToOvernightError;
            const job = jobs.find((entry) => entry.id === args.jobId);
            if (!job) throw `job '${String(args.jobId)}' not found`;
            job.overnight = true;
            persistJobs();
            emitEvent("job-state", { job_id: job.id, state: job.state });
            return null;
          }
          case "resynthesize_segment": {
            const scenario = config.jobScenario;
            if (scenario?.resynthesizeError) throw scenario.resynthesizeError;
            const job = jobs.find((entry) => entry.id === args.jobId);
            if (!job) throw `job '${String(args.jobId)}' not found`;
            if (job.state !== "DONE") {
              throw `a segment can only be re-synthesized on a finished job, not one that is ${String(job.state)}`;
            }
            const segment = (scenario?.segments ?? []).find(
              (row) => row.id === args.segmentId,
            );
            if (!segment)
              throw `segment '${String(args.segmentId)}' is not in this job`;
            if (typeof args.text === "string") {
              if (!originalTexts.has(segment.id)) {
                originalTexts.set(segment.id, segment.text);
              }
              if (args.text === segment.text) {
                throw "that edit would not change the segment";
              }
              segment.text = args.text;
              segment.edited = true;
            } else {
              const original = originalTexts.get(segment.id);
              if (original === undefined) {
                throw "that edit would not change the segment";
              }
              segment.text = original;
              originalTexts.delete(segment.id);
              segment.edited = false;
            }
            job.state = "QUEUED";
            persistJobs();
            emitEvent("job-state", { job_id: job.id, state: "QUEUED" });
            void runJobLifecycle(job);
            return null;
          }
          case "rerender_job": {
            const job = jobs.find((entry) => entry.id === args.jobId);
            if (!job) throw `job '${String(args.jobId)}' not found`;
            if (job.state !== "DONE") {
              throw `only a finished job can be re-rendered, not one that is ${String(job.state)}`;
            }
            job.state = "QUEUED";
            persistJobs();
            emitEvent("job-state", { job_id: job.id, state: "QUEUED" });
            void runJobLifecycle(job);
            return null;
          }
          case "list_exports":
            return exports.map((row) => ({ ...row }));
          case "lan_send_start": {
            const ids = (args.exportIds as string[] | undefined) ?? [];
            if (ids.length === 0) throw "nothing selected to send";
            lanSendSnapshot = {
              phase: "waiting",
              pairedDevices: 0,
              deviceNames: [],
              pairLocked: false,
              acceptedConnections: 0,
              casts: ids.map((id) => {
                const row = exports.find((entry) => entry.id === id);
                return {
                  castId: id,
                  title: String(row?.path ?? id)
                    .split("/")
                    .pop()!,
                  bytesSent: 0,
                  bytesTotal: Number(row?.bytes ?? 0),
                  sentComplete: false,
                  missing: row == null,
                };
              }),
            };
            return {
              qrSvg:
                '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 21 21" role="img" aria-label="pairing code"><rect width="21" height="21" fill="#fff"/><path d="M2 2h5v5H2zM14 2h5v5h-5zM2 14h5v5H2zM9 9h3v3H9z"/></svg>',
              qrPayload: JSON.stringify({
                v: 1,
                f: "a".repeat(64),
                h: ["192.168.1.20:47317"],
                s: "b".repeat(32),
              }),
              fingerprint: "a".repeat(64),
              hosts: ["192.168.1.20:47317"],
              port: 47317,
              secretTtlMs: 300_000,
            };
          }
          case "lan_send_status":
            return lanSendSnapshot == null ? null : { ...lanSendSnapshot };
          case "lan_send_stop":
            lanSendSnapshot = null;
            return null;
          case "delete_export": {
            const index = exports.findIndex((row) => row.id === args.exportId);
            if (index < 0) throw `export '${String(args.exportId)}' not found`;
            exports.splice(index, 1);
            return null;
          }
          case "delete_all_exports": {
            const deleted = exports.length;
            exports.length = 0;
            return { deleted, failed: 0 };
          }
          case "pause_job": {
            const job = jobs.find((entry) => entry.id === args.jobId);
            if (!job) throw `job '${String(args.jobId)}' not found`;
            job.state = "PAUSED";
            persistJobs();
            emitEvent("job-state", { job_id: job.id, state: "PAUSED" });
            return null;
          }
          case "resume_job": {
            const job = jobs.find((entry) => entry.id === args.jobId);
            if (!job) throw `job '${String(args.jobId)}' not found`;
            // Runtime semantics (worker.rs `resume_job`): PAUSED → QUEUED; the
            // worker then runs the oldest QUEUED cast once nothing is running.
            job.state = "QUEUED";
            persistJobs();
            emitEvent("job-state", { job_id: job.id, state: "QUEUED" });
            dispatchOldestQueued();
            return null;
          }
          case "cancel_job": {
            const job = jobs.find((entry) => entry.id === args.jobId);
            if (!job) throw `job '${String(args.jobId)}' not found`;
            job.state = "CANCELLED";
            persistJobs();
            emitEvent("job-state", { job_id: job.id, state: "CANCELLED" });
            return null;
          }
          case "delete_job": {
            if (liveJobIds.includes(String(args.jobId)))
              throw "cannot delete a cast with a live tutor session; end the session first";
            const index = jobs.findIndex((entry) => entry.id === args.jobId);
            if (index < 0) throw `job '${String(args.jobId)}' not found`;
            const state = jobs[index].state;
            if (
              state !== "DONE" &&
              state !== "CANCELLED" &&
              state !== "FAILED" &&
              state !== "PAUSED"
            ) {
              throw `cannot delete job in state ${state}; pause or cancel it first`;
            }
            jobs.splice(index, 1);
            for (let at = voiceSessions.length - 1; at >= 0; at -= 1) {
              if (voiceSessions[at].job_id === args.jobId)
                voiceSessions.splice(at, 1);
            }
            for (const row of exports) {
              if (row.jobId === args.jobId) row.jobId = null;
            }
            persistJobs();
            return null;
          }
          default:
            throw `unmocked command: ${cmd}`;
        }
      }

      Object.defineProperty(window, "__TAURI_INTERNALS__", {
        value: {
          invoke,
          transformCallback(callback: (payload: unknown) => void) {
            const id = nextId++;
            callbacks.set(id, callback);
            return id;
          },
          unregisterCallback(id: number) {
            callbacks.delete(id);
          },
          convertFileSrc(filePath: string) {
            const withData = readingPages.find(
              (entry) =>
                entry.imagePath === filePath && entry.imageDataUrl != null,
            );
            if (withData?.imageDataUrl != null) return withData.imageDataUrl;
            const cached = readingPages.find(
              (entry) => entry.imagePath === filePath,
            );
            const broken = config.readingScenario?.brokenPages ?? [];
            if (cached && !broken.includes(cached.pageNo)) {
              return "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";
            }
            return `https://asset.localhost/${encodeURIComponent(filePath)}`;
          },
        },
      });
      Object.defineProperty(window, "__TAURI_EVENT_PLUGIN_INTERNALS__", {
        value: {
          unregisterListener() {},
        },
      });
    },
    {
      ...scenario,
      providerDirectory:
        scenario.providerDirectory ??
        (providerDirectory as ProviderDefinition[]),
      reading: scenario.reading ?? readingGolden,
    },
  );
}

export async function createJobCalls(
  page: Page,
): Promise<Array<Record<string, unknown>>> {
  return page.evaluate(
    () =>
      ((window as unknown as Record<string, unknown>).__createJobCalls ??
        []) as Array<Record<string, unknown>>,
  );
}

export async function readingCalls(page: Page): Promise<{
  index: string[];
  chapter: string[];
  pages: number[][];
  ensure: number[][];
  rendered: number[];
  figures: string[][];
  clock: string[];
  pageMode: string[];
  followState: string[];
  tap: string[];
}> {
  return page.evaluate(
    () =>
      ((window as unknown as Record<string, unknown>).__readingCalls ?? {
        index: [],
        chapter: [],
        pages: [],
        ensure: [],
        rendered: [],
        figures: [],
        clock: [],
        pageMode: [],
        followState: [],
        tap: [],
      }) as {
        index: string[];
        chapter: string[];
        pages: number[][];
        ensure: number[][];
        rendered: number[];
        figures: string[][];
        clock: string[];
        pageMode: string[];
        followState: string[];
        tap: string[];
      },
  );
}

export async function markExportStale(
  page: Page,
  path?: string,
): Promise<void> {
  await page.evaluate((wanted) => {
    const hook = (window as unknown as Record<string, unknown>)
      .__mockMarkExportStale as ((path?: string) => void) | undefined;
    hook?.(wanted);
  }, path);
}

export async function advanceLanSend(
  page: Page,
  phase: "paired" | "sending" | "done",
): Promise<void> {
  await page.evaluate((wanted) => {
    const hook = (window as unknown as Record<string, unknown>)
      .__mockLanSendAdvance as ((phase: string) => void) | undefined;
    hook?.(wanted);
  }, phase);
}
