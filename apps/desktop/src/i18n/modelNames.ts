import type { MessageKey } from "./catalog";
import { t } from "./index";

// #31 public-copy law: no model ids, vendor names, parameter counts or
// mechanism words reach the DOM. Every catalog entry is named here by what it
// DOES (role) and, when several entries share a role, by tier (Pro / Pro+ /
// Max, ranked by the catalog's quality score). The raw `id` and vendor
// `display_name` are never returned.
export interface NameableModel {
  id: string;
  task: string;
  quality_score?: number;
  deprecated?: boolean;
}

export type ModelRole =
  "teaching" | "figures" | "voice" | "listening" | "layout" | "other";

export function modelRole(model: Pick<NameableModel, "task">): ModelRole {
  switch (model.task) {
    case "llm":
      return "teaching";
    case "tts":
      return "voice";
    case "asr":
      return "listening";
    case "layout":
      return "layout";
    default:
      return "other";
  }
}

const ROLE_KEY: Record<ModelRole, MessageKey> = {
  teaching: "models.role.teaching",
  figures: "models.role.figures",
  voice: "models.role.voice",
  listening: "models.role.listening",
  layout: "models.role.layout",
  other: "models.role.other",
};

const OUTCOME_KEY: Record<ModelRole, MessageKey> = {
  teaching: "models.outcome.teaching",
  figures: "models.outcome.figures",
  voice: "models.outcome.voice",
  listening: "models.outcome.listening",
  layout: "models.outcome.layout",
  other: "models.outcome.other",
};

/** Tier among the live entries of the same role: best quality = Max, next = Pro+, rest = Pro. */
export function modelTier(
  model: NameableModel,
  all: readonly NameableModel[],
  roleOverride?: ModelRole,
): MessageKey | null {
  const effectiveRole = roleOverride ?? modelRole(model);
  const seenIds = new Set<string>();
  const uniquePeers: NameableModel[] = [];
  for (const m of all) {
    if (!m.deprecated && modelRole(m) === effectiveRole && !seenIds.has(m.id)) {
      seenIds.add(m.id);
      uniquePeers.push(m);
    }
  }
  if (uniquePeers.length < 2) return null;
  const peers = uniquePeers.sort((a, b) => (b.quality_score ?? 0) - (a.quality_score ?? 0));
  const rank = peers.findIndex((m) => m.id === model.id);
  if (rank < 0) return null;
  return rank === 0
    ? "models.tier.max"
    : rank === 1
      ? "models.tier.proPlus"
      : "models.tier.pro";
}

/** User-facing name: "Teaching · Max" / "التعليم · Max"; "Teaching" when the role has one entry. */
export function modelDisplayName(
  model: NameableModel,
  all: readonly NameableModel[] = [model],
  roleOverride?: ModelRole,
): string {
  const role = t(ROLE_KEY[roleOverride ?? modelRole(model)]);
  const tier = modelTier(model, all, roleOverride);
  return tier == null ? role : t("models.name", { role, tier: t(tier) });
}

/** Outcome-first phrase for the role ("Explains the book in its own words"). */
export function modelOutcome(model: Pick<NameableModel, "task">): string {
  return t(OUTCOME_KEY[modelRole(model)]);
}
