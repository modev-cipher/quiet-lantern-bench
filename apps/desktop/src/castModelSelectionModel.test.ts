import { describe, expect, it } from "vitest";

import {
  buildCloudLlmOptions,
  buildCloudTtsOptions,
  buildLocalLlmOptions,
  buildLocalTtsOptions,
  computeCategoryRenderPlan,
  countCloudProviders,
  getCloudProvidersForCategory,
  switchProviderInPlan,
  type CastCategory,
  type ModelOption,
  type ProviderOption,
} from "./castModelSelectionModel";
import type { CastProviderOptions } from "./castProviderModel";
import type { CatalogModelEntry } from "./modelCatalogModel";

const mockLocalVoices = [
  { id: "voice-1", name: "Heart", engine: "kokoro-82m", kind: "preset" },
  { id: "voice-2", name: "Kumo", engine: "kokoro-82m", kind: "preset" },
  { id: "voice-3", name: "Xiaobei", engine: "kokoro-82m", kind: "preset" },
];

function mockModel(overrides: Partial<CatalogModelEntry> & { id: string; display_name: string; task: string }): CatalogModelEntry {
  return {
    family: overrides.id,
    format: "gguf",
    disk_bytes: 2_000_000_000,
    min_free_ram_mb: 4096,
    hybrid_capable: false,
    quality_score: 80,
    speed_score: 80,
    license: { spdx: "Apache-2.0", commercial_ok: true },
    badge: "green",
    manifest_sha256: "test-hash",
    ...overrides,
  };
}

const mockCatalogModels: CatalogModelEntry[] = [
  mockModel({
    id: "qwen2.5-7b",
    display_name: "Qwen 2.5 7B",
    task: "llm",
    quality_score: 90,
    speed_score: 80,
    disk_bytes: 4_500_000_000,
    min_free_ram_mb: 8000,
    badge: "green",
    capabilities: ["vision"],
  }),
  mockModel({
    id: "llama3.2-3b",
    display_name: "Llama 3.2 3B",
    task: "llm",
    quality_score: 75,
    speed_score: 95,
    disk_bytes: 2_000_000_000,
    min_free_ram_mb: 4000,
    badge: "green",
  }),
];

const mockCastOptionsSingle: CastProviderOptions = {
  scriptConnections: [
    {
      connectionId: "conn-gemini",
      name: "Google Gemini",
      provider: "gemini",
      defaultModel: "gemini-3.8-flash",
      loopback: false,
    },
  ],
  ttsVoices: [
    {
      voiceProfileId: "gem-puck",
      name: "Gemini Puck",
      voiceId: "Puck",
      connectionId: "conn-gemini",
      connectionName: "Google Gemini",
      provider: "gemini",
      model: "gemini-2.5-flash",
      loopback: false,
    },
    {
      voiceProfileId: "gem-charon",
      name: "Gemini Charon",
      voiceId: "Charon",
      connectionId: "conn-gemini",
      connectionName: "Google Gemini",
      provider: "gemini",
      model: "gemini-2.5-flash",
      loopback: false,
    },
  ],
  visionConnections: [
    {
      connectionId: "conn-gemini",
      name: "Google Gemini",
      provider: "gemini",
      defaultModel: "gemini-3.8-flash",
      loopback: false,
    },
  ],
};

const mockCastOptionsMulti: CastProviderOptions = {
  scriptConnections: [
    {
      connectionId: "conn-gemini",
      name: "Google Gemini",
      provider: "gemini",
      defaultModel: "gemini-3.8-flash",
      loopback: false,
    },
    {
      connectionId: "conn-openai",
      name: "OpenAI",
      provider: "openai_compatible",
      defaultModel: "gpt-5.6-luna",
      loopback: false,
    },
  ],
  ttsVoices: [
    {
      voiceProfileId: "gem-puck",
      name: "Gemini Puck",
      voiceId: "Puck",
      connectionId: "conn-gemini",
      connectionName: "Google Gemini",
      provider: "gemini",
      model: "gemini-2.5-flash",
      loopback: false,
    },
    {
      voiceProfileId: "oai-alloy",
      name: "OpenAI Alloy",
      voiceId: "alloy",
      connectionId: "conn-openai-tts",
      connectionName: "OpenAI Audio",
      provider: "openai_tts",
      model: "tts-1",
      loopback: false,
    },
  ],
  visionConnections: [
    {
      connectionId: "conn-gemini",
      name: "Google Gemini",
      provider: "gemini",
      defaultModel: "gemini-3.8-flash",
      loopback: false,
    },
    {
      connectionId: "conn-openai",
      name: "OpenAI",
      provider: "openai_compatible",
      defaultModel: "gpt-5.6-luna",
      loopback: false,
    },
  ],
};

describe("castModelSelectionModel", () => {
  describe("countCloudProviders & getCloudProvidersForCategory", () => {
    it("returns 0 cloud providers when options is null or empty", () => {
      expect(countCloudProviders(null, "tts")).toBe(0);
      expect(countCloudProviders({ scriptConnections: [], ttsVoices: [] }, "tts")).toBe(0);
      expect(countCloudProviders({ scriptConnections: [], ttsVoices: [] }, "llm")).toBe(0);
      expect(getCloudProvidersForCategory(null, "tts")).toEqual([]);
    });

    it("correctly identifies a single cloud provider", () => {
      expect(countCloudProviders(mockCastOptionsSingle, "tts")).toBe(1);
      expect(countCloudProviders(mockCastOptionsSingle, "llm")).toBe(1);

      const providers = getCloudProvidersForCategory(mockCastOptionsSingle, "tts");
      expect(providers).toHaveLength(1);
      expect(providers[0].id).toBe("conn-gemini");
    });

    it("correctly counts multiple distinct cloud providers", () => {
      expect(countCloudProviders(mockCastOptionsMulti, "tts")).toBe(2);
      expect(countCloudProviders(mockCastOptionsMulti, "llm")).toBe(2);

      const providers = getCloudProvidersForCategory(mockCastOptionsMulti, "llm");
      expect(providers).toHaveLength(2);
      expect(providers.map((p) => p.id)).toEqual(["conn-gemini", "conn-openai"]);
    });

    it("counts loopback connections as selectable providers", () => {
      const withLoopback: CastProviderOptions = {
        scriptConnections: [
          {
            connectionId: "c-ollama",
            name: "Local Ollama",
            provider: "openai_compatible",
            defaultModel: "llama3",
            loopback: true,
          },
        ],
        ttsVoices: [],
      };
      expect(countCloudProviders(withLoopback, "llm")).toBe(1);
      const providers = getCloudProvidersForCategory(withLoopback, "llm");
      expect(providers).toHaveLength(1);
      expect(providers[0]).toMatchObject({
        id: "c-ollama",
        kind: "cloud",
        loopback: true,
      });
      const opts = buildCloudLlmOptions(withLoopback.scriptConnections);
      expect(opts).toHaveLength(1);
      expect(opts[0].id).toBe("cloud:c-ollama:llama3");
    });

    it("builds cloud options correctly for LLM", () => {
      const llmOpts = buildCloudLlmOptions(mockCastOptionsMulti.scriptConnections, {
        "conn-gemini": ["gemini-3.8-flash", "gemini-3.7-flash"],
      });
      expect(llmOpts.length).toBeGreaterThan(1);
      expect(llmOpts.some((o) => o.id.includes("gemini-3.8-flash"))).toBe(true);
    });
  });

  describe("Local-Only Mode (0 cloud providers)", () => {
    const installedIds = new Set(["kokoro-82m", "qwen2.5-7b", "llama3.2-3b"]);

    it("Zero-option guard: 0 local options produces hidden mode", () => {
      const plan = computeCategoryRenderPlan({
        category: "tts",
        mode: "narrator",
        localOptions: [],
        cloudOptions: [],
        cloudProviders: [],
      });
      expect(plan.mode).toBe("hidden");
      expect(plan.totalOptionsCount).toBe(0);
      expect(plan.providerCount).toBe(0);
    });

    it("Single-option guard: 1 local TTS voice auto-selects and produces fixed-auto mode (no redundant dropdown)", () => {
      const singleVoice = buildLocalTtsOptions(
        [mockLocalVoices[0]],
        new Set(["kokoro-82m"]),
      );
      expect(singleVoice).toHaveLength(1);

      const plan = computeCategoryRenderPlan({
        category: "tts",
        mode: "narrator",
        localOptions: singleVoice,
        cloudOptions: [],
        cloudProviders: [],
      });

      expect(plan.mode).toBe("fixed-auto");
      expect(plan.totalOptionsCount).toBe(1);
      expect(plan.fixedOption?.id).toBe("voice-1");
      expect(plan.selectedModelId).toBe("voice-1");
    });

    it("2+ local options produce clean dropdown mode without cloud artifacts", () => {
      const multiVoice = buildLocalTtsOptions(mockLocalVoices, installedIds);
      expect(multiVoice).toHaveLength(3);

      const plan = computeCategoryRenderPlan({
        category: "tts",
        mode: "narrator",
        localOptions: multiVoice,
        cloudOptions: [],
        cloudProviders: [],
        smartDefaultLocalId: "voice-2",
      });

      expect(plan.mode).toBe("dropdown");
      expect(plan.totalOptionsCount).toBe(3);
      expect(plan.dropdownOptions).toHaveLength(3);
      expect(plan.selectedModelId).toBe("voice-2");
      expect(plan.providers).toHaveLength(0);
    });

    it("LLM in Narrator mode is always hidden", () => {
      const localLlms = buildLocalLlmOptions(mockCatalogModels, installedIds);
      const plan = computeCategoryRenderPlan({
        category: "llm",
        mode: "narrator",
        localOptions: localLlms,
        cloudOptions: [],
        cloudProviders: [],
      });
      expect(plan.mode).toBe("hidden");
    });

    it("LLM in Teacher mode with 1 option is fixed-auto, with 2+ options is dropdown", () => {
      const oneLlm = buildLocalLlmOptions([mockCatalogModels[0]], installedIds);
      const plan1 = computeCategoryRenderPlan({
        category: "llm",
        mode: "teacher",
        localOptions: oneLlm,
        cloudOptions: [],
        cloudProviders: [],
      });
      expect(plan1.mode).toBe("fixed-auto");
      expect(plan1.selectedModelId).toBe("qwen2.5-7b");

      const twoLlms = buildLocalLlmOptions(mockCatalogModels, installedIds);
      const plan2 = computeCategoryRenderPlan({
        category: "llm",
        mode: "teacher",
        localOptions: twoLlms,
        cloudOptions: [],
        cloudProviders: [],
        smartDefaultLocalId: "llama3.2-3b",
      });
      expect(plan2.mode).toBe("dropdown");
      expect(plan2.selectedModelId).toBe("llama3.2-3b");
    });

  });

  describe("Single Cloud Provider Mode (1 cloud provider)", () => {
    const installedIds = new Set(["kokoro-82m", "qwen2.5-7b"]);
    const localVoices = buildLocalTtsOptions(mockLocalVoices, installedIds);
    const cloudVoices = buildCloudTtsOptions(mockCastOptionsSingle.ttsVoices);
    const cloudProvidersTts = getCloudProvidersForCategory(mockCastOptionsSingle, "tts");

    it("reveals cloud options ONLY for that provider without placeholders", () => {
      expect(cloudProvidersTts).toHaveLength(1);
      const plan = computeCategoryRenderPlan({
        category: "tts",
        mode: "narrator",
        localOptions: localVoices,
        cloudOptions: cloudVoices,
        cloudProviders: cloudProvidersTts,
      });

      expect(plan.mode).toBe("dropdown");
      expect(plan.providerCount).toBe(1);
      expect(plan.totalOptionsCount).toBe(localVoices.length + cloudVoices.length);
      for (const opt of plan.dropdownOptions) {
        if (!opt.isLocal) {
          expect(opt.providerId).toBe("conn-gemini");
        }
      }
    });

    it("preserves Local as initial default selection", () => {
      const plan = computeCategoryRenderPlan({
        category: "tts",
        mode: "narrator",
        localOptions: localVoices,
        cloudOptions: cloudVoices,
        cloudProviders: cloudProvidersTts,
        smartDefaultLocalId: "voice-1",
      });

      expect(plan.selectedModelId).toBe("voice-1");
      const selected = plan.dropdownOptions.find((o) => o.id === plan.selectedModelId);
      expect(selected?.isLocal).toBe(true);
    });

    it("single-option guard applies if only 1 total option exists across local and single cloud", () => {
      const oneLocal = [localVoices[0]];
      const plan = computeCategoryRenderPlan({
        category: "tts",
        mode: "narrator",
        localOptions: oneLocal,
        cloudOptions: [],
        cloudProviders: cloudProvidersTts,
      });
      expect(plan.mode).toBe("fixed-auto");
      expect(plan.fixedOption?.id).toBe("voice-1");
    });
  });

  describe("Multi-Provider Mode (2+ cloud providers)", () => {
    const installedIds = new Set(["kokoro-82m", "qwen2.5-7b", "llama3.2-3b"]);
    const localVoices = buildLocalTtsOptions(mockLocalVoices, installedIds);
    const cloudVoices = buildCloudTtsOptions(mockCastOptionsMulti.ttsVoices);
    const cloudProvidersTts = getCloudProvidersForCategory(mockCastOptionsMulti, "tts");

    it("activates cascading mode with Local ALWAYS first and always present", () => {
      expect(cloudProvidersTts).toHaveLength(2);
      const plan = computeCategoryRenderPlan({
        category: "tts",
        mode: "narrator",
        localOptions: localVoices,
        cloudOptions: cloudVoices,
        cloudProviders: cloudProvidersTts,
        smartDefaultLocalId: "voice-1",
      });

      expect(plan.mode).toBe("cascading");
      expect(plan.providerCount).toBe(2);
      expect(plan.providers[0]).toEqual({
        id: "local",
        name: "On this device (Local)",
        isLocal: true,
        kind: "local",
      });
      expect(plan.selectedProviderId).toBe("local");
      expect(plan.cascadingModelOptions).toEqual(localVoices);
      expect(plan.selectedModelId).toBe("voice-1");
    });

    it("never auto-activates a cloud choice on its own", () => {
      const plan = computeCategoryRenderPlan({
        category: "tts",
        mode: "narrator",
        localOptions: localVoices,
        cloudOptions: cloudVoices,
        cloudProviders: cloudProvidersTts,
      });
      expect(plan.selectedProviderId).toBe("local");
      const selectedModel = plan.cascadingModelOptions.find((o) => o.id === plan.selectedModelId);
      expect(selectedModel?.isLocal).toBe(true);
    });

    it("omits local provider when local options are empty", () => {
      const plan = computeCategoryRenderPlan({
        category: "tts",
        mode: "narrator",
        localOptions: [],
        cloudOptions: cloudVoices,
        cloudProviders: cloudProvidersTts,
      });
      expect(plan.mode).toBe("cascading");
      expect(plan.providers.some((p) => p.id === "local")).toBe(false);
      expect(plan.selectedProviderId).toBe(cloudProvidersTts[0].id);
    });

    it("switching provider resets model to that provider smart default", () => {
      const plan = computeCategoryRenderPlan({
        category: "tts",
        mode: "narrator",
        localOptions: localVoices,
        cloudOptions: cloudVoices,
        cloudProviders: cloudProvidersTts,
        smartDefaultLocalId: "voice-1",
        smartDefaultCloudIds: {
          "conn-gemini": "cloud:gem-puck",
          "conn-openai-tts": "cloud:oai-alloy",
        },
      });

      const switched = switchProviderInPlan({
        plan,
        newProviderId: "conn-gemini",
        memory: {},
        smartDefaultLocalId: "voice-1",
        smartDefaultCloudIds: {
          "conn-gemini": "cloud:gem-puck",
        },
        localOptions: localVoices,
        cloudOptions: cloudVoices,
      });

      expect(switched.updatedProviderId).toBe("conn-gemini");
      expect(switched.updatedModelId).toBe("cloud:gem-puck");
      expect(switched.updatedMemory["conn-gemini"]).toBe("cloud:gem-puck");
      expect(switched.updatedMemory["local"]).toBe("voice-1");
    });

    it("switching back keeps previous selection if still valid (never traps the user)", () => {
      const plan = computeCategoryRenderPlan({
        category: "tts",
        mode: "narrator",
        localOptions: localVoices,
        cloudOptions: cloudVoices,
        cloudProviders: cloudProvidersTts,
        selectedModelId: "voice-2",
      });

      const step1 = switchProviderInPlan({
        plan,
        newProviderId: "conn-gemini",
        memory: { local: "voice-2" },
        localOptions: localVoices,
        cloudOptions: cloudVoices,
      });

      expect(step1.updatedProviderId).toBe("conn-gemini");

      step1.updatedMemory["conn-gemini"] = "cloud:gem-puck";

      const step2 = switchProviderInPlan({
        plan: {
          ...plan,
          selectedProviderId: "conn-gemini",
          selectedModelId: "cloud:gem-puck",
        },
        newProviderId: "local",
        memory: step1.updatedMemory,
        localOptions: localVoices,
        cloudOptions: cloudVoices,
      });

      expect(step2.updatedProviderId).toBe("local");
      expect(step2.updatedModelId).toBe("voice-2");

      const step3 = switchProviderInPlan({
        plan: {
          ...plan,
          selectedProviderId: "local",
          selectedModelId: "voice-2",
        },
        newProviderId: "conn-gemini",
        memory: step2.updatedMemory,
        localOptions: localVoices,
        cloudOptions: cloudVoices,
      });

      expect(step3.updatedProviderId).toBe("conn-gemini");
      expect(step3.updatedModelId).toBe("cloud:gem-puck");
    });

    it("applies single-option guard inside cascading mode if chosen provider has 1 model", () => {
      const singleVoicePerProvider = [
        mockCastOptionsMulti.ttsVoices[0],
      ];
      const singleCloudOpts = buildCloudTtsOptions(singleVoicePerProvider);

      const plan = computeCategoryRenderPlan({
        category: "tts",
        mode: "narrator",
        localOptions: localVoices,
        cloudOptions: singleCloudOpts,
        cloudProviders: cloudProvidersTts,
        selectedProviderId: "conn-gemini",
      });

      expect(plan.mode).toBe("cascading");
      expect(plan.cascadingModelOptions).toHaveLength(1);
      expect(plan.cascadingModelIsFixed).toBe(true);
      expect(plan.fixedOption?.id).toBe("cloud:gem-puck");
    });
  });

  describe("Category Render Plan Table across all modes and counts", () => {
    const categories: CastCategory[] = ["tts", "llm"];
    const modeCounts = [
      { modeName: "Local-Only", cloudCount: 0 },
      { modeName: "Single-Cloud", cloudCount: 1 },
      { modeName: "Multi-Provider", cloudCount: 2 },
    ] as const;

    for (const cat of categories) {
      for (const mc of modeCounts) {
        it(`${cat} in ${mc.modeName} behaves deterministically`, () => {
          const dummyLocal = [
            {
              id: "l1",
              name: "Local 1",
              providerId: "local",
              providerName: "Local",
              isLocal: true,
            },
            {
              id: "l2",
              name: "Local 2",
              providerId: "local",
              providerName: "Local",
              isLocal: true,
            },
          ];
          const dummyCloudProviders: ProviderOption[] = [];
          const dummyCloudOptions: ModelOption[] = [];
          for (let i = 0; i < mc.cloudCount; i++) {
            const pId = `provider-${i + 1}`;
            dummyCloudProviders.push({
              id: pId,
              name: `Provider ${i + 1}`,
              isLocal: false,
            });
            dummyCloudOptions.push({
              id: `c-model-${i + 1}`,
              name: `Model ${i + 1}`,
              providerId: pId,
              providerName: `Provider ${i + 1}`,
              isLocal: false,
            });
          }

          const plan = computeCategoryRenderPlan({
            category: cat,
            mode: "teacher",
            localOptions: dummyLocal,
            cloudOptions: dummyCloudOptions,
            cloudProviders: dummyCloudProviders,
          });

          if (mc.cloudCount === 0) {
            expect(plan.mode).toBe("dropdown");
          } else if (mc.cloudCount === 1) {
            expect(plan.mode).toBe("dropdown");
            expect(plan.totalOptionsCount).toBe(dummyLocal.length + 1);
          } else {
            expect(plan.mode).toBe("cascading");
            expect(plan.providers[0].id).toBe("local");
          }
        });
      }
    }
  });
});
