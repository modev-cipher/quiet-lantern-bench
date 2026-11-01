import { primaryVramFreeMb, type DeviceInventory } from "./deviceModel";
import { t } from "./i18n";
import { modelDisplayName, type ModelRole } from "./i18n/modelNames";
import type { BadgeKind, CatalogModelEntry } from "./modelCatalogModel";

export interface ResolvedJobModels {
  tts: CatalogModelEntry | null;
  llm: CatalogModelEntry | null;
}

export interface WarningHardware extends Partial<DeviceInventory> {
  ram?: { freeMb: number };
}

const BADGE_SEVERITY: Record<BadgeKind, number> = {
  green: 0,
  yellow: 1,
  orange: 2,
  red: 3,
};

export function badgeSeverity(kind: BadgeKind | null | undefined): number {
  return kind == null ? 4 : BADGE_SEVERITY[kind];
}

export function usableInstalledModels(
  models: CatalogModelEntry[],
  task: string,
  installed: ReadonlySet<string>,
): CatalogModelEntry[] {
  const inTask = models.filter((m) => m.task === task && installed.has(m.id));
  const live = inTask.filter((m) => !m.deprecated);
  return live.length > 0 ? live : inTask;
}

function bestForTask(
  models: CatalogModelEntry[],
  task: string,
  installed: ReadonlySet<string>,
): CatalogModelEntry | null {
  const pool = usableInstalledModels(models, task, installed).sort(
    (a, b) =>
      BADGE_SEVERITY[a.badge] - BADGE_SEVERITY[b.badge] ||
      b.quality_score - a.quality_score,
  );
  return pool[0] ?? null;
}

export function resolveTaskModel(
  models: CatalogModelEntry[],
  task: string,
  defaults: Readonly<Record<string, string | null | undefined>>,
  installed: ReadonlySet<string>,
): CatalogModelEntry | null {
  const defaultId = defaults[task];
  if (defaultId != null) {
    const entry = usableInstalledModels(models, task, installed).find(
      (m) => m.id === defaultId,
    );
    if (entry != null) return entry;
  }
  return bestForTask(models, task, installed);
}

export function resolveJobModels(
  models: CatalogModelEntry[],
  defaults: Record<string, string>,
  installed: ReadonlySet<string>,
): ResolvedJobModels {
  return {
    tts: resolveTaskModel(models, "tts", defaults, installed),
    llm: resolveTaskModel(models, "llm", defaults, installed),
  };
}

export function jobModelsInUse(
  resolved: ResolvedJobModels,
): CatalogModelEntry[] {
  const models: CatalogModelEntry[] = [];
  if (resolved.tts) models.push(resolved.tts);
  if (resolved.llm) models.push(resolved.llm);
  return models;
}

export function activeJobModels(params: {
  models: readonly CatalogModelEntry[];
  mode: "narrator" | "teacher";
  ttsIsCloud: boolean;
  scriptIsCloud: boolean;
}): CatalogModelEntry[] {
  return params.models.filter((model) => {
    if (model.task === "tts") return !params.ttsIsCloud;
    if (model.task === "llm")
      return params.mode === "teacher" && !params.scriptIsCloud;
    return true;
  });
}

export function visionBadgeModel(
  models: CatalogModelEntry[],
  visionModelId: string | null,
): CatalogModelEntry | null {
  if (visionModelId == null) return null;
  const entry = models.find((m) => m.id === visionModelId);
  if (entry == null) return null;
  return { ...entry, badge: entry.vision_badge ?? entry.badge };
}

export function worstBadge(models: CatalogModelEntry[]): BadgeKind | null {
  let worst: BadgeKind | null = null;
  for (const model of models) {
    if (worst == null || BADGE_SEVERITY[model.badge] > BADGE_SEVERITY[worst]) {
      worst = model.badge;
    }
  }
  return worst;
}

export function worstModel(
  models: CatalogModelEntry[],
): CatalogModelEntry | null {
  let worst: CatalogModelEntry | null = null;
  for (const model of models) {
    if (
      worst == null ||
      BADGE_SEVERITY[model.badge] > BADGE_SEVERITY[worst.badge]
    ) {
      worst = model;
    }
  }
  return worst;
}

export function etaWarningLine(etaLabel: string): string {
  return t("fit.eta", { eta: etaLabel });
}

/** 🔴 inline copy: culprit by role (#31-safe display name), then what is free. */
export function redInlineCopy(
  culprit: CatalogModelEntry | null,
  hw: WarningHardware | null,
  all: readonly CatalogModelEntry[] = culprit ? [culprit] : [],
  roleOverride?: ModelRole,
): string {
  if (culprit == null) return t("fit.red.detail");
  const model = modelDisplayName(culprit, all, roleOverride);
  if (hw?.ram == null) return t("fit.red.inlineNoHardware", { model });
  const ram = formatGb(hw.ram.freeMb / 1024);
  const vram = primaryVramFreeMb(hw);
  const available =
    vram == null
      ? t("fit.red.availableRam", { ram })
      : t("fit.red.availableBoth", { ram, vram: formatGb(vram / 1024) });
  return t("fit.red.inline", { model, available });
}

function formatGb(gb: number): string {
  return t("fit.gb", { gb: gb.toFixed(1) });
}

export const redConfirmCopy = () => ({
  title: t("fit.red.confirm.title"),
  body: t("fit.red.confirm.body"),
  run: t("fit.red.confirm.run"),
  choose: t("fit.red.confirm.choose"),
});

/** Which start button is the primary action for this fit (U5: 🟠 → overnight). */
export type StartIntent = "cast" | "overnight";
export function primaryAction(badge: BadgeKind | null): StartIntent {
  return badge === "orange" ? "overnight" : "cast";
}

/**
 * A 🔴 confirm is bound to the exact fit it was given for — the badge and the
 * model set it aggregates — and exists only as a call argument: "Run anyway"
 * mints it and hands it straight to `submit`, which spends it. A failed start,
 * a consent decline, or any change to the fit therefore re-asks (F2).
 */
export function confirmToken(
  badge: BadgeKind | null,
  models: readonly CatalogModelEntry[],
): string | null {
  if (badge !== "red") return null;
  const set = models
    .map((model) => `${model.id}=${model.badge}`)
    .sort()
    .join(",");
  return `red:${set}`;
}

/**
 * The start gate. `submit` runs only on `ok`; 🔴 needs a confirm token minted
 * for the CURRENT fit (`confirmToken`), so a token for another badge state or
 * model set is not a confirm for this one.
 */
export type StartGate =
  | { kind: "not-ready" }
  | { kind: "confirm-required"; culpritId: string | null }
  | { kind: "ok" };
export function startGate(params: {
  badge: BadgeKind | null;
  ready: boolean;
  models: readonly CatalogModelEntry[];
  culprit: CatalogModelEntry | null;
  token: string | null;
}): StartGate {
  if (!params.ready) return { kind: "not-ready" };
  if (params.models.length === 0) return { kind: "ok" };
  if (params.badge == null) return { kind: "not-ready" };
  if (params.badge !== "red") return { kind: "ok" };
  const expected = confirmToken(params.badge, params.models);
  if (params.token != null && params.token === expected) return { kind: "ok" };
  return { kind: "confirm-required", culpritId: params.culprit?.id ?? null };
}
