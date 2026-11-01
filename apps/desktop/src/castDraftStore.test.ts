import { afterEach, expect, it, vi } from "vitest";
import {
  castDraftSnapshot,
  clearCastDraft,
  getCastDraft,
  pruneCastDrafts,
  saveCastDraft,
  subscribeCastDrafts,
  type CastDraftFields,
} from "./castDraftStore";

const fields = (): CastDraftFields => ({
  step: "sound",
  scopeKind: "pages",
  selected: ["chapter-a"],
  pageStart: "3",
  pageEnd: "5",
  mode: "teacher",
  voiceEngineMode: "cloud",
  pickedVoiceId: "local-voice",
  selectedLocalLlmId: "local-model",
  selectedCloudProviderId: "connection-a",
  cloudVoiceMemory: { "connection-a": "cloud-voice" },
  explainFigures: false,
  providerSelection: {
    script: { kind: "local" },
    tts: { kind: "cloud", voiceProfileId: "cloud-voice" },
  },
  stickyMemory: { tts: {}, llm: {} },
  selectedProviders: { tts: "connection-a", llm: "local" },
  customScriptModel: null,
  fetchedScriptModels: {},
  scriptWherePick: null,
  verify: true,
});

afterEach(() => pruneCastDrafts(new Set()));

it("isolates books and snapshots choices without retaining the caller's mutable objects", () => {
  const choices = fields();
  saveCastDraft("a", "Book A", choices);
  saveCastDraft("b", "Book B", { ...fields(), pageStart: "8" });
  choices.selected?.push("another-chapter");
  choices.cloudVoiceMemory["connection-a"] = "different-voice";
  expect(getCastDraft("a")?.selected).toEqual(["chapter-a"]);
  expect(getCastDraft("a")?.cloudVoiceMemory["connection-a"]).toBe(
    "cloud-voice",
  );
  expect(getCastDraft("b")?.pageStart).toBe("8");
  expect(getCastDraft("new-book")).toBeNull();
  clearCastDraft("a");
  expect(getCastDraft("a")).toBeNull();
  expect(getCastDraft("b")?.pageStart).toBe("8");
});

it("notifies only meaningful changes and removes drafts for deleted books", () => {
  const listener = vi.fn();
  const unsubscribe = subscribeCastDrafts(listener);
  saveCastDraft("a", "A", fields());
  saveCastDraft("b", "B", fields());
  const saved = castDraftSnapshot();
  clearCastDraft("missing");
  pruneCastDrafts(new Set(["a", "b"]));
  expect(castDraftSnapshot()).toBe(saved);
  expect(listener).toHaveBeenCalledTimes(2);
  pruneCastDrafts(new Set(["b"]));
  expect(castDraftSnapshot().map((draft) => draft.bookId)).toEqual(["b"]);
  expect(listener).toHaveBeenCalledTimes(3);
  unsubscribe();
});
