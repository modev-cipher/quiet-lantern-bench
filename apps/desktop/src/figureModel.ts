import { t } from "./i18n";
import { modelDisplayName } from "./i18n/modelNames";
import {
  formatBytes,
  visionCapable,
  type CatalogModelEntry,
} from "./modelCatalogModel";

export type FigureState =
  | { kind: "off" }
  | { kind: "local"; model: CatalogModelEntry }
  | { kind: "cloud"; connectionName: string; model: string }
  | { kind: "cloudUnknown"; connectionName: string; model: string }
  | { kind: "projectorMissing"; model: CatalogModelEntry }
  | { kind: "unavailable" };

export interface CloudScriptFigures {
  connectionName: string;
  model: string;
  readsImages: boolean | "unknown";
}

export function castFigureModelId(params: {
  models: CatalogModelEntry[];
  installedIds: ReadonlySet<string>;
  llmOverrideId: string | null;
}): string | null {
  if (params.llmOverrideId != null) return params.llmOverrideId;
  const candidates = params.models
    .filter((m) => m.task === "llm" && params.installedIds.has(m.id))
    .sort(
      (a, b) =>
        Number(a.deprecated ?? false) - Number(b.deprecated ?? false) ||
        b.quality_score - a.quality_score,
    );
  return candidates[0]?.id ?? null;
}

export function figureState(params: {
  explainFigures: boolean;
  models: CatalogModelEntry[];
  castModelId: string | null;
  visionReadyIds: ReadonlySet<string>;
  projectorMissingIds: ReadonlySet<string>;
  cloudScript: CloudScriptFigures | null;
}): FigureState {
  if (!params.explainFigures) return { kind: "off" };
  if (params.cloudScript != null) {
    if (params.cloudScript.readsImages === "unknown") {
      return {
        kind: "cloudUnknown",
        connectionName: params.cloudScript.connectionName,
        model: params.cloudScript.model,
      };
    }
    return params.cloudScript.readsImages
      ? {
          kind: "cloud",
          connectionName: params.cloudScript.connectionName,
          model: params.cloudScript.model,
        }
      : { kind: "unavailable" };
  }
  const model = params.models.find((m) => m.id === params.castModelId);
  if (model == null || !visionCapable(model)) return { kind: "unavailable" };
  if (params.projectorMissingIds.has(model.id)) {
    return { kind: "projectorMissing", model };
  }
  if (params.visionReadyIds.has(model.id)) return { kind: "local", model };
  return { kind: "unavailable" };
}

export function figureOverride(explainFigures: boolean): {
  explain_figures?: false;
} {
  return explainFigures ? {} : { explain_figures: false };
}

export function figureModel(state: FigureState): CatalogModelEntry | null {
  return state.kind === "local" ? state.model : null;
}

export function projectorNeededCopy(entry: CatalogModelEntry): string {
  return entry.projector_bytes != null
    ? t("vision.projectorNeededSize", {
        name: entry.display_name,
        size: formatBytes(entry.projector_bytes),
      })
    : t("vision.projectorNeeded", { name: entry.display_name });
}

export function figureLine(
  state: FigureState,
  catalogModels: CatalogModelEntry[],
): string {
  switch (state.kind) {
    case "off":
      return t("cast.skipFiguresHint");
    case "local":
      return t("cast.figuresExplainedBy", {
        name: modelDisplayName(state.model, catalogModels),
      });
    case "cloud":
      return t("phase.figures.cloud", {
        connection: state.connectionName,
        model: state.model,
      });
    case "cloudUnknown":
      return t("providers.figuresUnknown", {
        connection: state.connectionName,
        model: state.model,
      });
    case "projectorMissing":
      return projectorNeededCopy(state.model);
    case "unavailable":
      return t("cast.figuresUnavailable");
  }
}
