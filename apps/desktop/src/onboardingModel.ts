import { t, type MessageKey } from "./i18n";
import type { CatalogModelEntry, BadgeKind } from "./modelCatalogModel";
import { primaryDevice, type DeviceInventory } from "./deviceModel";

export const WIZARD_STEPS = ["welcome", "voice", "book"] as const;
export type WizardStep = (typeof WIZARD_STEPS)[number];

export const WIZARD_TASKS: Record<string, MessageKey> = {
  tts: "wizard.intro.tts",
  llm: "wizard.intro.llm",
};

export interface WizardHardware extends DeviceInventory {
  ram: { totalMb: number; freeMb: number };
  cpu: { coresPhysical: number; coresLogical: number };
  disk: { modelsFreeGb: number };
}

export interface WizardSelection {
  tts: string[];
  llm: string | null;
}

export function emptySelection(): WizardSelection {
  return { tts: [], llm: null };
}

export interface Recommendation {
  model: CatalogModelEntry;
  fallback: boolean;
}

const TIER_ORDER: BadgeKind[] = ["green", "yellow", "orange", "red"];

export function recommendModel(
  models: CatalogModelEntry[],
  task: string,
): Recommendation | null {
  const candidates = models.filter(
    (m) => m.task === task && !m.deprecated && m.license.commercial_ok,
  );
  for (const tier of TIER_ORDER) {
    const pool = candidates
      .filter((m) => m.badge === tier)
      .sort(
        (a, b) =>
          b.quality_score - a.quality_score || b.speed_score - a.speed_score,
      );
    if (pool.length > 0) {
      return { model: pool[0], fallback: tier !== "green" };
    }
  }
  return null;
}

export function reasonLine(
  model: CatalogModelEntry,
  hw: WizardHardware | null,
  fallback: boolean,
): string {
  if (fallback) {
    return t("wizard.reason.fallback");
  }
  const device = primaryDevice(hw);
  if (device && model.full_gpu_vram_mb != null) {
    const have = device.memoryTotalMb ?? device.memoryFreeMb;
    if (have != null) {
      return t("wizard.reason.vram", {
        need: formatGb(model.full_gpu_vram_mb / 1024),
        have: formatGb(have / 1024),
      });
    }
  }
  if (hw) {
    return t("wizard.reason.ram", {
      need: formatGb(model.min_free_ram_mb / 1024),
      have: formatGb(hw.ram.totalMb / 1024),
    });
  }
  return t("wizard.reason.bestFit");
}

function formatGb(gb: number): string {
  const rounded = Math.round(gb * 10) / 10;
  return Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1);
}

export function taskModels(
  models: CatalogModelEntry[],
  task: string,
): CatalogModelEntry[] {
  return models
    .filter((m) => m.task === task && !m.deprecated)
    .sort(
      (a, b) =>
        TIER_ORDER.indexOf(a.badge) - TIER_ORDER.indexOf(b.badge) ||
        b.quality_score - a.quality_score ||
        b.speed_score - a.speed_score,
    );
}

export function toggleTts(
  selection: WizardSelection,
  modelId: string,
): WizardSelection {
  const tts = selection.tts.includes(modelId)
    ? selection.tts.filter((id) => id !== modelId)
    : [...selection.tts, modelId];
  return { ...selection, tts };
}

export function pickLlm(
  selection: WizardSelection,
  id: string | null,
): WizardSelection {
  return { ...selection, llm: id };
}

export function selectedModelIds(selection: WizardSelection): string[] {
  const ids = [...selection.tts];
  if (selection.llm) ids.push(selection.llm);
  return ids;
}

export function queuedModels(
  models: CatalogModelEntry[],
  selection: WizardSelection,
  installed: Set<string>,
): CatalogModelEntry[] {
  const ids = new Set(selectedModelIds(selection));
  return models.filter((m) => ids.has(m.id) && !installed.has(m.id));
}

export function combinedDiskBytes(queued: CatalogModelEntry[]): number {
  return queued.reduce((sum, m) => sum + m.disk_bytes, 0);
}

export function combinedDiskBlock(
  queued: CatalogModelEntry[],
  hw: WizardHardware | null,
): string | null {
  if (!hw) return null;
  const neededGb = combinedDiskBytes(queued) / 1e9;
  const freeGb = hw.disk.modelsFreeGb;
  if (neededGb <= freeGb) return null;
  return t("models.diskBlock", {
    need: `${neededGb.toFixed(1)} GB`,
    have: `${freeGb.toFixed(1)} GB`,
  });
}

export function defaultChoices(
  models: CatalogModelEntry[],
  selection: WizardSelection,
): Array<{ task: string; modelId: string }> {
  const choices: Array<{ task: string; modelId: string }> = [];
  const ttsSelected = models
    .filter((m) => selection.tts.includes(m.id))
    .sort(
      (a, b) =>
        b.quality_score - a.quality_score || b.speed_score - a.speed_score,
    );
  if (ttsSelected.length > 0) {
    choices.push({ task: "tts", modelId: ttsSelected[0].id });
  }
  if (selection.llm) choices.push({ task: "llm", modelId: selection.llm });
  return choices;
}

export function gpuLine(hw: WizardHardware): string {
  const device = primaryDevice(hw);
  if (!device) return t("wizard.noGpu");
  const memory = device.memoryTotalMb ?? device.memoryFreeMb;
  const label = device.unified ? t("wizard.sharedMemory") : "VRAM";
  const vram =
    memory != null
      ? ` · ${t("wizard.gpuMemory", { gb: formatGb(memory / 1024), label })}`
      : ` · ${t("wizard.memoryUnreadable", { label })}`;
  const others = hw.devices.length - 1;
  const rest = others > 0 ? ` ${t("wizard.moreDevices", { n: others })}` : "";
  return `${device.name}${vram}${rest}`;
}

export function ramCpuLine(hw: WizardHardware): string {
  return t("wizard.ramCpuLine", {
    gb: formatGb(hw.ram.totalMb / 1024),
    cores: hw.cpu.coresPhysical,
  });
}

export function diskLine(hw: WizardHardware): string {
  return t("wizard.diskLine", { gb: formatGb(hw.disk.modelsFreeGb) });
}

export const SKIP_COPY: Record<string, MessageKey> = {
  tts: "wizard.skipTts",
  llm: "wizard.skipLlm",
};

const BOOK_EXTENSIONS = [".pdf", ".epub"];

export function pickBookPath(paths: readonly string[]): string | null {
  return (
    paths.find((path) =>
      BOOK_EXTENSIONS.some((ext) => path.toLowerCase().endsWith(ext)),
    ) ?? null
  );
}
