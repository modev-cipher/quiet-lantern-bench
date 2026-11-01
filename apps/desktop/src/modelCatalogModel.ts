import { t, type MessageKey } from "./i18n";
import { primaryDevice, type DeviceInventory } from "./deviceModel";

export interface CatalogLicense {
  spdx: string;
  commercial_ok: boolean;
}

export interface ResourcePlan {
  model_id: string;
  /** Rust serialises `DevicePlan` ({ backend, ordinal?, key? }); older mocks send a string. */
  device: string | { backend: string; ordinal?: number; key?: string };
  n_gpu_layers: number;
  n_layers_total: number;
  kv_offload: boolean;
  ctx: number;
  batch: number;
  tok_s_est?: number | null;
  badge: BadgeKind;
}

export interface CatalogModelEntry {
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
  languages?: string[];
  license: CatalogLicense;
  deprecated?: boolean;
  badge: BadgeKind;
  memory_unreadable?: boolean;
  vision_badge?: BadgeKind | null;
  projector_bytes?: number | null;
  manifest_sha256: string;
  plan?: ResourcePlan | null;
}

export const FIGURE_EXPLANATION = "figure_explanation";

export function visionCapable(model: CatalogModelEntry): boolean {
  return (
    model.task === "llm" &&
    (model.capabilities ?? []).includes(FIGURE_EXPLANATION)
  );
}

export interface FullCatalog {
  catalog_version: string;
  models: CatalogModelEntry[];
}

export interface HardwareFitProfile extends Partial<DeviceInventory> {
  ram: { freeMb: number };
  disk: { modelsFreeGb: number };
}

export interface InstalledModel {
  modelId: string;
  diskBytes: number;
  sha256: string;
  missingFiles?: string[];
}

export function projectorMissing(install: InstalledModel): boolean {
  return (install.missingFiles ?? []).some((path) => path.includes("mmproj"));
}

export function completeReadySet(installs: InstalledModel[]): Set<string> {
  return new Set(
    installs
      .filter((i) => (i.missingFiles ?? []).length === 0)
      .map((i) => i.modelId),
  );
}

export function textReadySet(installs: InstalledModel[]): Set<string> {
  return new Set(
    installs
      .filter((i) => (i.missingFiles ?? []).every((path) => path.includes("mmproj")))
      .map((i) => i.modelId),
  );
}

export function visionReadySet(installs: InstalledModel[]): Set<string> {
  return completeReadySet(installs);
}

export function projectorMissingSet(installs: InstalledModel[]): Set<string> {
  return new Set(installs.filter(projectorMissing).map((i) => i.modelId));
}

export interface ModelDownloadProgress {
  modelId: string;
  fileIndex: number;
  filesTotal: number;
  bytesDone: number;
  bytesTotal: number;
  state: "downloading" | "reconnecting" | "verifying" | "done";
}

export type BadgeKind = "green" | "yellow" | "orange" | "red";

export interface FitBadge {
  kind: BadgeKind;
  label: string;
  detail: string;
}

export const UNREADABLE_VRAM_DETAIL = () => t("fit.yellow.vramUnreadable");

/**
 * `hw` decides the 🟢 wording when the plan is absent: the planner returns
 * `{ badge: Green, plan: None }` for a model that fits on the CPU of a GPU-less
 * machine (plan.rs `cpu_badge`), and that machine must never read "on your GPU"
 * (F3). Unknown hardware (`undefined`) with no plan yields neutral wording.
 */
export function fitBadge(
  model: CatalogModelEntry,
  hw?: Partial<DeviceInventory> | null,
): FitBadge {
  return badge(
    model.badge,
    model.plan ?? null,
    model.memory_unreadable === true,
    runsOnCpu(model.plan ?? null, hw),
  );
}

/** true = will not touch a GPU here; false = uses one; null = cannot tell. */
export function runsOnCpu(
  plan: ResourcePlan | null,
  hw: Partial<DeviceInventory> | null | undefined,
): boolean | null {
  const device: unknown = plan?.device;
  if (device === "cpu") return true;
  if (typeof device === "object" && device != null) {
    return (device as { backend?: unknown }).backend === "cpu";
  }
  if (plan != null && plan.n_gpu_layers > 0) return false;
  if (hw === undefined) return null;
  return primaryDevice(hw) == null;
}

function badge(
  kind: BadgeKind,
  plan: ResourcePlan | null,
  unreadable = false,
  cpuOnly: boolean | null = null,
): FitBadge {
  const base = badgeCopy(kind);
  if (unreadable && kind === "yellow") {
    return { ...base, detail: UNREADABLE_VRAM_DETAIL() };
  }
  if (kind === "green" && cpuOnly === true) {
    return { ...base, detail: t("fit.green.detailCpu") };
  }
  if (kind === "green" && cpuOnly == null) {
    return { ...base, detail: t("fit.green.detailUnknown") };
  }
  if (plan && kind === "yellow" && plan.n_layers_total > 0) {
    const gpuBlocks = Math.min(
      Math.max(plan.n_gpu_layers - 1, 0),
      plan.n_layers_total,
    );
    const pct = Math.round((gpuBlocks / plan.n_layers_total) * 100);
    return { ...base, detail: t("fit.yellow.detailPlan", { pct }) };
  }
  return base;
}

// ONBOARDING_UX §2 canonical strings, all through the catalog (U5).
function badgeCopy(kind: BadgeKind): FitBadge {
  return {
    kind,
    label: t(`fit.${kind}.label`),
    detail: t(`fit.${kind}.detail`),
  };
}

export function fitReason(
  model: CatalogModelEntry,
  hw: HardwareFitProfile | null,
): string {
  const vramNeed = isKnownAmount(model.full_gpu_vram_mb)
    ? model.full_gpu_vram_mb
    : null;
  if (vramNeed != null) {
    const need = formatGbFromMb(vramNeed);
    if (hw == null) {
      return t("models.fit.vram", { need });
    }
    const device = primaryDevice(hw);
    if (device == null) {
      return t("models.fit.vramNoGpu", { need });
    }
    const vramFree = device.memoryFreeMb ?? null;
    if (vramFree != null) {
      return t("models.fit.vramHave", { need, have: formatGbFromMb(vramFree) });
    }
    return t("models.fit.vramUnknown", { need });
  }
  if (!isKnownAmount(model.min_free_ram_mb)) return "";
  const ramNeed = formatGbFromMb(model.min_free_ram_mb);
  const ramFree = hw?.ram.freeMb;
  if (ramFree != null) {
    return t("models.fit.ramHave", { need: ramNeed, have: formatGbFromMb(ramFree) });
  }
  return t("models.fit.ram", { need: ramNeed });
}

function formatGbFromMb(mb: number): string {
  return `${(mb / 1024).toFixed(1)} GB`;
}

export function diskBlockMessage(
  model: CatalogModelEntry,
  hw: HardwareFitProfile | null,
): string | null {
  if (!hw) return null;
  const neededGb = model.disk_bytes / 1e9;
  const freeGb = hw.disk.modelsFreeGb;
  if (neededGb <= freeGb) return null;
  return t("models.diskBlock", { need: formatGb(neededGb), have: formatGb(freeGb) });
}

function formatGb(gb: number): string {
  return `${gb.toFixed(1)} GB`;
}

function isKnownAmount(value: number | null | undefined): value is number {
  return value != null && Number.isFinite(value) && value >= 0;
}

export function formatBytes(bytes: number): string {
  if (!isKnownAmount(bytes)) return "—";
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  if (bytes >= 1e6) return `${Math.round(bytes / 1e6)} MB`;
  return `${Math.max(1, Math.round(bytes / 1e3))} KB`;
}

export const TASK_ORDER = ["tts", "llm", "align", "layout"] as const;

export const TASK_LABELS: Record<string, MessageKey> = {
  llm: "models.group.llm",
  tts: "models.group.tts",
  align: "models.group.align",
  layout: "models.group.layout",
};

export const TASK_PURPOSE: Record<string, MessageKey> = {
  tts: "models.purpose.tts",
  llm: "models.purpose.llm",
  align: "models.purpose.align",
  layout: "models.purpose.layout",
};

export interface Recommendation {
  model: CatalogModelEntry;
  fallback: boolean;
}

export function recommendModel(
  models: CatalogModelEntry[],
): Recommendation | null {
  const live = models.filter((model) => !model.deprecated);
  if (live.length === 0) return null;
  const byQuality = (a: CatalogModelEntry, b: CatalogModelEntry) =>
    b.quality_score - a.quality_score || b.speed_score - a.speed_score;
  const green = live.filter((model) => model.badge === "green").sort(byQuality);
  if (green.length > 0) return { model: green[0]!, fallback: false };
  const hybrid = live
    .filter((model) => model.badge === "yellow")
    .sort(byQuality);
  if (hybrid.length > 0) return { model: hybrid[0]!, fallback: true };
  const best = [...live].sort(byQuality)[0]!;
  return { model: best, fallback: true };
}

export interface CapabilityStatus {
  task: string;
  label: string;
  ready: boolean;
}

const SETUP_FEATURES: { task: string; label: MessageKey }[] = [
  { task: "tts", label: "models.role.voice" },
  { task: "llm", label: "models.setup.teaching" },
];

export function setupStatus(
  models: CatalogModelEntry[],
  installed: Set<string>,
): CapabilityStatus[] {
  const live = models.filter((model) => !model.deprecated);
  return SETUP_FEATURES.filter((feature) =>
    live.some((model) => model.task === feature.task),
  ).map((feature) => ({
    task: feature.task,
    label: t(feature.label),
    ready: models.some(
      (model) => model.task === feature.task && installed.has(model.id),
    ),
  }));
}

export interface TaskGroup {
  task: string;
  label: string;
  models: CatalogModelEntry[];
}

export function groupByTask(
  models: CatalogModelEntry[],
  installed: Set<string> = new Set(),
): TaskGroup[] {
  const known = new Set<string>(TASK_ORDER);
  const tasks: string[] = [...TASK_ORDER];
  for (const model of models) {
    if (!known.has(model.task)) {
      known.add(model.task);
      tasks.push(model.task);
    }
  }
  return tasks
    .map((task) => ({
      task,
      label: TASK_LABELS[task] ? t(TASK_LABELS[task]) : task,
      models: models
        .filter(
          (m) => m.task === task && (!m.deprecated || installed.has(m.id)),
        )
        .sort((a, b) => b.quality_score - a.quality_score),
    }))
    .filter((group) => group.models.length > 0);
}

export function downloadPct(progress: ModelDownloadProgress): number {
  if (progress.filesTotal <= 0) return 0;
  const fileFraction =
    progress.bytesTotal > 0 ? progress.bytesDone / progress.bytesTotal : 0;
  const overall =
    (progress.fileIndex + Math.min(1, fileFraction)) / progress.filesTotal;
  return Math.min(100, Math.round(overall * 100));
}

export function downloadStatusLabel(progress: ModelDownloadProgress): string {
  if (progress.state === "reconnecting") return t("models.download.reconnecting");
  if (progress.state === "verifying") {
    if (progress.filesTotal > 1) {
      return t("models.download.verifyingFile", { index: progress.fileIndex + 1, total: progress.filesTotal });
    }
    return t("models.download.verifying");
  }
  if (progress.state === "done") return t("models.download.finishing");
  if (progress.filesTotal > 1) {
    return t("models.download.downloadingFile", { index: progress.fileIndex + 1, total: progress.filesTotal });
  }
  return t("models.download.downloading");
}

export function installedSet(installs: InstalledModel[]): Set<string> {
  return new Set(installs.map((i) => i.modelId));
}

export function updateAvailable(
  model: CatalogModelEntry,
  installs: InstalledModel[],
): boolean {
  const install = installs.find((i) => i.modelId === model.id);
  if (!install) return false;
  return (
    install.sha256 !== model.manifest_sha256 ||
    (install.missingFiles ?? []).length > 0
  );
}

function onlyProjectorMissing(
  model: CatalogModelEntry,
  install: InstalledModel,
): boolean {
  const missing = install.missingFiles ?? [];
  return (
    missing.length > 0 &&
    missing.every((path) => path.includes("mmproj")) &&
    model.projector_bytes != null
  );
}

export function updateSizeBytes(
  model: CatalogModelEntry,
  installs: InstalledModel[],
): number {
  const install = installs.find((i) => i.modelId === model.id);
  if (!install) return model.disk_bytes;
  if (onlyProjectorMissing(model, install)) return model.projector_bytes ?? 0;
  return Math.max(0, model.disk_bytes - install.diskBytes);
}

export function updateLabel(
  model: CatalogModelEntry,
  installs: InstalledModel[],
): string {
  const install = installs.find((i) => i.modelId === model.id);
  const extra = updateSizeBytes(model, installs);
  if (install != null && onlyProjectorMissing(model, install)) {
    return t("models.addFigureExplanationSize", { size: formatBytes(extra) });
  }
  if (extra <= 0) return t("models.update");
  return t("models.updateSize", { size: formatBytes(extra) });
}

export function licenseTag(model: CatalogModelEntry): string | null {
  return model.license.commercial_ok ? null : t("models.nonCommercial");
}

export const DEFAULTABLE_TASKS = ["llm", "tts", "align"] as const;

export function canSetDefault(task: string): boolean {
  return (DEFAULTABLE_TASKS as readonly string[]).includes(task);
}
