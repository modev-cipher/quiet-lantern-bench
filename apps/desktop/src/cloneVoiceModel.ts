import type { VoiceProfileSummary } from "./jobFormModel";
import type { CatalogModelEntry } from "./modelCatalogModel";
import { t } from "./i18n";

export const VOICE_CLONING = "voice_cloning";

export const MIN_SECONDS = 3;
export const MAX_SECONDS = 12;

export const CLONING_ENGINE_FAMILY = "chatterbox";

export type SampleSource = "recording" | "import";

export interface VoiceSample {
  sampleId: string;
  seconds: number;
  hitHardStop: boolean;
  source: SampleSource;
}

export type CloneStep = "intro" | "capturing" | "review" | "saving";

export interface AudioInput {
  name: string;
  isDefault: boolean;
}

export interface CloneState {
  step: CloneStep;
  inputs: AudioInput[];
  /** Raw input-device error (dev log only); the UI renders inputsErrorKey. */
  inputsError: string | null;
  inputsErrorKey: CloneErrorKey | null;
  selectedInput: string | null;
  sample: VoiceSample | null;
  name: string;
  previewPath: string | null;
  /** Raw backend error (dev log only); the UI renders errorKey (#31). */
  error: string | null;
  errorKey: CloneErrorKey | null;
  busy: boolean;
}

export function initialState(): CloneState {
  return {
    step: "intro",
    inputs: [],
    inputsError: null,
    inputsErrorKey: null,
    selectedInput: null,
    sample: null,
    name: "",
    previewPath: null,
    error: null,
    errorKey: null,
    busy: false,
  };
}

export function cloningEngineIds(
  models: readonly CatalogModelEntry[],
): string[] {
  return models
    .filter(
      (model) =>
        model.task === "tts" &&
        !model.deprecated &&
        (model.capabilities ?? []).includes(VOICE_CLONING),
    )
    .map((model) => model.id);
}

export function cloningInstalled(
  installedEngines: readonly string[],
  cloningEngineIds: readonly string[],
): boolean {
  return cloningEngineIds.some((id) => installedEngines.includes(id));
}

export function canRecord(state: CloneState): boolean {
  return state.inputs.length > 0 && !state.busy;
}

export function canSave(state: CloneState): boolean {
  return state.sample != null && state.name.trim().length > 0 && !state.busy;
}

export function durationLabel(seconds: number): string {
  const rounded = Math.round(seconds * 10) / 10;
  return `${rounded.toFixed(1)}s`;
}

/** Params for `voice.clone.sampleSummary`; components render them through
 * `tx` so the duration island carries no control characters (I18N-1 R-E). */
export function sampleSummaryParams(sample: VoiceSample): {
  where: string;
  duration: string;
} {
  return {
    where: t(
      sample.source === "recording"
        ? "voice.clone.recorded"
        : "voice.clone.imported",
    ),
    duration: durationLabel(sample.seconds),
  };
}
export function sampleSummary(sample: VoiceSample): string {
  return t("voice.clone.sampleSummary", sampleSummaryParams(sample));
}

export function sampleWarningKey(
  sample: VoiceSample,
): "voice.clone.warnLong" | "voice.clone.warnMax" | null {
  if (sample.hitHardStop) return "voice.clone.warnLong";
  if (sample.seconds >= MAX_SECONDS - 0.05) return "voice.clone.warnMax";
  return null;
}
export function sampleWarning(sample: VoiceSample): string | null {
  const key = sampleWarningKey(sample);
  return key == null ? null : t(key, { n: MAX_SECONDS });
}

export function defaultName(existing: readonly VoiceProfileSummary[]): string {
  const base = t("voice.clone.defaultName");
  const taken = new Set(existing.map((profile) => profile.name));
  if (!taken.has(base)) return base;
  for (let n = 2; n < 1000; n += 1) {
    const candidate = `${base} ${n}`;
    if (!taken.has(candidate)) return candidate;
  }
  return base;
}

export function nameError(
  name: string,
  existing: readonly VoiceProfileSummary[],
): string | null {
  const trimmed = name.trim();
  if (trimmed.length === 0) {
    return t("voice.clone.nameEmpty");
  }
  if (existing.some((profile) => profile.name === trimmed)) {
    return t("voice.clone.nameTaken");
  }
  return null;
}

export function selectInput(
  state: CloneState,
  inputs: AudioInput[],
): CloneState {
  const preferred =
    inputs.find((input) => input.isDefault)?.name ?? inputs[0]?.name ?? null;
  return {
    ...state,
    inputs,
    inputsError: null,
    inputsErrorKey: null,
    selectedInput: preferred,
  };
}

export function inputsUnavailable(
  state: CloneState,
  message: string,
): CloneState {
  return {
    ...state,
    inputs: [],
    inputsError: message,
    inputsErrorKey: "voice.clone.err.noInputs",
    selectedInput: null,
  };
}

export function sampleReady(
  state: CloneState,
  sample: VoiceSample,
  suggestedName: string,
): CloneState {
  return {
    ...state,
    step: "review",
    sample,
    error: null,
    errorKey: null,
    busy: false,
    previewPath: null,
    name: state.name.trim().length > 0 ? state.name : suggestedName,
  };
}

export function discardSample(state: CloneState): CloneState {
  return {
    ...state,
    step: "intro",
    sample: null,
    previewPath: null,
    error: null,
    errorKey: null,
    busy: false,
  };
}

export type CloneErrorKey =
  | "voice.clone.err.tooShort"
  | "voice.clone.err.engineNotReady"
  | "voice.clone.err.noRecording"
  | "voice.clone.err.sampleGone"
  | "voice.clone.err.noInputs"
  | "voice.clone.err.generic";

/** Backend/native error text → an outcome-first catalog key (round-2 F4, #31):
 * the raw text (which can carry model ids and stack names) never reaches the
 * DOM; unknown classes render the generic line and log the raw text. */
export function cloneErrorKey(raw: string): CloneErrorKey {
  if (/at least \d+ seconds|too short|seconds of speech/i.test(raw))
    return "voice.clone.err.tooShort";
  if (/not ready|engine/i.test(raw)) return "voice.clone.err.engineNotReady";
  if (/no recording/i.test(raw)) return "voice.clone.err.noRecording";
  if (/no longer available|not found/i.test(raw))
    return "voice.clone.err.sampleGone";
  if (/microphone|input device|audio input/i.test(raw))
    return "voice.clone.err.noInputs";
  return "voice.clone.err.generic";
}

export function failed(state: CloneState, error: string): CloneState {
  const errorKey = cloneErrorKey(error);
  if (errorKey === "voice.clone.err.generic")
    console.debug("voice clone:", error);
  return {
    ...state,
    step: state.sample ? "review" : "intro",
    error,
    errorKey,
    busy: false,
  };
}
