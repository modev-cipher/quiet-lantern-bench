import { expect, test, type Page } from "@playwright/test";
import type { CastProviderOptions } from "../src/castProviderModel";
import { castStep } from "./castSteps";
import { msg } from "./i18nText";
import {
  fixtureBook,
  fixtureCatalogModels,
  fixtureChapters,
  fixtureHardware,
  installTauriMock,
} from "./tauriMock";

test.use({ viewport: { width: 900, height: 600 } });

async function replaceCastOptions(page: Page, options: CastProviderOptions) {
  await page.evaluate((next) => {
    const internals = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            command: string,
            args?: Record<string, unknown>,
            options?: unknown,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const original = internals.invoke.bind(internals);
    internals.invoke = (command, args, invokeOptions) =>
      command === "cast_provider_options"
        ? Promise.resolve(next)
        : original(command, args, invokeOptions);
  }, options);
}

test("a provider setup detour resumes the same cast and closing discards it", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    onboardingComplete: true,
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 250,
      outputPath: "/fixture.mp3",
    },
    modelScenario: {
      catalogModels: fixtureCatalogModels(),
      hardware: fixtureHardware(),
      installed: [{ modelId: "kokoro-82m", diskBytes: 355_000_000 }],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.getByTestId("scope-pages-tab").click();
  await page.getByTestId("page-start-input").fill("3");
  await page.getByTestId("page-end-input").fill("5");
  await castStep(page, "sound");
  const localId = await page.getByTestId("voice-select").inputValue();
  const localName = await page.evaluate(async (id) => {
    const native = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            command: string,
          ) => Promise<Array<{ id: string; name: string }>>;
        };
      }
    ).__TAURI_INTERNALS__;
    return (
      (await native.invoke("list_voice_profiles")).find(
        (voice) => voice.id === id,
      )?.name ?? ""
    );
  }, localId);
  expect(localName).not.toBe("");
  await page.getByTestId("voice-engine-cloud").click();
  await page.getByTestId("cast-open-providers-btn").click();
  await expect(page.getByTestId("job-modal")).toHaveCount(0);
  await page.getByTestId("cast-draft-return").click();
  await expect(page.getByTestId("voice-engine-cloud")).toHaveAttribute(
    "aria-checked",
    "true",
  );
  await castStep(page, "review");
  await expect(page.getByTestId("phase-summary")).toHaveCount(0);
  await expect(page.getByTestId("eta-label")).toHaveCount(0);
  await expect(page.getByTestId("cast-step-sound")).not.toContainText(
    localName,
  );
  await page.getByTestId("cast-finish-cloud-choices").click();
  await expect(page.getByTestId("cast-step-sound")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await castStep(page, "scope");
  await expect(page.getByTestId("page-start-input")).toHaveValue("3");
  await expect(page.getByTestId("page-end-input")).toHaveValue("5");
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("cast-draft-return")).toHaveCount(0);
  await expect(page.getByTestId("create-audio-book-fixture")).toBeFocused();
  await page.getByTestId("create-audio-book-fixture").click();
  await expect(page.getByTestId("scope-chapters-tab")).toHaveAttribute(
    "aria-checked",
    "true",
  );
  await castStep(page, "sound");
  await expect(page.getByTestId("voice-engine-local")).toHaveAttribute(
    "aria-checked",
    "true",
  );
});

test("onboarding lets users change language and revisit choices without restarting downloads", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    onboardingComplete: false,
    modelScenario: {
      catalogModels: fixtureCatalogModels(),
      hardware: fixtureHardware(),
      downloadTicks: 30,
      downloadDelayMs: 100,
    },
  });
  await page.goto("/");
  await page.getByTestId("wizard-language-ar").click();
  await expect(page.locator("html")).toHaveAttribute("lang", "ar");
  await page.getByTestId("wizard-next").click();
  await page.getByTestId("wizard-option-kokoro-82m").click();
  await page.getByTestId("wizard-next").click();
  await page.getByTestId("wizard-back").click();
  await expect(page.getByTestId("wizard-heading")).toBeFocused();
  await expect(page.getByTestId("wizard-option-kokoro-82m")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await page.getByTestId("wizard-next").click();
  const calls = await page.evaluate(
    () =>
      (window as unknown as { __invokedCommands?: string[] })
        .__invokedCommands ?? [],
  );
  expect(calls.filter((name) => name === "download_model")).toHaveLength(1);
});

test("a model setup detour preserves chapter choices, teacher mode and figures", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    onboardingComplete: true,
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 250,
      outputPath: "/fixture.mp3",
    },
    modelScenario: {
      catalogModels: fixtureCatalogModels(),
      hardware: fixtureHardware(),
      installed: [{ modelId: "kokoro-82m", diskBytes: 355_000_000 }],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  const chapter = page
    .getByTestId("chapter-list")
    .getByRole("checkbox")
    .first();
  await chapter.uncheck();
  await castStep(page, "sound");
  await page.getByTestId("mode-teacher").click();
  await page.getByTestId("cast-explain-figures").uncheck();
  await castStep(page, "review");
  await page.getByTestId("get-missing-model").click();
  await expect(page.getByTestId("models-panel")).toBeVisible();
  await page.getByTestId("cast-draft-return").click();
  await expect(page.getByTestId("cast-step-review")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await castStep(page, "sound");
  await expect(page.getByTestId("mode-teacher")).toHaveAttribute(
    "aria-checked",
    "true",
  );
  await expect(page.getByTestId("cast-explain-figures")).not.toBeChecked();
  await castStep(page, "scope");
  await expect(
    page.getByTestId("chapter-list").getByRole("checkbox").first(),
  ).not.toBeChecked();
  const calls = await page.evaluate(
    () =>
      (window as unknown as { __invokedCommands: string[] }).__invokedCommands,
  );
  expect(calls).not.toContain("create_job");
});

test("removing a model requires an informed confirmation and Escape preserves it", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    onboardingComplete: true,
    modelScenario: {
      catalogModels: fixtureCatalogModels(),
      hardware: fixtureHardware(),
      installed: [{ modelId: "kokoro-82m", diskBytes: 355_000_000 }],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-models").click();
  await page.getByTestId("delete-model-kokoro-82m").first().click();
  await expect(page.getByTestId("delete-model-modal")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(
    page.getByTestId("delete-model-kokoro-82m").first(),
  ).toBeFocused();
  await page.getByTestId("delete-model-kokoro-82m").first().click();
  await page.getByTestId("confirm-delete-model-button").click();
  await expect(page.getByTestId("delete-model-kokoro-82m")).toHaveCount(0);
});

test("the empty shelf has a clearly named primary import action", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    onboardingComplete: true,
    importScenario: { path: "/fixtures/book.pdf", book: fixtureBook() },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  const action = page.getByTestId("empty-shelf-import");
  await expect(action).toBeVisible();
  await action.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("open-book-book-fixture")).toBeVisible();
});

test("a later usable Gemini slot enables the tutor when the first slot has no key", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    bookAgentProbe: "notInBuild",
    voiceKeysScenario: {
      entries: [
        {
          baseUrl: "https://generativelanguage.googleapis.com",
          provider: "gemini",
          slot: 0,
          state: "unchecked",
          keyPresent: false,
          last4: null,
          custom: false,
        },
        {
          baseUrl: "https://generativelanguage.googleapis.com",
          provider: "gemini",
          slot: 1,
          state: "active",
          keyPresent: true,
          last4: "abcd",
          custom: false,
        },
      ],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("voice-session-start")).toBeEnabled();
});

test("a tutor with only OpenAI directs setup to Gemini", async ({ page }) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    bookAgentProbe: "notInBuild",
    voiceKeysScenario: {
      entries: [
        {
          baseUrl: "https://api.openai.com/v1",
          provider: "openai_compat",
          state: "active",
          keyPresent: true,
          last4: "abcd",
          custom: false,
        },
      ],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("voice-session-start")).toBeDisabled();
  await expect(page.getByTestId("voice-session-key-hint")).toContainText(
    "Gemini",
  );
  await page.getByTestId("voice-session-key-action").click();
  await expect(page).toHaveURL(/#providers\/gemini$/);
});

test("Back cannot imply cancelling a first-run completion already submitted", async ({
  page,
}) => {
  await installTauriMock(page, { books: [], onboardingComplete: false });
  await page.goto("/");
  await page.getByTestId("wizard-next").click();
  await page.getByTestId("wizard-next").click();
  await page.evaluate(() => {
    const internals = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            command: string,
            args?: Record<string, unknown>,
            options?: unknown,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const original = internals.invoke.bind(internals);
    internals.invoke = (command, args, options) =>
      command === "set_onboarding_complete"
        ? new Promise((done) => {
            (
              window as unknown as { __finishWizard: () => void }
            ).__finishWizard = () => {
              void original(command, args, options).then(done);
            };
          })
        : original(command, args, options);
  });
  await page.getByTestId("wizard-finish").click();
  await expect(page.getByTestId("wizard-back")).toBeDisabled();
  await expect(page.getByTestId("wizard-skip-all")).toBeDisabled();
  await page.evaluate(() =>
    (window as unknown as { __finishWizard: () => void }).__finishWizard(),
  );
  await expect(page.getByTestId("wizard")).toHaveCount(0);
});

test("returning from setup adopts a new cloud voice and isolates a removed provider", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    onboardingComplete: true,
    castProviders: {
      scriptConnections: [],
      ttsVoices: [],
      alignAdvisory: true,
    },
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 250,
      outputPath: "/fixture.mp3",
    },
    modelScenario: {
      catalogModels: fixtureCatalogModels(),
      hardware: fixtureHardware(),
      installed: [{ modelId: "kokoro-82m", diskBytes: 355_000_000 }],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.getByTestId("scope-pages-tab").click();
  await page.getByTestId("page-start-input").fill("3");
  await page.getByTestId("page-end-input").fill("5");
  await castStep(page, "sound");
  await page.getByTestId("voice-engine-cloud").click();
  await page.getByTestId("cast-open-providers-btn").click();
  await replaceCastOptions(page, {
    scriptConnections: [],
    ttsVoices: [
      {
        voiceProfileId: "new-voice",
        name: "New voice",
        voiceId: "alloy",
        connectionId: "new-connection",
        connectionName: "OpenAI",
        provider: "openai_tts",
        model: "tts-1",
        loopback: false,
      },
    ],
    visionConnections: [],
  });
  await page.getByTestId("cast-draft-return").click();
  await expect(page.getByTestId("cast-cascade-model-tts")).toHaveValue(
    "new-voice",
  );
  await expect(page.getByTestId("cast-cost-tts")).toBeVisible();
  await page.getByTestId("cast-align-advisory-models").click();
  await replaceCastOptions(page, {
    scriptConnections: [],
    ttsVoices: [],
    visionConnections: [],
  });
  await page.getByTestId("cast-draft-return").click();
  await expect(page.getByTestId("voice-engine-local")).toHaveAttribute(
    "aria-checked",
    "true",
  );
  await castStep(page, "scope");
  await expect(page.getByTestId("page-start-input")).toHaveValue("3");
  await expect(page.getByTestId("page-end-input")).toHaveValue("5");
  const calls = await page.evaluate(
    () =>
      (window as unknown as { __invokedCommands: string[] }).__invokedCommands,
  );
  expect(calls).not.toContain("create_job");
});

test("Gemini setup opens even when the provider index is already mounted", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    onboardingComplete: true,
    bookAgentProbe: "notInBuild",
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-providers").click();
  await page.getByTestId("tutor-launcher").click();
  await page.getByTestId("voice-session-key-action").click();
  await expect(page).toHaveURL(/#providers\/gemini$/);
});

test("the model-removal confirmation remains reachable in an enlarged layout", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    onboardingComplete: true,
    modelScenario: {
      catalogModels: fixtureCatalogModels().map((model) =>
        model.id === "kokoro-82m"
          ? { ...model, display_name: "VeryLongModelName".repeat(15) }
          : model,
      ),
      hardware: fixtureHardware(),
      installed: [{ modelId: "kokoro-82m", diskBytes: 355_000_000 }],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-models").click();
  await page.getByTestId("delete-model-kokoro-82m").first().click();
  await page.setViewportSize({ width: 450, height: 300 });
  const dialog = page.getByTestId("delete-model-modal").getByRole("dialog");
  const bounds = await dialog.evaluate((element) => {
    const rect = element.getBoundingClientRect();
    return {
      top: rect.top,
      bottom: rect.bottom,
      width: rect.width,
      client: element.clientWidth,
      scroll: element.scrollWidth,
    };
  });
  expect(bounds.top).toBeGreaterThanOrEqual(0);
  expect(bounds.bottom).toBeLessThanOrEqual(300);
  expect(bounds.scroll).toBeLessThanOrEqual(bounds.client + 1);
  await page
    .getByTestId("confirm-delete-model-button")
    .scrollIntoViewIfNeeded();
  await expect(
    page.getByTestId("confirm-delete-model-button"),
  ).toBeInViewport();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("delete-model-modal")).toHaveCount(0);
});

test("a resumed cast keeps its scope through a failed metadata reload and Retry", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    onboardingComplete: true,
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 250,
      outputPath: "/fixture.mp3",
    },
    modelScenario: {
      catalogModels: fixtureCatalogModels(),
      hardware: fixtureHardware(),
      installed: [{ modelId: "kokoro-82m", diskBytes: 355_000_000 }],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.getByTestId("scope-pages-tab").click();
  await page.getByTestId("page-start-input").fill("3");
  await page.getByTestId("page-end-input").fill("5");
  await castStep(page, "sound");
  await page.getByTestId("voice-engine-cloud").click();
  await page.getByTestId("cast-open-providers-btn").click();
  await page.evaluate(() => {
    const state = window as unknown as {
      __castReadFailure: boolean;
      __TAURI_INTERNALS__: {
        invoke: (
          command: string,
          args?: Record<string, unknown>,
          options?: unknown,
        ) => Promise<unknown>;
      };
    };
    state.__castReadFailure = true;
    const original = state.__TAURI_INTERNALS__.invoke.bind(
      state.__TAURI_INTERNALS__,
    );
    state.__TAURI_INTERNALS__.invoke = (command, args, options) =>
      command === "get_catalog" && state.__castReadFailure
        ? Promise.reject("fixture catalog unavailable")
        : original(command, args, options);
  });
  await page.getByTestId("cast-draft-return").click();
  const dialog = page.getByTestId("job-modal").getByRole("dialog");
  const error = dialog.getByRole("alert");
  await expect(error).toBeVisible();
  await page.evaluate(() => {
    (window as unknown as { __castReadFailure: boolean }).__castReadFailure =
      false;
  });
  await error.getByRole("button").click();
  await expect(dialog).toBeFocused();
  await expect(error).toHaveCount(0);
  await castStep(page, "scope");
  await expect(page.getByTestId("page-start-input")).toHaveValue("3");
  await expect(page.getByTestId("page-end-input")).toHaveValue("5");
});

test("removing a draft's selected local model updates both the picker and execution summary", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    onboardingComplete: true,
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 250,
      outputPath: "/fixture.mp3",
    },
    modelScenario: {
      catalogModels: fixtureCatalogModels(),
      hardware: fixtureHardware(),
      installed: [
        { modelId: "kokoro-82m", diskBytes: 355_000_000 },
        { modelId: "qwen3.5-9b-instruct-q4km", diskBytes: 5_700_000_000 },
        { modelId: "qwen3.5-32b-class-q4km", diskBytes: 16_700_000_000 },
      ],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await castStep(page, "sound");
  await page.getByTestId("mode-teacher").click();
  await page.getByTestId("cast-explain-figures").uncheck();
  await page.getByTestId("teacher-llm-select-button").click();
  await page.keyboard.press("End");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("teacher-llm-select")).toHaveValue(
    "qwen3.5-32b-class-q4km",
  );
  await page.getByTestId("pick-faster-model").click();
  await page
    .getByTestId("installed-models")
    .getByTestId("delete-model-qwen3.5-32b-class-q4km")
    .click();
  await page.getByTestId("confirm-delete-model-button").click();
  await expect(
    page.getByTestId("delete-model-qwen3.5-32b-class-q4km"),
  ).toHaveCount(0);
  await page.getByTestId("cast-draft-return").click();
  const name = msg("models.name", {
    role: msg("models.role.teaching"),
    tier: msg("models.tier.proPlus"),
  });
  await expect(page.getByTestId("cast-fixed-llm")).toContainText(name);
  await castStep(page, "review");
  await expect(page.getByTestId("phase-summary-script")).toContainText(name);
});

test("multiple saved casts have distinct names and safe focus when discarded", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [
      fixtureBook(),
      fixtureBook({ id: "book-second", title: "Second Book" }),
    ],
    onboardingComplete: true,
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 250,
      outputPath: "/fixture.mp3",
    },
    modelScenario: {
      catalogModels: fixtureCatalogModels(),
      hardware: fixtureHardware(),
      installed: [{ modelId: "kokoro-82m", diskBytes: 355_000_000 }],
    },
  });
  await page.goto("/");
  const suspend = async (id: string, start: string) => {
    await page.getByTestId("nav-library").click();
    await page.getByTestId(`create-audio-${id}`).click();
    await page.getByTestId("scope-pages-tab").click();
    await page.getByTestId("page-start-input").fill(start);
    await castStep(page, "sound");
    await page.getByTestId("voice-engine-cloud").click();
    await page.getByTestId("cast-open-providers-btn").click();
  };
  await suspend("book-fixture", "3");
  await suspend("book-second", "7");
  const returns = page.getByTestId("cast-draft-return");
  await expect(returns).toHaveCount(2);
  await expect(returns.nth(0)).toHaveAttribute(
    "aria-label",
    /Fixture Book Title/,
  );
  await expect(returns.nth(1)).toHaveAttribute("aria-label", /Second Book/);
  await page.getByTestId("cast-draft-discard-book-second").click();
  await expect(returns).toHaveCount(1);
  await expect(returns).toBeFocused();
  await returns.click();
  await castStep(page, "scope");
  await expect(page.getByTestId("page-start-input")).toHaveValue("3");
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("create-audio-book-fixture")).toBeFocused();
  await suspend("book-fixture", "1");
  await page.getByTestId("cast-draft-discard-book-fixture").click();
  await expect(returns).toHaveCount(0);
  await expect(page.locator("#main-content")).toBeFocused();
});
