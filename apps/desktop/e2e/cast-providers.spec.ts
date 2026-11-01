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

// Provider pickers live inside the adaptive model sections (provider →
// model cascades); the writer/consent/cost laws are pinned in Rust and the
// picker model in vitest. GALLERY_SHOTS=1 additionally captures the design
// gallery states in both themes.

const CLOUD: CastProviderScenario = {
  scriptConnections: [
    {
      connectionId: "c-gem",
      name: "Work Gemini",
      provider: "gemini",
      defaultModel: "gemini-2.5-flash",
      loopback: false,
    },
    {
      connectionId: "c-ollama",
      name: "Local Ollama",
      provider: "openai_compatible",
      defaultModel: "llama3",
      loopback: true,
    },
  ],
  ttsVoices: [
    {
      voiceProfileId: "vp-cart",
      name: "Warm narrator (Cartesia)",
      voiceId: "voice-uuid-1",
      connectionId: "c-cart",
      connectionName: "Cartesia",
      provider: "cartesia_tts",
      model: "sonic-3.6",
      loopback: false,
    },
  ],
  cost: {
    state: "known",
    usd_low: 1.2,
    usd_high: 1.56,
    currency: "USD",
    as_of: "2026-08-28",
    basis: "~2,000 scoped source characters; output = input x 1.5 (fixture basis)",
  },
  consentRequired: [
    {
      phase: "script",
      connectionId: "c-gem",
      connectionName: "Work Gemini",
      provider: "gemini",
      hosts: ["generativelanguage.googleapis.com"],
      dataCategories: ["source_text"],
      retentionUrl: "https://ai.google.dev/gemini-api/terms",
    },
  ],
};

const MODELS = {
  catalogModels: fixtureCatalogModels(),
  hardware: fixtureHardware(),
  installed: [
    { modelId: "kokoro-82m", diskBytes: 355_000_000 },
    { modelId: "qwen3.5-9b-instruct-q4km", diskBytes: 5_700_000_000 },
  ],
};

async function openModal(page: Page, cast?: CastProviderScenario) {
  await installTauriMock(page, {
    books: [fixtureBook()],
    onboardingComplete: true,
    modelScenario: MODELS,
    castProviders: cast,
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 250,
      outputPath: "/exports/fixture-book.mp3",
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await expect(page.getByTestId("job-modal")).toBeVisible();
  await castStep(page, "sound");
}

const gallery = process.env.GALLERY_SHOTS === "1";
const OUT = "../../docs/design/gallery";

async function shotBothThemes(page: Page, name: string) {
  if (!gallery) return;
  for (const theme of ["light", "dark"]) {
    await page.evaluate((value) => {
      document.documentElement.setAttribute("data-theme", value);
    }, theme);
    await page.waitForTimeout(300);
    await page.screenshot({ path: `${OUT}/cast-providers-${name}-${theme}.png` });
  }
  await page.evaluate(() => {
    document.documentElement.setAttribute("data-theme", "light");
  });
}

test("zero connections: no provider controls, no provider vocabulary, zero provider traffic, and a clean local cast (T7)", async ({
  page,
}) => {
  await openModal(page, undefined);
  await expect(
    page.getByTestId("cast-cascade-provider-llm"),
  ).not.toBeVisible();
  await expect(
    page.getByTestId("cast-cascade-provider-tts"),
  ).not.toBeVisible();
  await expect(page.getByTestId("figure-section")).not.toContainText(/provider/i);
  await expect(page.getByTestId("job-modal")).not.toContainText(/provider/i);
  await shotBothThemes(page, "zero-connections");
  // A local cast starts and its args carry NO writer-owned cloud keys, and
  // no model-fetch/estimate/consent IPC ever fired.
  await expect(page.getByTestId("eta-label")).toContainText("~");
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-modal")).not.toBeVisible();
  const calls = await page.evaluate(
    () => (window as unknown as { __mockCalls?: Record<string, number> }).__mockCalls ?? {},
  );
  for (const command of [
    "provider_fetch_models",
    "estimate_cast_cost",
    "cast_consent_grant",
  ]) {
    expect(calls[command] ?? 0).toBe(0);
  }
  const jobArgs = await page.evaluate(
    () =>
      (window as unknown as { __createJobCalls?: Array<Record<string, unknown>> })
        .__createJobCalls ?? [],
  );
  expect(jobArgs.length).toBe(1);
  expect(jobArgs[0]).not.toHaveProperty("scriptCloud");
  expect(jobArgs[0]).not.toHaveProperty("ttsCloud");
});

test("configured connections render Local-first cascades with Local active (T4)", async ({
  page,
}) => {
  await openModal(page, CLOUD);
  // Narrator: no teacher scripting control, and the voice picker defaults
  // to a local voice.
  await expect(
    page.getByTestId("cast-cascade-provider-llm"),
  ).not.toBeVisible();
  const voiceSelect = page.getByTestId("voice-select");
  await expect(voiceSelect).toBeVisible();
  expect(await voiceSelect.inputValue()).not.toMatch(/^cloud:/);

  // Teacher: This computer is active first; Cloud lists the connections.
  await page.getByTestId("mode-teacher").click();
  await expect(page.getByTestId("teacher-where-local")).toHaveAttribute(
    "aria-checked",
    "true",
  );
  await expect(
    page.getByTestId("cast-cascade-provider-llm"),
  ).not.toBeVisible();
  await page.getByTestId("teacher-where-cloud").click();
  const providerSelect = page.getByTestId("cast-cascade-provider-llm");
  await expect(providerSelect).toBeVisible();
  const providers = providerSelect.locator("option");
  await expect(providers).toHaveCount(2);
  await expect(providers.nth(0)).toHaveAttribute("value", "c-gem");
  await expect(providers.nth(1)).toHaveAttribute("value", "c-ollama");
  await expect(providerSelect).toHaveValue("c-gem");

  // Loopback rows carry the this-computer fact, never cost copy (A18/T14).
  await providerSelect.selectOption("c-ollama");
  await expect(page.getByTestId("cast-loopback-script")).toContainText(
    msg("cast.loopbackFact"),
  );
  await expect(page.getByTestId("cast-cost-script")).not.toBeVisible();
  await shotBothThemes(page, "local-default");
});

test("a cloud script selection shows the model string and the cost truth (T12)", async ({
  page,
}) => {
  await openModal(page, CLOUD);
  await page.getByTestId("mode-teacher").click();
  await page.getByTestId("teacher-where-cloud").click();
  await page.getByTestId("cast-cascade-provider-llm").selectOption("c-gem");
  await expect(page.getByTestId("cast-cascade-model-llm")).toHaveValue(
    "cloud:c-gem:gemini-2.5-flash",
  );
  await expect(page.getByTestId("cast-cost-script")).toContainText(
    "~$1.20–~$1.56 USD",
  );
  await expect(page.getByTestId("cast-cost-basis-script")).toContainText(
    "fixture basis",
  );
  await expect(page.getByTestId("cast-cost-script")).toContainText(
    msg("cast.costKnown", { range: "~$1.20–~$1.56 USD", asOf: "2026-08-28" }),
  );
  await expect(page.getByTestId("cast-cost-script")).toContainText(
    "ETA depends on your network",
  );
  await shotBothThemes(page, "cloud-selected");
});

test("an unknown rate says the provider-check line, never a zero (T12)", async ({
  page,
}) => {
  await openModal(page, { ...CLOUD, cost: undefined });
  await page.getByTestId("mode-teacher").click();
  await page.getByTestId("teacher-where-cloud").click();
  await page.getByTestId("cast-cascade-provider-llm").selectOption("c-gem");
  await expect(page.getByTestId("cast-cost-script")).toContainText(
    "Cost unavailable from this app - check your provider",
  );
  await expect(page.getByTestId("cast-cost-script")).not.toContainText("$0");
  await shotBothThemes(page, "cost-unknown");
});

test("start with an unconsented remote phase opens the consent sheet; decline returns to the untouched form", async ({
  page,
}) => {
  await openModal(page, CLOUD);
  await page.getByTestId("mode-teacher").click();
  await page.getByTestId("teacher-where-cloud").click();
  const providerSelect = page.getByTestId("cast-cascade-provider-llm");
  await providerSelect.selectOption("c-gem");
  await expect(page.getByTestId("eta-label")).toContainText("~");
  await page.getByTestId("start-job").click();
  const sheet = page.getByTestId("cast-consent-sheet");
  await expect(sheet).toBeVisible();
  await expect(sheet).toContainText("generativelanguage.googleapis.com");
  await expect(sheet).toContainText(msg("cast.data.sourceText"));
  await expect(sheet).toContainText("Work Gemini");
  await expect(sheet).toContainText(msg("cast.consentKey"));
  await shotBothThemes(page, "consent-sheet");
  await page.getByTestId("cast-consent-decline").click();
  await expect(sheet).not.toBeVisible();
  await expect(page.getByTestId("job-modal")).toBeVisible();
  await expect(providerSelect).toHaveValue("c-gem");
});

test("granting consent records it and the cast starts", async ({ page }) => {
  await openModal(page, CLOUD);
  await page.getByTestId("mode-teacher").click();
  await page.getByTestId("teacher-where-cloud").click();
  await page.getByTestId("cast-cascade-provider-llm").selectOption("c-gem");
  await expect(page.getByTestId("eta-label")).toContainText("~");
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("cast-consent-sheet")).toBeVisible();
  await page.getByTestId("cast-consent-grant").click();
  await expect(page.getByTestId("job-modal")).not.toBeVisible();
});

test("the A22 align advisory appears for a timestamp-less OpenAI TTS voice with the Models action", async ({
  page,
}) => {
  // Cartesia is the native-timing provider; the honest A22 fixture is the
  // deliberately timestamp-less OpenAI TTS voice.
  await openModal(page, {
    ...CLOUD,
    ttsVoices: [
      {
        voiceProfileId: "vp-oai",
        name: "Alloy (OpenAI)",
        voiceId: "alloy",
        connectionId: "c-oai",
        connectionName: "OpenAI TTS",
        provider: "openai_tts",
        model: "tts-1",
        loopback: false,
      },
    ],
    alignAdvisory: true,
  });
  await page.getByTestId("voice-engine-cloud").click();
  await page.getByTestId("cast-cascade-model-tts").selectOption("vp-oai");
  await expect(page.getByTestId("cast-cost-tts")).toBeVisible();
  await expect(page.getByTestId("cast-align-advisory")).toContainText(msg("voice.alignAdvisory"));
  await expect(
    page.getByTestId("cast-align-advisory-models"),
  ).toContainText(msg("voice.installAlignment"));
  await shotBothThemes(page, "align-advisory");
});

test("on the reader engine, the word timing link offers the full engine", async ({
  page,
}) => {
  await openModal(page, {
    ...CLOUD,
    ttsVoices: [
      {
        voiceProfileId: "vp-oai",
        name: "Alloy (OpenAI)",
        voiceId: "alloy",
        connectionId: "c-oai",
        connectionName: "OpenAI TTS",
        provider: "openai_tts",
        model: "tts-1",
        loopback: false,
      },
    ],
    alignAdvisory: true,
  });
  await page.evaluate(() => {
    (
      window as unknown as { __mockEngine: { variant: string } }
    ).__mockEngine.variant = "lite";
  });
  await page.getByTestId("voice-engine-cloud").click();
  await page.getByTestId("cast-cascade-model-tts").selectOption("vp-oai");
  await page.getByTestId("cast-align-advisory-models").click();
  await expect(page.getByTestId("engine-setup-sheet")).toBeVisible();
});

test("fetching models fills the suggestion list and applies the tier default", async ({
  page,
}) => {
  await openModal(page, {
    ...CLOUD,
    alignAdvisory: true,
    ttsVoices: [{ voiceProfileId: "vp-oai", name: "Alloy (OpenAI)", voiceId: "alloy", connectionId: "c-oai", connectionName: "OpenAI TTS", provider: "openai_tts", model: "tts-1", loopback: false }],
    fetchedModels: {
      models: ["gemini-3.7-flash", "gemini-3.5-flash", "gemini-2.5-flash"],
      suggestedDefault: "gemini-3.7-flash",
    },
  });
  await page.getByTestId("mode-teacher").click();
  await page.getByTestId("teacher-where-cloud").click();
  await page.getByTestId("cast-cascade-provider-llm").selectOption("c-gem");
  const modelSelect = page.getByTestId("cast-cascade-model-llm");
  await expect(modelSelect).toHaveValue("cloud:c-gem:gemini-2.5-flash");
  await page.getByTestId("cast-fetch-models").click();
  await expect(modelSelect).toHaveValue("cloud:c-gem:gemini-3.7-flash");
  await page.getByTestId("voice-engine-cloud").click();
  await page.getByTestId("cast-align-advisory-models").click();
  await page.getByTestId("cast-draft-return").click();
  await expect(page.getByTestId("cast-cascade-model-llm")).toHaveValue("cloud:c-gem:gemini-3.7-flash");

  const modelOptions = modelSelect.locator("option");
  await expect(modelOptions).toHaveCount(4);
  await expect(modelOptions.nth(0)).toHaveAttribute(
    "value",
    "cloud:c-gem:gemini-3.7-flash",
  );
  // Manual entry still wins after a fetch (T5).
  await modelSelect.selectOption("__custom__");
  const customInput = page.getByTestId("cast-script-model");
  await expect(customInput).toBeVisible();
  await expect(customInput).toHaveValue("gemini-3.7-flash");
  await customInput.fill("my-custom-model");
  await expect(customInput).toHaveValue("my-custom-model");
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("cast-consent-sheet")).toBeVisible();
  await page.getByTestId("cast-consent-grant").click();
  await expect(page.getByTestId("job-modal")).not.toBeVisible();
  const jobArgs = await page.evaluate(
    () =>
      (window as unknown as { __createJobCalls?: Array<Record<string, unknown>> })
        .__createJobCalls ?? [],
  );
  expect(jobArgs[jobArgs.length - 1]).toMatchObject({
    scriptCloud: { connectionId: "c-gem", model: "my-custom-model" },
  });
});

test("a provider-failed job offers explicit recovery: same binding or local", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    onboardingComplete: true,
    modelScenario: MODELS,
    jobScenario: {
      chapters: fixtureChapters(),
      seededJobs: [
        {
          id: "job-cloud-failed",
          bookId: "book-fixture",
          scope: { kind: "chapters", chapter_ids: ["ch-1"] },
          mode: "teacher",
          voiceProfileId: "preset-kokoro-af-heart",
          outputFormat: "mp3",
          modelOverridesJson: null,
          state: "FAILED",
          overnight: false,
          etaSeconds: null,
          reportJson: JSON.stringify({
            error: "cloud script over connection 'Work Gemini' failed",
            provider_failure: {
              class: "quota",
              retryable: true,
              retry_after_secs: 30,
              detail: "provider returned HTTP 429",
            },
          }),
          outputPathsJson: null,
          createdAt: Date.now() - 1000,
          updatedAt: Date.now() - 1000,
        },
      ],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-jobs").click();
  await page.getByTestId("open-production").click();
  await expect(page.getByTestId("job-row-job-cloud-failed")).toBeVisible();
  const recovery = page.getByTestId("job-provider-recovery-job-cloud-failed");
  await expect(recovery).toBeVisible();
  await expect(recovery).toContainText(msg("job.provider.rateLimited", { n: 30 }));
  // Retryable failure: the same-binding remedy is offered (a non-retryable
  // class hides it — pinned in vitest), plus explicit local and re-cast
  // remedies.
  await expect(
    page.getByTestId("job-retry-provider-job-cloud-failed"),
  ).toContainText(msg("job.retrySameProvider"));
  await expect(
    page.getByTestId("job-recast-job-cloud-failed"),
  ).toContainText(msg("job.newCastOtherProvider"));
  // Switch-to-local is CONFIRMED, with the consequence spelled out, never
  // immediate (Hermes R2 blocker 7).
  await page.getByTestId("job-retry-local-job-cloud-failed").click();
  const confirm = page.getByTestId("job-local-confirm-job-cloud-failed");
  await expect(confirm).toContainText(msg("job.switchLocalQuestion"));
  await shotBothThemes(page, "provider-recovery");
  await page.getByTestId("job-retry-local-confirm-job-cloud-failed").click();
  await expect(confirm).not.toBeVisible();
});

test("switching a failed cast to local without its engine offers the setup", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    onboardingComplete: true,
    modelScenario: MODELS,
    jobScenario: {
      localRetryError: "ERR_ENGINE_LOCAL_AI_REQUIRED:localAi",
      chapters: fixtureChapters(),
      seededJobs: [
        {
          id: "job-cloud-failed",
          bookId: "book-fixture",
          scope: { kind: "chapters", chapter_ids: ["ch-1"] },
          mode: "teacher",
          voiceProfileId: "preset-kokoro-af-heart",
          outputFormat: "mp3",
          modelOverridesJson: null,
          state: "FAILED",
          overnight: false,
          etaSeconds: null,
          reportJson: JSON.stringify({
            error: "cloud script over connection 'Work Gemini' failed",
            provider_failure: {
              class: "quota",
              retryable: true,
              retry_after_secs: 30,
              detail: "provider returned HTTP 429",
            },
          }),
          outputPathsJson: null,
          createdAt: Date.now() - 1000,
          updatedAt: Date.now() - 1000,
        },
      ],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-jobs").click();
  await page.getByTestId("open-production").click();
  await expect(page.getByTestId("job-row-job-cloud-failed")).toBeVisible();
  const recovery = page.getByTestId("job-provider-recovery-job-cloud-failed");
  await expect(recovery).toBeVisible();
  await expect(recovery).toContainText(msg("job.provider.rateLimited", { n: 30 }));
  await page.getByTestId("job-retry-local-job-cloud-failed").click();
  await page.getByTestId("job-retry-local-confirm-job-cloud-failed").click();
  await expect(page.getByTestId("engine-setup-sheet")).toBeVisible();
});

test("a recovered job's terminal card shows the calm recovery line (§5.5 visible history)", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    onboardingComplete: true,
    modelScenario: MODELS,
    jobScenario: {
      chapters: fixtureChapters(),
      seededJobs: [
        {
          id: "job-recovered-done",
          bookId: "book-fixture",
          scope: { kind: "chapters", chapter_ids: ["ch-1"] },
          mode: "teacher",
          voiceProfileId: "preset-kokoro-af-heart",
          outputFormat: "mp3",
          modelOverridesJson: null,
          state: "DONE",
          overnight: false,
          etaSeconds: null,
          reportJson: JSON.stringify({
            provider_recovery_log: [
              { action: "retry_same_binding", plan_rev: 1, at_unix: 1 },
              { action: "switch_to_local", plan_rev: 2, at_unix: 2 },
            ],
            provider_plan_rev: 2,
          }),
          outputPathsJson: JSON.stringify(["/exports/fixture-book.mp3"]),
          createdAt: Date.now() - 1000,
          updatedAt: Date.now() - 1000,
        },
      ],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-jobs").click();
  await page.getByTestId("open-production").click();
  await expect(
    page.getByTestId("job-recovered-job-recovered-done"),
  ).toContainText(
    msg("job.recovered", {
      what: msg("job.recoveredLocal"),
      attempt: msg("job.attempt", { n: 2 }),
    }),
  );
});

test("narrator mode makes cloud scripting unpickable and snaps it back (F10)", async ({
  page,
}) => {
  await openModal(page, CLOUD);
  // Narrator is the default: the inline law is visible, and there is no
  // scripting picker at all — cloud scripting is unpickable, not disabled.
  await expect(page.getByTestId("cast-script-narrator-note")).toContainText(
    msg("cast.narratorNote.title"),
  );
  await expect(
    page.getByTestId("cast-cascade-provider-llm"),
  ).not.toBeVisible();
  // Teacher unlocks cloud scripting; flipping back to Narrator snaps the
  // selection home instead of queueing a stage-time refusal.
  await page.getByTestId("mode-teacher").click();
  await page.getByTestId("teacher-where-cloud").click();
  const providerSelect = page.getByTestId("cast-cascade-provider-llm");
  await providerSelect.selectOption("c-gem");
  await expect(providerSelect).toHaveValue("c-gem");
  await page.getByTestId("mode-narrator").click();
  await expect(page.getByTestId("cast-script-narrator-note")).toBeVisible();
  await page.getByTestId("mode-teacher").click();
  await expect(page.getByTestId("teacher-where-local")).toHaveAttribute(
    "aria-checked",
    "true",
  );
  await expect(
    page.getByTestId("cast-cascade-provider-llm"),
  ).not.toBeVisible();
});

const VISION_CONSENT = {
  phase: "vision",
  connectionId: "c-gem",
  connectionName: "Work Gemini",
  provider: "gemini",
  hosts: ["generativelanguage.googleapis.com"],
  dataCategories: ["figure_crops"],
  retentionUrl: "https://ai.google.dev/gemini-api/terms",
};

for (const [name, consentRequired, title] of [
  [
    "a cloud script with figures asks for text and figure images together",
    [CLOUD.consentRequired![0]!, VISION_CONSENT],
    "cast.consentTitleTextAndImages",
  ],
  [
    "a figures-only consent names the images, not the book's text",
    [VISION_CONSENT],
    "cast.consentTitleImages",
  ],
] as const) {
  test(name, async ({ page }) => {
    await openModal(page, { ...CLOUD, consentRequired: [...consentRequired] });
    await page.getByTestId("mode-teacher").click();
    await page.getByTestId("teacher-where-cloud").click();
    await page.getByTestId("cast-cascade-provider-llm").selectOption("c-gem");
    await expect(page.getByTestId("eta-label")).toContainText("~");
    await page.getByTestId("start-job").click();
    const sheet = page.getByTestId("cast-consent-sheet");
    await expect(sheet).toBeVisible();
    await expect(sheet).toHaveAttribute("aria-label", msg(title));
    await expect(sheet).toContainText(msg("cast.phaseFigures"));
    await expect(sheet).toContainText(msg("cast.data.figureCrops"));
    await expect(sheet).not.toContainText("figure_crops");
    await expect(sheet).not.toContainText(msg("cast.phaseAudio"));
  });
}
