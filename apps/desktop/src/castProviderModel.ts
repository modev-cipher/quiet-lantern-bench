import { t, type MessageKey } from "./i18n";
// Cast-form provider picker model (provider-flexibility S-4, Hermes
// pre-brief items 2-8). Pure logic, no Tauri: the component renders what
// these functions decide, and the vitest rows are the T4/T7/T12 proof
// surface. Local is ALWAYS the initial selection; nothing here ever
// activates a cloud choice on its own.

export interface CastConnectionOption {
  modelVision?: Record<string, "supported" | "unsupported" | "unknown">;
  connectionId: string;
  name: string;
  provider: string;
  defaultModel: string;
  loopback: boolean;
}

export interface CastCloudVoiceOption {
  voiceProfileId: string;
  name: string;
  voiceId: string;
  connectionId: string;
  connectionName: string;
  provider: string;
  model: string;
  loopback: boolean;
}

export interface CastProviderOptions {
  scriptConnections: CastConnectionOption[];
  ttsVoices: CastCloudVoiceOption[];
  visionConnections?: CastConnectionOption[];
}

export type CostEstimate =
  | {
      state: "known";
      usd_low: number;
      usd_high: number;
      currency: string;
      as_of: string;
      basis: string;
    }
  | {
      state: "stale";
      usd_low: number;
      usd_high: number;
      currency: string;
      as_of: string;
      basis: string;
    }
  | { state: "unknown"; message: string };

export interface ConsentRequirement {
  phase: string;
  connectionId: string;
  connectionName: string;
  provider: string;
  hosts: string[];
  dataCategories: string[];
  retentionUrl?: string | null;
}

export type ReasoningEffort = "low" | "medium" | "high";

// Owner ruling 2026-08-29: LOW is the default everywhere; raising it is an
// explicit per-phase expert choice, never a surprise.
export const DEFAULT_EFFORT: ReasoningEffort = "low";

export const EFFORT_CHOICES: {
  value: ReasoningEffort;
  label: MessageKey;
  hint: MessageKey;
}[] = [
  {
    value: "low",
    label: "cast.effort.low.label",
    hint: "cast.effort.low.hint",
  },
  {
    value: "medium",
    label: "cast.effort.medium.label",
    hint: "cast.effort.medium.hint",
  },
  {
    value: "high",
    label: "cast.effort.high.label",
    hint: "cast.effort.high.hint",
  },
];

export type ScriptSelection =
  | { kind: "local" }
  | {
      kind: "cloud";
      connectionId: string;
      model: string;
      effort?: ReasoningEffort;
    };

export type TtsSelection =
  { kind: "local" } | { kind: "cloud"; voiceProfileId: string };

export interface ProviderSelection {
  script: ScriptSelection;
  tts: TtsSelection;
}

// T4: every fresh or duplicated form starts Local/Local. There is no other
// constructor — prior job history may only ever render as inert text.
export function initialProviderSelection(): ProviderSelection {
  return { script: { kind: "local" }, tts: { kind: "local" } };
}

// T12 cost truth: known shows amount + as-of + approximate; stale is
// visibly stale; unknown is the exact provider-check line. Never "$0",
// never "Free".
// A real but tiny amount must never round down to a zero rendering (T12):
// below a displayable cent it reads "under $0.01", never "$0.000".
function amount(usd: number): string {
  if (usd < 0.01) {
    return "under $0.01";
  }
  return `~$${usd.toFixed(usd < 0.1 ? 3 : 2)}`;
}

function range(low: number, high: number, currency: string): string {
  if (Math.abs(high - low) < 1e-9) {
    return `${amount(low)} ${currency}`;
  }
  return `${amount(low)}–${amount(high)} ${currency}`;
}

export function costLine(estimate: CostEstimate | null): string {
  if (estimate == null) {
    return t("cast.costEstimating");
  }
  switch (estimate.state) {
    case "known":
      return t("cast.costKnown", {
        range: range(estimate.usd_low, estimate.usd_high, estimate.currency),
        asOf: estimate.as_of,
      });
    case "stale":
      return t("cast.costStale", {
        range: range(estimate.usd_low, estimate.usd_high, estimate.currency),
        asOf: estimate.as_of,
      });
    case "unknown":
      return estimate.message;
  }
}

// The snapshot's OWN computed basis renders with the number (Hermes R2
// fold-in 5): what was assumed is visible, not a generic disclaimer.
export function costBasis(estimate: CostEstimate | null): string | null {
  if (estimate == null || estimate.state === "unknown") {
    return null;
  }
  return estimate.basis;
}

// A18: the loopback fact line — no cost claims, no consent theater.
export const LOOPBACK_FACT = () => t("cast.loopbackFact");

const CONSENT_PREFIX = "CONSENT_REQUIRED:";

// The writer boundary refuses with a structured payload when consent is
// missing; the form opens the sheet from it. Anything else is an ordinary
// error message.
export function parseConsentRequired(
  message: string,
): ConsentRequirement[] | null {
  if (!message.startsWith(CONSENT_PREFIX)) {
    return null;
  }
  try {
    const parsed = JSON.parse(message.slice(CONSENT_PREFIX.length));
    return Array.isArray(parsed) ? (parsed as ConsentRequirement[]) : null;
  } catch {
    return null;
  }
}

// Consent copy (pre-brief item 6): names the connection/provider, every
// destination host, the exact data category in plain words, key
// authorization without exposure, and provider-controlled retention.
export function consentDataCategoryCopy(category: string): string {
  switch (category) {
    case "source_text":
      return t("cast.data.sourceText");
    case "script_text":
      return t("cast.data.scriptText");
    case "figure_crops":
      return t("cast.data.figureCrops");
    default:
      return category;
  }
}

export function consentPhaseCopy(phase: string): string {
  switch (phase) {
    case "script":
      return t("cast.phaseScripting");
    case "vision":
      return t("cast.phaseFigures");
    default:
      return t("cast.phaseAudio");
  }
}

export function consentTitleCopy(
  requirements: ReadonlyArray<Pick<ConsentRequirement, "dataCategories">>,
): string {
  const categories = requirements.flatMap((r) => r.dataCategories);
  const images = categories.includes("figure_crops");
  const text = categories.some((c) => c !== "figure_crops");
  if (images && text) return t("cast.consentTitleTextAndImages");
  if (images) return t("cast.consentTitleImages");
  return t("cast.consentTitle");
}

export function cloudArgs(selection: ProviderSelection): {
  scriptCloud?: {
    connectionId: string;
    model: string;
    effort?: ReasoningEffort;
  };
  ttsCloud?: { voiceProfileId: string };
} {
  const args: {
    scriptCloud?: {
      connectionId: string;
      model: string;
      effort?: ReasoningEffort;
    };
    ttsCloud?: { voiceProfileId: string };
  } = {};
  if (selection.script.kind === "cloud") {
    args.scriptCloud = {
      connectionId: selection.script.connectionId,
      model: selection.script.model,
      effort: selection.script.effort ?? DEFAULT_EFFORT,
    };
  }
  if (selection.tts.kind === "cloud") {
    args.ttsCloud = { voiceProfileId: selection.tts.voiceProfileId };
  }
  return args;
}
