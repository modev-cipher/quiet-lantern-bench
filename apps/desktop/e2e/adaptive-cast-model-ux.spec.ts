import { expect, test, type Page } from "@playwright/test";
import { castStep } from "./castSteps";
import { msg } from "./i18nText";
import {
  fixtureBook,
  fixtureCatalogModels,
  fixtureChapters,
  fixtureHardware,
  installTauriMock,
  type CastProviderScenario,
} from "./tauriMock";

const SINGLE_CLOUD_TTS: CastProviderScenario = {
  scriptConnections: [],
  ttsVoices: [
    {
      voiceProfileId: "vp-cart-1",
      name: "Warm narrator (Cartesia)",
      voiceId: "voice-uuid-1",
      connectionId: "c-cart",
      connectionName: "Cartesia",
      provider: "cartesia_tts",
      model: "sonic-3.6",
      loopback: false,
    },
  ],
};

const SINGLE_CLOUD_LLM: CastProviderScenario = {
  scriptConnections: [
    {
      connectionId: "c-gem",
      name: "Google Gemini",
      provider: "gemini",
      defaultModel: "gemini-2.5-flash",
      loopback: false,
    },
  ],
  ttsVoices: [],
};

const MULTI_CLOUD_ALL: CastProviderScenario = {
  scriptConnections: [
    {
      connectionId: "c-gem",
      name: "Google Gemini",
      provider: "gemini",
      defaultModel: "gemini-2.5-flash",
      loopback: false,
    },
    {
      connectionId: "c-oai",
      name: "OpenAI",
      provider: "openai",
      defaultModel: "gpt-4o-mini",
      loopback: false,
    },
  ],
  ttsVoices: [
    {
      voiceProfileId: "vp-cart-1",
      name: "Cartesia Warm",
      voiceId: "cart-1",
      connectionId: "c-cart",
      connectionName: "Cartesia",
      provider: "cartesia_tts",
      model: "sonic-3.6",
      loopback: false,
    },
    {
      voiceProfileId: "vp-oai-1",
      name: "OpenAI Alloy",
      voiceId: "alloy",
      connectionId: "c-oai-tts",
      connectionName: "OpenAI TTS",
      provider: "openai_tts",
      model: "tts-1",
      loopback: false,
    },
  ],
};

const DEFAULT_MODELS = {
  catalogModels: fixtureCatalogModels(),
  hardware: fixtureHardware(),
  installed: [{ modelId: "kokoro-82m", diskBytes: 355_000_000 }],
};

const MULTI_LOCAL_MODELS = {
  catalogModels: fixtureCatalogModels(),
  hardware: fixtureHardware(),
  installed: [
    { modelId: "kokoro-82m", diskBytes: 355_000_000 },
    { modelId: "qwen3.5-9b-instruct-q4km", diskBytes: 5_700_000_000 },
    { modelId: "gemma4-e4b-q4km", diskBytes: 4_000_000_000 },
  ],
};

async function openCastModal(
  page: Page,
  options?: {
    castProviders?: CastProviderScenario;
    models?: typeof DEFAULT_MODELS;
    voiceProfiles?: Array<{
      id: string;
      name: string;
      engine: string;
      kind: "preset" | "cloned";
      presetVoiceId?: string;
    }>;
  },
) {
  await installTauriMock(page, {
    books: [fixtureBook()],
    onboardingComplete: true,
    modelScenario: options?.models ?? DEFAULT_MODELS,
    castProviders: options?.castProviders,
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 250,
      outputPath: "/exports/fixture-book.mp3",
      voiceProfiles: options?.voiceProfiles,
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await expect(page.getByTestId("job-modal")).toBeVisible();
  await castStep(page, "sound");
}

test.describe("Adaptive & Progressive Model Selection UX", () => {
  test("Local-Only Mode: single voice selector renders local voice dropdown and no cloud provider controls", async ({
    page,
  }) => {
    // 0 cloud providers configured and only 1 voice profile
    await openCastModal(page, {
      castProviders: undefined,
      voiceProfiles: [
        {
          id: "preset-kokoro-af-heart",
          name: "Heart · American English (female)",
          engine: "kokoro-82m",
          kind: "preset",
          presetVoiceId: "af_heart",
        },
      ],
    });

    // Zero cloud elements visible
    await expect(
      page.getByTestId("cast-cascade-provider-tts"),
    ).not.toBeVisible();
    await expect(
      page.getByTestId("cast-cascade-provider-llm"),
    ).not.toBeVisible();
    await expect(page.getByTestId("job-modal")).not.toContainText(/provider/i);

    // Segmented control is present with Local active
    await expect(page.getByTestId("voice-engine-local")).toHaveAttribute("aria-checked", "true");
    const voiceSelect = page.getByTestId("voice-select");
    await expect(voiceSelect).toBeVisible();
    await expect(voiceSelect).toContainText("Heart");
  });

  test("Local-Only Mode: single local LLM guard renders fixed summary in Teacher mode without marketing prompts", async ({
    page,
  }) => {
    // 0 cloud providers and 1 local LLM installed
    await openCastModal(page, {
      castProviders: undefined,
      models: {
        catalogModels: fixtureCatalogModels(),
        hardware: fixtureHardware(),
        installed: [
          { modelId: "kokoro-82m", diskBytes: 355_000_000 },
          { modelId: "qwen3.5-9b-instruct-q4km", diskBytes: 5_700_000_000 },
        ],
      },
    });

    await page.getByTestId("mode-teacher").click();

    // Single-option guard for LLM
    const fixedLlm = page.getByTestId("cast-fixed-llm");
    await expect(fixedLlm).toBeVisible();
    await expect(fixedLlm).toContainText(msg("models.role.teaching"));
    await expect(fixedLlm).toContainText(msg("cast.default"));

    // No raw marketing prompt
    await expect(page.getByTestId("job-modal")).not.toContainText(/Want to add Gemini/i);
    await expect(page.getByTestId("job-modal")).not.toContainText(/Connect in Providers/i);
  });

  test("Local-Only Mode: multiple voices render clean local dropdown without cloud optgroups", async ({
    page,
  }) => {
    await openCastModal(page, {
      castProviders: undefined,
      voiceProfiles: [
        {
          id: "preset-kokoro-af-heart",
          name: "Heart · American English (female)",
          engine: "kokoro-82m",
          kind: "preset",
          presetVoiceId: "af_heart",
        },
        {
          id: "preset-kokoro-jm-kumo",
          name: "Kumo · Japanese (male)",
          engine: "kokoro-82m",
          kind: "preset",
          presetVoiceId: "jm_kumo",
        },
      ],
    });

    const voiceSelect = page.getByTestId("voice-select");
    await expect(voiceSelect).toBeVisible();
    // Verify optgroups contain only language names with flags and no cloud text
    await expect(voiceSelect.locator("optgroup")).toHaveCount(2);
    await expect(voiceSelect).not.toContainText(/Cloud Voices/i);
    await expect(voiceSelect).not.toContainText(/On this computer/i);
  });

  test("Local-Only Mode: voices whose engine is not installed are not offered", async ({
    page,
  }) => {
    await openCastModal(page, {
      models: {
        catalogModels: fixtureCatalogModels(),
        hardware: fixtureHardware(),
        installed: [{ modelId: "studio-tts", diskBytes: 2_100_000_000 }],
      },
      voiceProfiles: [
        {
          id: "preset-kokoro-af-heart",
          name: "Heart · American English (female)",
          engine: "kokoro-82m",
          kind: "preset",
          presetVoiceId: "af_heart",
        },
        {
          id: "clone-studio-1",
          name: "My cloned voice",
          engine: "studio-tts",
          kind: "cloned",
        },
      ],
    });

    const voiceSelect = page.getByTestId("voice-select");
    await expect(voiceSelect).toBeVisible();
    await expect(voiceSelect).toContainText("My cloned voice");
    await expect(voiceSelect).not.toContainText("Heart");
  });

  test("Local-Only Mode: no installed voice engine keeps the install path, never an empty voice list", async ({
    page,
  }) => {
    await openCastModal(page, {
      models: {
        catalogModels: fixtureCatalogModels(),
        hardware: fixtureHardware(),
        installed: [],
      },
      voiceProfiles: [
        {
          id: "preset-kokoro-af-heart",
          name: "Heart · American English (female)",
          engine: "kokoro-82m",
          kind: "preset",
          presetVoiceId: "af_heart",
        },
      ],
    });

    await expect(page.getByTestId("voice-required")).toBeVisible();
    await expect(page.getByTestId("voice-select")).not.toBeVisible();
  });

  test("Local-Only Mode: no installed language model hides the Teacher picker and names the gap", async ({
    page,
  }) => {
    await openCastModal(page, {
      models: {
        catalogModels: fixtureCatalogModels(),
        hardware: fixtureHardware(),
        installed: [{ modelId: "kokoro-82m", diskBytes: 355_000_000 }],
      },
    });

    await page.getByTestId("mode-teacher").click();
    await expect(page.getByTestId("missing-model-warning")).toBeVisible();

    await page.getByTestId("teacher-where-cloud").click();
    const empty = page.getByTestId("cast-no-cloud-script");
    await expect(empty).toContainText(msg("cast.noCloudScript"));
    await expect(page.getByTestId("cast-open-providers-script")).toHaveText(
      msg("cast.connectProviders"),
    );
    await expect(page.getByTestId("start-job")).toBeDisabled();
    await page.getByTestId("teacher-where-local").click();
    await expect(page.getByTestId("teacher-llm-select")).not.toBeVisible();
    await expect(page.getByTestId("teacher-llm-select-wrapper")).not.toBeVisible();
    await expect(page.getByTestId("cast-fixed-llm")).not.toBeVisible();
  });

  test("Single Cloud Provider Mode: reveals voice options only for configured provider with functional separation", async ({
    page,
  }) => {
    // Only 1 cloud TTS provider configured
    await openCastModal(page, {
      castProviders: SINGLE_CLOUD_TTS,
      voiceProfiles: [
        {
          id: "preset-kokoro-af-heart",
          name: "Heart · American English (female)",
          engine: "kokoro-82m",
          kind: "preset",
          presetVoiceId: "af_heart",
        },
        {
          id: "preset-kokoro-jm-kumo",
          name: "Kumo · Japanese (male)",
          engine: "kokoro-82m",
          kind: "preset",
          presetVoiceId: "jm_kumo",
        },
      ],
    });

    // In Local mode, only local voices are visible
    const voiceSelect = page.getByTestId("voice-select");
    await expect(voiceSelect).toBeVisible();
    await expect(voiceSelect).toContainText("Heart");
    await expect(voiceSelect.locator("optgroup[label*='Cartesia']")).not.toBeAttached();

    // Switch to Cloud: reveals cascading controls for the configured provider
    await page.getByTestId("voice-engine-cloud").click();
    const providerSelect = page.getByTestId("cast-cascade-provider-tts");
    await expect(providerSelect).toBeVisible();
    await expect(providerSelect).toContainText("Cartesia");
    await expect(providerSelect).not.toContainText("OpenAI");
    await expect(providerSelect).not.toContainText("Gemini");

    const modelSelect = page.getByTestId("cast-cascade-model-tts");
    await expect(modelSelect).toBeVisible();
    await expect(modelSelect).toContainText("Warm narrator (Cartesia)");
  });

  test("Single Cloud Provider Mode: reveals LLM options only for configured provider in Teacher mode", async ({
    page,
  }) => {
    // Only 1 cloud LLM provider configured
    await openCastModal(page, {
      castProviders: SINGLE_CLOUD_LLM,
      models: MULTI_LOCAL_MODELS,
    });

    await page.getByTestId("mode-teacher").click();
    await expect(page.getByTestId("teacher-where-local")).toHaveAttribute(
      "aria-checked",
      "true",
    );
    await expect(page.getByTestId("teacher-llm-select-wrapper")).toBeVisible();
    await expect(page.getByTestId("job-modal")).not.toContainText("Google Gemini");

    await page.getByTestId("teacher-where-cloud").click();
    await expect(page.getByTestId("job-modal")).toContainText("Google Gemini");
    await expect(page.getByTestId("job-modal")).not.toContainText("Anthropic");
  });

  test("Multi-Provider Mode: cascading dropdowns independently per category with local-first initial selection", async ({
    page,
  }) => {
    await openCastModal(page, {
      castProviders: MULTI_CLOUD_ALL,
      models: MULTI_LOCAL_MODELS,
      voiceProfiles: [
        {
          id: "preset-kokoro-af-heart",
          name: "Heart · American English (female)",
          engine: "kokoro-82m",
          kind: "preset",
          presetVoiceId: "af_heart",
        },
        {
          id: "preset-kokoro-jm-kumo",
          name: "Kumo · Japanese (male)",
          engine: "kokoro-82m",
          kind: "preset",
          presetVoiceId: "jm_kumo",
        },
      ],
    });

    // Switch to Teacher mode to check LLM cascading
    await page.getByTestId("mode-teacher").click();

    // 1. This computer is ALWAYS the initial choice, with the local models
    await expect(page.getByTestId("teacher-where-local")).toHaveAttribute(
      "aria-checked",
      "true",
    );
    await expect(page.getByTestId("teacher-llm-select-wrapper")).toBeVisible();
    await expect(page.getByTestId("cast-cascade-provider-llm")).not.toBeVisible();

    // Cloud lists only the connected cloud providers, then their models
    await page.getByTestId("teacher-where-cloud").click();
    const llmProviderSelect = page.getByTestId("cast-cascade-provider-llm");
    await expect(llmProviderSelect).toBeVisible();
    await expect(llmProviderSelect.locator("option")).toHaveCount(2);
    await expect(llmProviderSelect).toHaveValue("c-gem");
    await expect(llmProviderSelect).not.toContainText(msg("cast.onDeviceLocal"));
    await expect(page.getByTestId("cast-cascade-model-llm")).toBeVisible();

    // 2. TTS Segmented Control & Cascading
    await expect(page.getByTestId("voice-engine-local")).toHaveAttribute("aria-checked", "true");
    await expect(page.getByTestId("voice-select")).toBeVisible();

    // Switch to Cloud: reveals cascading selectors
    await page.getByTestId("voice-engine-cloud").click();
    const ttsProviderSelect = page.getByTestId("cast-cascade-provider-tts");
    await expect(ttsProviderSelect).toBeVisible();
    await expect(ttsProviderSelect).toHaveValue("c-cart");

    const ttsModelSelect = page.getByTestId("cast-cascade-model-tts");
    await expect(ttsModelSelect).toBeVisible();
    await expect(ttsModelSelect).toContainText("Cartesia Warm");

    // Switching back to Local restores local selection independently
    await page.getByTestId("voice-engine-local").click();
    await expect(page.getByTestId("voice-select")).toBeVisible();
  });
});

