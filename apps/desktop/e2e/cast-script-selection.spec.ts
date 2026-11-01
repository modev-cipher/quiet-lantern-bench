import { expect, test, type Page } from "@playwright/test";

import { castStep } from "./castSteps";
import {
  fixtureBook,
  fixtureCatalogModels,
  fixtureChapters,
  fixtureHardware,
  installTauriMock,
  type CastProviderScenario,
} from "./tauriMock";

const MODELS = {
  catalogModels: fixtureCatalogModels(),
  hardware: fixtureHardware(),
  installed: [
    { modelId: "kokoro-82m", diskBytes: 355_000_000 },
    { modelId: "qwen3.5-9b-instruct-q4km", diskBytes: 5_700_000_000 },
    { modelId: "gemma4-e4b-q4km", diskBytes: 4_000_000_000 },
  ],
};

function cloudProviders(count: number): CastProviderScenario {
  return {
    scriptConnections: Array.from({ length: count }, (_, index) => ({
      connectionId: `c-script-${index}`,
      name: `Script connection ${index + 1}`,
      provider: "gemini",
      defaultModel: "gemini-2.5-flash",
      loopback: false,
    })),
    ttsVoices: [],
  };
}

async function prepare(page: Page, providerCount: number) {
  await installTauriMock(page, {
    books: [fixtureBook()],
    onboardingComplete: true,
    modelScenario: MODELS,
    castProviders: cloudProviders(providerCount),
    jobScenario: { chapters: fixtureChapters() },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
}

async function holdDiscovery(page: Page, command: string) {
  await page.evaluate((heldCommand) => {
    const target = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (command: string, args: unknown) => Promise<unknown>;
      };
      __releaseCastDiscovery: () => void;
    };
    const invoke = target.__TAURI_INTERNALS__.invoke.bind(
      target.__TAURI_INTERNALS__,
    );
    const pending = new Promise<void>((resolve) => {
      target.__releaseCastDiscovery = resolve;
    });
    target.__TAURI_INTERNALS__.invoke = async (name, args) => {
      if (name === heldCommand) await pending;
      return invoke(name, args);
    };
  }, command);
}

async function releaseDiscovery(page: Page) {
  await page.evaluate(() => {
    (
      window as unknown as { __releaseCastDiscovery: () => void }
    ).__releaseCastDiscovery();
  });
}

async function openTeacher(page: Page) {
  await page.getByTestId("create-audio-book-fixture").click();
  await castStep(page, "sound");
  await page.getByTestId("mode-teacher").click();
}

async function submittedJob(page: Page) {
  await expect(page.getByTestId("start-job")).toBeEnabled();
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-modal")).not.toBeVisible();
  const calls = await page.evaluate(
    () =>
      (
        window as unknown as {
          __createJobCalls?: Array<Record<string, unknown>>;
        }
      ).__createJobCalls ?? [],
  );
  expect(calls).toHaveLength(1);
  return calls[0];
}

for (const command of ["get_catalog", "list_installed_models"]) {
  for (const providerCount of [1, 2]) {
    test(`Teacher stays local while ${command} is pending with ${providerCount} cloud connections`, async ({
      page,
    }) => {
      await prepare(page, providerCount);
      await holdDiscovery(page, command);
      await openTeacher(page);
      await expect(page.getByTestId("teacher-where-local")).toHaveAttribute(
        "aria-checked",
        "true",
      );
      await expect(page.getByTestId("start-job")).toBeDisabled();
      await releaseDiscovery(page);
      await expect(page.getByTestId("start-job")).toBeEnabled();
      await expect(page.getByTestId("teacher-where-local")).toHaveAttribute(
        "aria-checked",
        "true",
      );
      expect(await submittedJob(page)).not.toHaveProperty("scriptCloud");
    });
  }
}

test("an explicit Cloud choice survives pending local-model discovery", async ({
  page,
}) => {
  await prepare(page, 2);
  await holdDiscovery(page, "list_installed_models");
  await openTeacher(page);
  await page.getByTestId("teacher-where-cloud").click();
  await expect(page.getByTestId("start-job")).toBeDisabled();
  await releaseDiscovery(page);
  await expect(page.getByTestId("teacher-where-cloud")).toHaveAttribute(
    "aria-checked",
    "true",
  );
  expect(await submittedJob(page)).toMatchObject({
    scriptCloud: {
      connectionId: "c-script-0",
      model: "gemini-2.5-flash",
    },
  });
});

test("returning from a custom cloud model displays and submits the selected local model", async ({
  page,
}) => {
  await prepare(page, 1);
  await openTeacher(page);
  const localModel = "gemma4-e4b-q4km";
  await page.getByTestId("teacher-llm-select-button").click();
  await page.getByTestId(`cast-option-${localModel}`).click();
  const localLabel = await page
    .getByTestId("teacher-llm-select-button")
    .textContent();
  expect(localLabel?.trim().length).toBeGreaterThan(0);
  await page.getByTestId("teacher-where-cloud").click();
  await page.getByTestId("cast-cascade-model-llm-button").click();
  await page.getByTestId("cast-cascade-model-llm-option-__custom__").click();
  await page.getByTestId("cast-script-model").fill("my-custom-model");
  await page.getByTestId("teacher-where-local").click();
  await expect(page.getByTestId("teacher-llm-select-button")).toHaveText(
    localLabel!,
  );
  await expect(page.getByTestId("cast-script-model")).not.toBeVisible();
  const args = await submittedJob(page);
  expect(args).not.toHaveProperty("scriptCloud");
  expect(JSON.parse(String(args.modelOverridesJson))).toMatchObject({
    llm: localModel,
  });
});
