import { expect, test } from "@playwright/test";
import { castStep } from "./castSteps";

import {
  fixtureBook,
  fixtureCatalogModels,
  fixtureChapters,
  installTauriMock,
  type MockCatalogModel,
  type Scenario,
} from "./tauriMock";
import { msg } from "./i18nText";

function chatterbox(): MockCatalogModel {
  return {
    id: "chatterbox-0.5b",
    task: "tts",
    display_name: "Chatterbox",
    family: "chatterbox",
    quant: null,
    format: "pytorch",
    disk_bytes: 3_191_966_992,
    full_gpu_vram_mb: 5120,
    min_free_ram_mb: 6144,
    hybrid_capable: false,
    quality_score: 90,
    speed_score: 70,
    capabilities: ["voice_cloning"],
    license: { spdx: "MIT", commercial_ok: true },
    perf: { rtf_gpu: 0.2, rtf_cpu: 4.0 },
  };
}

function scenario(
  overrides: Partial<Scenario> = {},
  installed = true,
): Scenario {
  return {
    books: [fixtureBook()],
    jobScenario: { chapters: fixtureChapters() },
    modelScenario: {
      catalogModels: [...fixtureCatalogModels(), chatterbox()],
      installed: installed
        ? [
            { modelId: "kokoro-82m", diskBytes: 355_000_000 },
            { modelId: "chatterbox-0.5b", diskBytes: 3_191_966_992 },
          ]
        : [{ modelId: "kokoro-82m", diskBytes: 355_000_000 }],
    },
    ...overrides,
  };
}

async function openCloneModal(page: import("@playwright/test").Page) {
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await expect(page.getByTestId("job-modal")).toBeVisible();
  await castStep(page, "sound");
  await page.getByTestId("clone-voice-open").click();
  await expect(page.getByTestId("clone-voice-modal")).toBeVisible();
}

test("records a sample, previews it and saves a cloned voice that the job then uses", async ({
  page,
}) => {
  await installTauriMock(page, scenario());
  await openCloneModal(page);

  await expect(page.getByTestId("clone-script")).toBeVisible();
  await expect(page.getByTestId("clone-consent")).toContainText(
    msg("voice.clone.consentNote"),
  );
  await expect(page.getByTestId("clone-watermark")).toContainText(
    msg("voice.clone.watermarkNote"),
  );

  await page.getByTestId("clone-record").click();
  await expect(page.getByTestId("clone-stop")).toBeVisible();
  await page.getByTestId("clone-stop").click();

  await expect(page.getByTestId("clone-sample")).toContainText(
    msg("voice.clone.recorded"),
  );
  await expect(page.getByTestId("clone-sample")).toContainText("6.4s");
  await page.getByTestId("clone-preview").click();

  await page.getByTestId("clone-name").fill("Reader voice");
  await page.getByTestId("clone-save").click();

  await expect(page.getByTestId("clone-voice-modal")).not.toBeVisible();
  await expect(page.getByTestId("voice-select")).toHaveValue("cloned-sample-1");
  await expect(page.getByTestId("voice-select")).toContainText("Reader voice");
});

test("imports a file when the machine has no microphone", async ({ page }) => {
  await installTauriMock(
    page,
    scenario({
      jobScenario: {
        chapters: fixtureChapters(),
        cloneScenario: {
          audioInputsError:
            "txtCaster could not find a microphone on this computer. You can import an audio file instead.",
          importPath: "/samples/me.wav",
          importSeconds: 7.2,
        },
      },
    }),
  );
  await openCloneModal(page);

  await expect(page.getByTestId("clone-no-microphone")).toContainText(
    msg("voice.clone.err.noInputs"),
  );
  await expect(page.getByTestId("clone-record")).toBeDisabled();

  await page.getByTestId("clone-import").click();
  await expect(page.getByTestId("clone-sample")).toContainText(
    msg("voice.clone.imported"),
  );
  await expect(page.getByTestId("clone-sample")).toContainText("7.2s");

  await page.getByTestId("clone-name").fill("Imported voice");
  await page.getByTestId("clone-save").click();
  await expect(page.getByTestId("clone-voice-modal")).not.toBeVisible();
});

test("a sample that is too short is refused with copy that says what to do", async ({
  page,
}) => {
  await installTauriMock(
    page,
    scenario({
      jobScenario: {
        chapters: fixtureChapters(),
        cloneScenario: {
          importPath: "/samples/short.wav",
          importError:
            "That sample is only 1.4 seconds of speech. Cloning needs at least 3 seconds — read a full sentence or two.",
        },
      },
    }),
  );
  await openCloneModal(page);

  await page.getByTestId("clone-import").click();
  await expect(page.getByTestId("clone-error")).toContainText(
    msg("voice.clone.err.tooShort"),
  );
  await expect(page.getByTestId("clone-save")).toHaveCount(0);
});

test("without the cloning model the modal explains and links to Models", async ({
  page,
}) => {
  await installTauriMock(page, scenario({}, false));
  await openCloneModal(page);

  await expect(page.getByTestId("clone-needs-model")).toContainText("3.2 GB");
  await expect(page.getByTestId("clone-record")).toHaveCount(0);
  await page.getByTestId("clone-open-models").click();
  await expect(page.getByTestId("clone-voice-modal")).not.toBeVisible();
});

test("a long recording tells the user only part of it was kept", async ({
  page,
}) => {
  await installTauriMock(
    page,
    scenario({
      jobScenario: {
        chapters: fixtureChapters(),
        cloneScenario: { recordSeconds: 60, hitHardStop: true },
      },
    }),
  );
  await openCloneModal(page);

  await page.getByTestId("clone-record").click();
  await page.getByTestId("clone-stop").click();
  await expect(page.getByTestId("clone-sample-warning")).toContainText(
    msg("voice.clone.warnLong", { n: 12 }),
  );
});

test("starting over discards the sample and returns to the recording step", async ({
  page,
}) => {
  await installTauriMock(page, scenario());
  await openCloneModal(page);

  await page.getByTestId("clone-record").click();
  await page.getByTestId("clone-stop").click();
  await expect(page.getByTestId("clone-sample")).toBeVisible();

  await page.getByTestId("clone-retake").click();
  await expect(page.getByTestId("clone-script")).toBeVisible();
  await expect(page.getByTestId("clone-sample")).toHaveCount(0);
});

test("closing the dialog mid-recording releases the microphone", async ({
  page,
}) => {
  await installTauriMock(page, scenario());
  await openCloneModal(page);

  await page.getByTestId("clone-record").click();
  await expect(page.getByTestId("clone-stop")).toBeVisible();

  await page.getByTestId("clone-close").click();
  await expect(page.getByTestId("clone-voice-modal")).not.toBeVisible();

  const cancelled = await page.evaluate(async () =>
    (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (cmd: string) => Promise<string[]>;
        };
      }
    ).__TAURI_INTERNALS__.invoke("__cancelled_recordings"),
  );
  expect(cancelled.length).toBeGreaterThan(0);
});

test("reopening after an abandoned recording can record again", async ({
  page,
}) => {
  await installTauriMock(page, scenario());
  await openCloneModal(page);
  await page.getByTestId("clone-record").click();
  await page.getByTestId("clone-close").click();

  await castStep(page, "sound");
  await page.getByTestId("clone-voice-open").click();
  await expect(page.getByTestId("clone-record")).toBeEnabled();
  await page.getByTestId("clone-record").click();
  await expect(page.getByTestId("clone-stop")).toBeVisible();
  await page.getByTestId("clone-stop").click();
  await expect(page.getByTestId("clone-sample")).toBeVisible();
});

test("a save that fails keeps the sample so the user can try again", async ({
  page,
}) => {
  await installTauriMock(
    page,
    scenario({
      jobScenario: {
        chapters: fixtureChapters(),
        cloneScenario: { saveError: "engine is not ready" },
      },
    }),
  );
  await openCloneModal(page);

  await page.getByTestId("clone-record").click();
  await page.getByTestId("clone-stop").click();
  await page.getByTestId("clone-name").fill("Reader voice");
  await page.getByTestId("clone-save").click();

  await expect(page.getByTestId("clone-error")).toContainText(
    msg("voice.clone.err.engineNotReady"),
  );
  await expect(page.getByTestId("clone-sample")).toBeVisible();
  await expect(page.getByTestId("clone-save")).toBeEnabled();
});
