import { expect, test } from "@playwright/test";

import {
  fixtureCatalogModels,
  fixtureHardware,
  installTauriMock,
} from "./tauriMock";

test("voices sit in a gallery of tiles, while text and vision models stay rows", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await installTauriMock(page, {
    books: [],
    modelScenario: {
      catalogModels: fixtureCatalogModels(),
      hardware: fixtureHardware(),
      downloadTicks: 3,
      downloadDelayMs: 200,
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-models").click();

  const kokoro = page.getByTestId("model-row-kokoro-82m").last();
  const studio = page.getByTestId("model-row-studio-tts").last();
  await expect(kokoro).toHaveAttribute("data-layout", "tile");
  await expect(studio).toHaveAttribute("data-layout", "tile");
  const a = (await kokoro.boundingBox())!;
  const b = (await studio.boundingBox())!;
  expect(
    Math.abs(a.y - b.y),
    "two voices share a row of the gallery",
  ).toBeLessThanOrEqual(2);
  expect(a.width).toBeLessThan(700);

  await page.getByTestId("models-tab-llm").click();
  const rows = page.locator('[data-layout="row"]');
  await expect(rows.first()).toBeVisible();

  await page.getByTestId("models-tab-tts").click();
  await kokoro.getByTestId("download-model-kokoro-82m").click();
  await expect(kokoro.getByTestId("download-status-kokoro-82m")).toBeVisible();
});
