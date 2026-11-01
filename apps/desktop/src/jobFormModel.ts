import { t } from "./i18n";
import { modelDisplayName } from "./i18n/modelNames";
import type { DeviceInventory } from "./deviceModel";

export interface ChapterSummary {
  id: string;
  idx: number;
  title: string;
  pageStart: number;
  pageEnd: number;
  isPseudo: boolean;
  wordCount: number | null;
}

export interface VoiceProfileSummary {
  id: string;
  name: string;
  engine: string;
  kind: string;
  presetVoiceId?: string | null;
}

export type JobMode = "narrator" | "teacher";
export type OutputFormat = "mp3";

export type Scope =
  | { kind: "chapters"; chapter_ids: string[] }
  | { kind: "pages"; page_start: number; page_end: number };

export interface CreateJobArgs {
  bookId: string;
  scope: Scope;
  mode: JobMode;
  outputFormat: OutputFormat;
  voiceProfileId: string;
  modelOverridesJson?: string | null;
  overnight: boolean;
  verify: boolean;
}

export interface EtaRequest {
  mode: JobMode;
  words: number;
  voiceEngine: string;
  llmModelId?: string | null;
  ttsWorkers: number;
}

export interface EtaBreakdown {
  scriptS: number;
  ttsS: number;
  alignS: number;
  renderS: number;
  totalS: number;
}

export type FitBadgeKind = "green" | "yellow" | "orange" | "red";

export interface CatalogModel {
  id: string;
  task: string;
  display_name?: string;
  badge?: FitBadgeKind;
}

export interface Catalog {
  models: CatalogModel[];
}

export interface VoiceFit {
  kind: FitBadgeKind;
  label: string;
}

export function voiceFit(
  catalog: Catalog | null,
  voiceEngine: string | null,
): VoiceFit | null {
  if (voiceEngine == null) return null;
  const entry = catalog?.models.find((model) => model.id === voiceEngine);
  if (!entry?.badge) return null;
  return { kind: entry.badge, label: t(`fit.${entry.badge}.label`) };
}

export function resolveDefaultVoiceId(
  voices: VoiceProfileSummary[],
  installedIds: ReadonlySet<string>,
  lastChoiceId: string | null,
  speaksBook?: (voice: VoiceProfileSummary) => boolean,
  engineFitRank?: (engine: string) => number | null,
  preferredEngine?: string | null,
): string | null {
  if (lastChoiceId != null) {
    const last = voices.find((voice) => voice.id === lastChoiceId);
    if (last != null && installedIds.has(last.engine)) return last.id;
  }
  const rank = (engine: string): number => engineFitRank?.(engine) ?? 4;
  const bestFit = (pool: VoiceProfileSummary[]): VoiceProfileSummary | null => {
    let best: VoiceProfileSummary | null = null;
    for (const voice of pool) {
      if (best == null || rank(voice.engine) < rank(best.engine)) best = voice;
    }
    return best;
  };

  if (preferredEngine != null && installedIds.has(preferredEngine)) {
    const prefVoices = voices.filter(
      (v) => v.engine === preferredEngine && installedIds.has(v.engine),
    );
    if (prefVoices.length > 0) {
      if (lastChoiceId != null) {
        const last = prefVoices.find((v) => v.id === lastChoiceId);
        if (last != null) return last.id;
      }
      if (speaksBook != null) {
        const match = prefVoices.find(speaksBook);
        if (match != null) return match.id;
      }
      return prefVoices[0]!.id;
    }
  }

  if (lastChoiceId != null) {
    const last = voices.find((voice) => voice.id === lastChoiceId);
    if (last != null && installedIds.has(last.engine)) return last.id;
  }
  if (speaksBook != null) {
    const compatible = bestFit(
      voices.filter(
        (voice) => installedIds.has(voice.engine) && speaksBook(voice),
      ),
    );
    if (compatible != null) return compatible.id;
  }
  const installed = bestFit(
    voices.filter((voice) => installedIds.has(voice.engine)),
  );
  if (installed != null) return installed.id;
  return voices[0]?.id ?? null;
}

const LAST_VOICE_KEY = "txtcaster.lastVoiceId";

export function readLastVoiceId(): string | null {
  try {
    return localStorage.getItem(LAST_VOICE_KEY);
  } catch {
    return null;
  }
}

export function writeLastVoiceId(voiceId: string): void {
  try {
    localStorage.setItem(LAST_VOICE_KEY, voiceId);
  } catch {
    return;
  }
}

export interface MissingModel {
  modelId: string;
  message: string;
}

export function missingModels(params: {
  mode: JobMode;
  voiceEngine: string | null;
  catalog: Catalog | null;
  installedIds: ReadonlySet<string>;
  ttsIsCloud?: boolean;
  scriptIsCloud?: boolean;
}): MissingModel[] {
  const missing: MissingModel[] = [];
  if (!params.ttsIsCloud) {
    if (params.voiceEngine == null) {
      missing.push({
        modelId: "",
        message: t("cast.missing.noVoice"),
      });
    } else if (!params.installedIds.has(params.voiceEngine)) {
      const label = modelLabel(params.catalog, params.voiceEngine);
      missing.push({
        modelId: params.voiceEngine,
        message: t("cast.missing.voice", { name: label }),
      });
    }
  }
  if (params.mode === "teacher" && !params.scriptIsCloud) {
    const llm = params.catalog?.models.find(
      (model) => model.task === "llm" && params.installedIds.has(model.id),
    );
    if (!llm) {
      missing.push({
        modelId: "",
        message: t("cast.missing.teacher"),
      });
    }
  }
  return missing;
}

function modelLabel(catalog: Catalog | null, modelId: string): string {
  const models = catalog?.models ?? [];
  const entry = models.find((model) => model.id === modelId);
  return entry != null
    ? modelDisplayName(entry, models)
    : t("cast.missing.voiceFallback");
}

export interface HardwareProfile extends Partial<DeviceInventory> {
  ram?: { freeMb: number };
}

export interface ComputeEstimate {
  computeScore: number;
  band: string;
  tokSEst: number | null;
}

const FALLBACK_WORDS_PER_PAGE = 250;

export function chapterWords(chapter: ChapterSummary): number {
  if (chapter.wordCount != null && chapter.wordCount > 0) {
    return chapter.wordCount;
  }
  const pages = Math.max(1, chapter.pageEnd - chapter.pageStart + 1);
  return pages * FALLBACK_WORDS_PER_PAGE;
}

export function selectedChapterWords(
  chapters: ChapterSummary[],
  selectedIds: ReadonlySet<string>,
): number {
  return chapters
    .filter((chapter) => selectedIds.has(chapter.id))
    .reduce((total, chapter) => total + chapterWords(chapter), 0);
}

export function pageRangeWords(
  chapters: ChapterSummary[],
  start: number,
  end: number,
): number {
  if (start > end || start < 1) return 0;
  let total = 0;
  let overlapped = false;
  for (const chapter of chapters) {
    if (chapter.pageStart > end || chapter.pageEnd < start) continue;
    overlapped = true;
    const overlapStart = Math.max(chapter.pageStart, start);
    const overlapEnd = Math.min(chapter.pageEnd, end);
    const overlapPages = overlapEnd - overlapStart + 1;
    const chapterPages = Math.max(1, chapter.pageEnd - chapter.pageStart + 1);
    total += Math.round((chapterWords(chapter) * overlapPages) / chapterPages);
  }
  if (!overlapped) {
    return (end - start + 1) * FALLBACK_WORDS_PER_PAGE;
  }
  return total;
}

export type PageRangeResult =
  { ok: true; start: number; end: number } | { ok: false; error: string };

export function validatePageRange(
  startText: string,
  endText: string,
  pageCount: number | null,
): PageRangeResult {
  const start = Number.parseInt(startText, 10);
  const end = Number.parseInt(endText, 10);
  if (!Number.isFinite(start) || !Number.isFinite(end)) {
    return { ok: false, error: t("cast.range.enterBoth") };
  }
  if (start < 1) {
    return { ok: false, error: t("cast.range.startMin") };
  }
  if (pageCount != null && end > pageCount) {
    return {
      ok: false,
      error: t("cast.range.pageCount", { count: pageCount }),
    };
  }
  if (start > end) {
    return { ok: false, error: t("cast.range.order") };
  }
  return { ok: true, start, end };
}

export function buildEtaRequest(params: {
  mode: JobMode;
  words: number;
  voiceEngine: string;
  llmModelId?: string | null;
}): EtaRequest {
  return {
    mode: params.mode,
    words: params.words,
    voiceEngine: params.voiceEngine,
    llmModelId: params.llmModelId ?? null,
    ttsWorkers: 1,
  };
}

// Phase-aware ETA (Hermes R2 blocker 8): a phase replaced by a cloud
// selection contributes NO local work to the local estimate. Cloud Script
// drops the local Teacher pass (the estimate degrades to narrator-shaped
// local work); cloud TTS drops local synthesis entirely — when both are
// cloud there is no local estimate to show and the caller renders the
// network-dependence fact instead.
export function buildCloudAwareEtaRequest(params: {
  mode: JobMode;
  words: number;
  voiceEngine: string | null;
  scriptIsCloud: boolean;
  ttsIsCloud: boolean;
}): EtaRequest | null {
  if (params.ttsIsCloud) {
    // Local work is script-only (or nothing when that is cloud too).
    if (params.scriptIsCloud || params.mode === "narrator") {
      return null;
    }
    // Teacher scripting still runs locally: estimate it without TTS by
    // requesting zero synthesis words... the backend has no script-only
    // shape, so the honest answer is no combined local number.
    return null;
  }
  if (params.voiceEngine == null) {
    return null;
  }
  return {
    mode: params.scriptIsCloud ? "narrator" : params.mode,
    words: params.words,
    voiceEngine: params.voiceEngine,
    llmModelId: null,
    ttsWorkers: 1,
  };
}

export function formatEta(totalS: number): string {
  if (!Number.isFinite(totalS) || totalS <= 0) return "—";
  const minutes = Math.round(totalS / 60);
  if (minutes < 1) return t("cast.eta.underMinute");
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  const minutesText = t("session.durationMinutes", { count: rest });
  const duration =
    hours === 0
      ? minutesText
      : rest === 0
        ? t("session.durationHours", { count: hours })
        : t("session.durationHoursMinutes", {
            hours: t("session.durationHours", { count: hours }),
            minutes: minutesText,
          });
  return t("cast.eta.approx", { duration });
}

export function chapterPages(
  chapter: ChapterSummary,
  locale: string = "en",
): string {
  const isAr = locale === "ar";
  const numLocale = isAr ? "ar-u-nu-latn" : "en-US";
  const pagePrefix = isAr
    ? "ص."
    : chapter.pageStart === chapter.pageEnd
      ? "p."
      : "pp.";
  return chapter.pageStart === chapter.pageEnd
    ? `${pagePrefix} ${chapter.pageStart.toLocaleString(numLocale)}`
    : `${pagePrefix} ${chapter.pageStart.toLocaleString(numLocale)}–${chapter.pageEnd.toLocaleString(numLocale)}`;
}

export function chapterMeta(
  chapter: ChapterSummary,
  locale: string = "en",
): string {
  const isAr = locale === "ar";
  const numLocale = isAr ? "ar-u-nu-latn" : "en-US";
  const pages = chapterPages(chapter, locale);
  if (chapter.wordCount != null && chapter.wordCount > 0) {
    const wordsStr = chapter.wordCount.toLocaleString(numLocale);
    const wordsLabel = isAr ? "كلمة" : "words";
    return `${wordsStr} ${wordsLabel} · ${pages}`;
  }
  return pages;
}

export function castStartErrorMessage(raw: string): string {
  if (raw.includes("ERR_FFMPEG_MISSING")) {
    return t("cast.ffmpegMissing");
  }
  return t("cast.startFailed", { reason: raw });
}
