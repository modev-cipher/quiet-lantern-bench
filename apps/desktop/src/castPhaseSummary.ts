// F6 phase visibility (owner feedback 2026-08-29): the cast form must SAY
// which model runs each phase — scripting, figures, and the voice — in
// plain words for a zero-knowledge user, with the exact id alongside for
// experts. Pure derivation over state the form already holds; the ruled
// defaults (local models, LOW effort) are what render when the user has
// chosen nothing.

import type {
  CastProviderOptions,
  ProviderSelection,
  ReasoningEffort,
} from "./castProviderModel";
import { DEFAULT_EFFORT, EFFORT_CHOICES } from "./castProviderModel";
import type { CatalogModelEntry } from "./modelCatalogModel";
import type { ResolvedJobModels } from "./jobWarningModel";
import { figureLine, type FigureState } from "./figureModel";
import { t } from "./i18n";
import { modelDisplayName, modelOutcome } from "./i18n/modelNames";

export interface PhaseSummaryLine {
  phase: "script" | "figures" | "tts";
  name: string;
  title: string;
  expert: string | null;
}

export function effortLabel(effort: ReasoningEffort | undefined): string {
  const value = effort ?? DEFAULT_EFFORT;
  const choice = EFFORT_CHOICES.find((option) => option.value === value);
  return choice ? t(choice.label) : value;
}

export function phaseSummaryLines(args: {
  mode: string;
  selection: ProviderSelection;
  options: CastProviderOptions | null;
  resolved: ResolvedJobModels;
  figureState: FigureState;
  catalogModels: CatalogModelEntry[];
  voiceName: string | null;
  voiceEngine: string | null;
}): PhaseSummaryLine[] {
  const {
    mode,
    selection,
    options,
    resolved,
    figureState,
    catalogModels,
    voiceName,
  } = args;

  let script: PhaseSummaryLine;
  if (selection.script.kind === "cloud") {
    const connection = options?.scriptConnections.find(
      (candidate) =>
        selection.script.kind === "cloud" &&
        candidate.connectionId === selection.script.connectionId,
    );
    script = {
      phase: "script",
      name: t("phase.name.script"),
      title: t("phase.script.cloud", {
        connection: connection?.name ?? t("phase.script.connectionFallback"),
        effort: effortLabel(selection.script.effort),
      }),
      expert: selection.script.model,
    };
  } else if (mode === "narrator") {
    script = {
      phase: "script",
      name: t("phase.name.script"),
      title: t("phase.script.narrator"),
      expert: resolved.llm ? modelOutcome(resolved.llm) : null,
    };
  } else {
    script = {
      phase: "script",
      name: t("phase.name.script"),
      title:
        resolved.llm != null
          ? t("phase.onThisComputer", {
              name: modelDisplayName(resolved.llm, catalogModels),
            })
          : t("phase.script.noModel"),
      expert: resolved.llm ? modelOutcome(resolved.llm) : null,
    };
  }

  const figures: PhaseSummaryLine = {
    phase: "figures",
    name: t("phase.name.figures"),
    title:
      figureState.kind === "off"
        ? t("phase.figures.skip")
        : figureState.kind === "local"
          ? t("phase.onThisComputer", {
              name: modelDisplayName(figureState.model, catalogModels),
            })
          : figureLine(figureState, catalogModels),
    expert:
      figureState.kind === "local" ? modelOutcome(figureState.model) : null,
  };

  let tts: PhaseSummaryLine;
  if (selection.tts.kind === "cloud") {
    const voice = options?.ttsVoices.find(
      (candidate) =>
        selection.tts.kind === "cloud" &&
        candidate.voiceProfileId === selection.tts.voiceProfileId,
    );
    tts = {
      phase: "tts",
      name: t("phase.name.voice"),
      title: t("phase.voice.cloud", {
        voice: voice?.name ?? t("phase.voice.cloudFallback"),
        connection:
          voice?.connectionName ?? t("phase.voice.connectionFallback"),
      }),
      expert: voice != null ? `${voice.model} · ${voice.voiceId}` : null,
    };
  } else {
    tts = {
      phase: "tts",
      name: t("phase.name.voice"),
      title:
        voiceName != null
          ? t("phase.voice.local", { voice: voiceName })
          : t("phase.voice.pick"),
      expert: resolved.tts ? modelOutcome(resolved.tts) : null,
    };
  }

  return [script, figures, tts];
}
