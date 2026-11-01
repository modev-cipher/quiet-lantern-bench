import type { ProviderSelection } from "./castProviderModel";
import type { ProviderSelectionMemory } from "./castModelSelectionModel";
import type { JobMode } from "./jobFormModel";

export interface CastDraftFields {
  step: "scope" | "sound" | "review";
  scopeKind: "chapters" | "pages";
  selected: string[] | null;
  pageStart: string;
  pageEnd: string;
  mode: JobMode;
  voiceEngineMode: "local" | "cloud";
  pickedVoiceId: string | null;
  selectedLocalLlmId: string | null;
  selectedCloudProviderId: string | null;
  cloudVoiceMemory: Record<string, string>;
  explainFigures: boolean;
  providerSelection: ProviderSelection;
  stickyMemory: ProviderSelectionMemory;
  selectedProviders: { tts: string; llm: string };
  customScriptModel: string | null;
  fetchedScriptModels: Record<string, string[]>;
  scriptWherePick: "local" | "cloud" | null;
  verify: boolean;
}

interface CastDraft {
  bookId: string;
  title: string;
  fields: CastDraftFields;
}

let drafts: readonly CastDraft[] = [];
const listeners = new Set<() => void>();
const emit = () => listeners.forEach((listener) => listener());

export function subscribeCastDrafts(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export const castDraftSnapshot = () => drafts;

export function getCastDraft(bookId: string): CastDraftFields | null {
  return drafts.find((draft) => draft.bookId === bookId)?.fields ?? null;
}

export function saveCastDraft(
  bookId: string,
  title: string,
  fields: CastDraftFields,
): void {
  drafts = [
    ...drafts.filter((draft) => draft.bookId !== bookId),
    { bookId, title, fields: structuredClone(fields) },
  ];
  emit();
}

export function clearCastDraft(bookId: string): void {
  const next = drafts.filter((draft) => draft.bookId !== bookId);
  if (next.length === drafts.length) return;
  drafts = next;
  emit();
}

export function pruneCastDrafts(bookIds: ReadonlySet<string>): void {
  const next = drafts.filter((draft) => bookIds.has(draft.bookId));
  if (next.length === drafts.length) return;
  drafts = next;
  emit();
}
