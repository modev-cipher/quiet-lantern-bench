import { expect, test } from "@playwright/test";

import { msg } from "./i18nText";

import {
  fixtureCatalogModels,
  fixtureDevice,
  fixtureDevices,
  fixtureHardware,
  installTauriMock,
  NO_DEVICES,
  type ModelScenario,
} from "./tauriMock";

function scenario(overrides: Partial<ModelScenario> = {}): ModelScenario {
  return {
    catalogModels: fixtureCatalogModels(),
    hardware: fixtureHardware(),
    ...overrides,
  };
}

test("installs Kokoro from the catalog with live progress", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    modelScenario: scenario({ downloadTicks: 3, downloadDelayMs: 200 }),
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-models").click();

  const row = page.getByTestId("model-row-kokoro-82m").last();
  await expect(row).toContainText(
    msg("models.name", {
      role: msg("models.role.voice"),
      tier: msg("models.tier.proPlus"),
    }),
  );
  await expect(row).toContainText("355 MB");

  await row.getByTestId("download-model-kokoro-82m").click();
  await expect(row.getByTestId("download-status-kokoro-82m")).toContainText(
    msg("models.download.downloading"),
  );

  await expect(page.getByTestId("installed-models")).toContainText(
    msg("models.name", {
      role: msg("models.role.voice"),
      tier: msg("models.tier.proPlus"),
    }),
    { timeout: 10_000 },
  );
  await expect(
    page.getByTestId("installed-models").getByTestId("delete-model-kokoro-82m"),
  ).toBeVisible();
});

test("badge states render per fixture profiles", async ({ page }) => {
  await installTauriMock(page, {
    books: [],
    modelScenario: scenario({
      hardware: fixtureHardware({
        ...fixtureDevices([
          fixtureDevice({
            name: "4 GB GPU",
            memoryTotalMb: 4096,
            memoryFreeMb: 4096,
          }),
        ]),
        ram: { totalMb: 16_384, freeMb: 4096 },
      }),
    }),
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-models").click();

  await expect(page.getByTestId("model-badge-kokoro-82m")).toHaveAttribute(
    "data-badge",
    "green",
  );
  await page.getByTestId("models-tab-llm").click();
  await expect(
    page.getByTestId("model-badge-qwen3.5-9b-instruct-q4km"),
  ).toHaveAttribute("data-badge", "yellow");
  await expect(
    page.getByTestId("model-badge-qwen3.5-32b-class-q4km"),
  ).toHaveAttribute("data-badge", "red");
});

test("no-GPU profile renders CPU badges", async ({ page }) => {
  await installTauriMock(page, {
    books: [],
    modelScenario: scenario({
      hardware: fixtureHardware({
        ...NO_DEVICES,
        ram: { totalMb: 16_384, freeMb: 12_288 },
      }),
    }),
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-models").click();

  await expect(page.getByTestId("model-badge-kokoro-82m")).toHaveAttribute(
    "data-badge",
    "orange",
  );
  await page.getByTestId("models-tab-llm").click();
  await expect(
    page.getByTestId("model-badge-qwen3.5-9b-instruct-q4km"),
  ).toHaveAttribute("data-badge", "orange");
});

test("no-GPU profile never advertises GPU execution on a green badge (F3)", async ({
  page,
}) => {
  // The planner returns { badge: Green, plan: None } for a model that fits on
  // the CPU of a GPU-less machine; the copy must say CPU, never "on your GPU".
  await installTauriMock(page, {
    books: [],
    modelScenario: scenario({
      catalogModels: fixtureCatalogModels().map((model) =>
        model.id === "kokoro-82m"
          ? { ...model, badge: "green" as const }
          : model,
      ),
      hardware: fixtureHardware({
        ...NO_DEVICES,
        ram: { totalMb: 16_384, freeMb: 12_288 },
      }),
    }),
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-models").click();

  const badge = page.getByTestId("model-badge-kokoro-82m");
  await expect(badge).toHaveAttribute("data-badge", "green");
  await expect(badge).toHaveAttribute("title", msg("fit.green.detailCpu"));
  await expect(badge).not.toHaveAttribute("title", /GPU/);
});

test("blocks download on insufficient disk with exact numbers", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    modelScenario: scenario({
      hardware: fixtureHardware({ disk: { modelsFreeGb: 4.1 } }),
    }),
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-models").click();

  await page.getByTestId("models-tab-llm").click();
  const blocked = page.getByTestId("download-model-qwen3.5-9b-instruct-q4km");
  await expect(blocked).toBeDisabled();
  await expect(
    page.getByTestId("disk-block-qwen3.5-9b-instruct-q4km"),
  ).toContainText(msg("models.diskBlock", { need: "5.7 GB", have: "4.1 GB" }));

  await page.getByTestId("models-tab-tts").click();
  await expect(page.getByTestId("download-model-kokoro-82m")).toBeEnabled();
});

test("cancel stops an in-flight download", async ({ page }) => {
  await installTauriMock(page, {
    books: [],
    modelScenario: scenario({ downloadTicks: 30, downloadDelayMs: 200 }),
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-models").click();

  await page.getByTestId("download-model-kokoro-82m").click();
  await page.getByTestId("cancel-download-kokoro-82m").click();

  await expect(page.getByTestId("download-model-kokoro-82m")).toBeEnabled();
  await expect(
    page.getByTestId("download-error-kokoro-82m"),
    "a cancel you asked for is not an error",
  ).toHaveCount(0);
});

test("surfaces failed downloads and allows retry", async ({ page }) => {
  await installTauriMock(page, {
    books: [],
    modelScenario: scenario({
      downloadTicks: 2,
      downloadDelayMs: 50,
      downloadError: "checksum mismatch for 'weights.bin'",
    }),
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-models").click();

  await page.getByTestId("download-model-kokoro-82m").click();
  const error = page.getByTestId("download-error-kokoro-82m");
  await expect(error).toContainText(msg("models.error.generic"));
  await expect(error.getByText("checksum mismatch")).toBeHidden();
  await error.getByText(msg("models.error.details")).click();
  await expect(
    error.getByText("checksum mismatch", { exact: false }),
  ).toBeVisible();
  await expect(page.getByTestId("download-resume-kokoro-82m")).toBeEnabled();
  await expect(page.getByTestId("download-model-kokoro-82m")).toBeEnabled();
});

test("a dropped connection reads as calm and resumable, not as a raw network error", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    modelScenario: scenario({
      downloadTicks: 2,
      downloadDelayMs: 50,
      downloadError: "network error: error decoding response body",
    }),
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-models").click();
  await page.getByTestId("download-model-kokoro-82m").click();
  const error = page.getByTestId("download-error-kokoro-82m");
  await expect(error).toHaveAttribute("data-kind", "connection");
  await expect(error).toContainText(msg("models.error.connection"));
  await expect(page.getByTestId("download-resume-kokoro-82m")).toHaveText(
    msg("models.error.resume"),
  );
});

test("installed models support set-default and delete", async ({ page }) => {
  await installTauriMock(page, {
    books: [],
    modelScenario: scenario({
      installed: [
        { modelId: "kokoro-82m", diskBytes: 355_000_000 },
        { modelId: "studio-tts", diskBytes: 2_100_000_000 },
      ],
    }),
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-models").click();

  const installed = page.getByTestId("installed-models");
  await expect(installed).toContainText(
    msg("models.name", {
      role: msg("models.role.voice"),
      tier: msg("models.tier.proPlus"),
    }),
  );
  await expect(installed).toContainText(
    msg("models.name", {
      role: msg("models.role.voice"),
      tier: msg("models.tier.max"),
    }),
  );
  await expect(installed.getByTestId("model-license-studio-tts")).toContainText(
    msg("models.nonCommercial"),
  );

  await installed.getByTestId("set-default-kokoro-82m").click();
  await expect(installed.getByTestId("model-default-kokoro-82m")).toBeVisible();
  await expect(
    installed.getByTestId("set-default-kokoro-82m"),
  ).not.toBeVisible();

  await installed.getByTestId("delete-model-studio-tts").click();
  await page.getByTestId("confirm-delete-model-button").click();
  await expect(installed).not.toContainText(
    msg("models.name", {
      role: msg("models.role.voice"),
      tier: msg("models.tier.max"),
    }),
  );
});
