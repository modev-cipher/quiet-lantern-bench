import { expect, test } from "@playwright/test";

import { installTauriMock } from "./tauriMock";
import { msg } from "./i18nText";

async function openSettings(page: import("@playwright/test").Page) {
  await installTauriMock(page, { books: [] });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await expect(page.getByTestId("storage-settings")).toBeVisible();
}

async function keyProbeCount(page: import("@playwright/test").Page) {
  return page.evaluate(
    () =>
      (
        window as unknown as { __invokedCommands: string[] }
      ).__invokedCommands.filter(
        (cmd) => cmd === "probe_voice_keys" || cmd === "list_voice_keys",
      ).length,
  );
}

test("storage panel shows where models, audio and the cache live", async ({
  page,
}) => {
  await openSettings(page);

  const rows = [
    ["storage-models-dir", "/mock/data/models", "models"],
    ["storage-exports-dir", "/mock/data/exports", "exports"],
    ["storage-cache-dir", "/mock/data/cache", "cache"],
  ] as const;

  const shown: string[] = [];
  for (const [testId, full, leaf] of rows) {
    const row = page.getByTestId(testId);
    await expect(
      row,
      "the full path is what the row means; the shortened form is only what fits",
    ).toHaveAttribute("title", full);
    await expect(
      row,
      "the leaf is the only part that tells these three rows apart, so it is the part that must survive",
    ).toContainText(leaf);
    shown.push((await row.textContent()) ?? "");
  }

  expect(
    new Set(shown).size,
    "three directories that render as one string tell the reader nothing",
  ).toBe(3);
  await expect(
    page.getByText(msg("settings.defaultLocation")).first(),
  ).toBeVisible();
});

test("cache limit starts at the shipped default and takes a custom size", async ({
  page,
}) => {
  await openSettings(page);

  await expect(page.getByTestId("cache-cap-effective")).toHaveText(
    msg("settings.cacheUsage", { used: "3", cap: "20" }),
  );
  await expect(page.getByTestId("cache-cap-mode")).toHaveValue("default");

  await page.getByTestId("cache-cap-mode").selectOption("custom");
  await page.getByTestId("cache-cap-gb").fill("5");
  await page.getByTestId("cache-cap-save").click();

  await expect(page.getByTestId("cache-cap-effective")).toHaveText(
    msg("settings.cacheUsage", { used: "3", cap: "5" }),
  );
  await expect(page.getByTestId("cache-cap-mode")).toHaveValue("custom");
  await expect(page.getByTestId("cache-cap-gb")).toHaveValue("5");
});

test("an unusable cache limit is refused with a reason and nothing is saved", async ({
  page,
}) => {
  await openSettings(page);

  await page.getByTestId("cache-cap-mode").selectOption("custom");
  await page.getByTestId("cache-cap-gb").fill("20 GB");
  await page.getByTestId("cache-cap-save").click();

  await expect(page.getByTestId("cache-cap-error")).toContainText(
    msg("settings.capErrorShape"),
  );
  await expect(page.getByTestId("cache-cap-effective")).toHaveText(
    msg("settings.cacheUsage", { used: "3", cap: "20" }),
  );

  await page.reload();
  await page.getByTestId("nav-settings").click();
  await expect(page.getByTestId("cache-cap-mode")).toHaveValue("default");
});

test("no limit is rehydrated from the backend, not from local UI state", async ({
  page,
}) => {
  await openSettings(page);

  await page.getByTestId("cache-cap-mode").selectOption("unlimited");
  await page.getByTestId("cache-cap-save").click();

  await expect(page.getByTestId("cache-cap-effective")).toHaveText(
    msg("settings.cacheUsageUnlimited", { used: "3" }),
  );

  await page.reload();
  await page.getByTestId("nav-settings").click();
  await expect(page.getByTestId("cache-cap-mode")).toHaveValue("unlimited");
  await expect(page.getByTestId("cache-cap-effective")).toHaveText(
    msg("settings.cacheUsageUnlimited", { used: "3" }),
  );
});

test("a cache limit the app cannot read is shown as the default in force", async ({
  page,
}) => {
  await installTauriMock(page, { books: [], cacheCapRaw: "20 GB" });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();

  await expect(page.getByTestId("cache-cap-mode")).toHaveValue("default");
  await expect(page.getByTestId("cache-cap-effective")).toHaveText(
    msg("settings.cacheUsage", { used: "3", cap: "20" }),
  );
  await expect(page.getByTestId("cache-cap-notice")).toContainText("20 GB");
});

test("General settings shows ONE key surface: the pointer card into Providers", async ({
  page,
}) => {
  // S-5 gate 11: VoiceKeysPanel is gone; the only key editor is
  // Settings -> Providers. The pointer card navigates there, and no key
  // row, probe, or key-state vocabulary renders in General settings.
  await installTauriMock(page, { books: [] });
  await page.goto("/");
  await expect(page.getByTestId("nav-library")).toHaveAttribute(
    "aria-current",
    "page",
  );
  const probesBeforeSettings = await keyProbeCount(page);
  await page.getByTestId("nav-settings").click();
  await expect(page.getByTestId("providers-pointer-card")).toBeVisible();
  await expect(page.getByTestId("settings-panel")).not.toContainText(/····/);
  expect((await keyProbeCount(page)) - probesBeforeSettings).toBe(0);
  await page.getByTestId("open-providers").click();
  await expect(page.getByTestId("providers-page")).toBeVisible();
});

test("storage breakdown displays granular categories and handles purge cache and uninstall engine", async ({
  page,
}) => {
  await openSettings(page);

  await expect(page.getByTestId("storage-breakdown-engine")).toBeVisible();
  await expect(page.getByTestId("storage-breakdown-cache")).toBeVisible();
  await expect(page.getByTestId("storage-breakdown-books")).toBeVisible();
  await expect(page.getByTestId("storage-breakdown-exports")).toBeVisible();
  await expect(page.getByTestId("storage-breakdown-models")).toBeVisible();

  // Cache purge
  const purgeBtn = page.getByTestId("purge-cache-button");
  await expect(purgeBtn).toBeEnabled();
  await purgeBtn.click();

  // Engine uninstall
  // Engine uninstall triggers confirmation modal before uninstalling
  const uninstallEngineBtn = page.getByTestId("uninstall-engine-button");
  await expect(uninstallEngineBtn).toBeVisible();
  await uninstallEngineBtn.click();

  const engineModal = page.getByTestId("uninstall-engine-modal");
  await expect(engineModal).toBeVisible();

  // Canceling via cancel button dismisses modal without uninstalling
  const cancelEngineBtn = page.getByTestId("cancel-uninstall-engine-button");
  await cancelEngineBtn.click();
  await expect(engineModal).toHaveCount(0);
  await expect(uninstallEngineBtn).toBeVisible();

  // Test Escape key dismissal
  await uninstallEngineBtn.click();
  await expect(engineModal).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(engineModal).toHaveCount(0);
  await expect(uninstallEngineBtn).toBeVisible();

  // Confirming uninstalls the engine
  await uninstallEngineBtn.click();
  await expect(engineModal).toBeVisible();
  const confirmEngineBtn = page.getByTestId("confirm-uninstall-engine-button");
  await confirmEngineBtn.click();
  await expect(engineModal).toHaveCount(0);
  await expect(page.getByTestId("uninstall-engine-button")).toHaveCount(0);
});

test("the support row opens the membership page only when asked", async ({
  page,
}) => {
  await openSettings(page);
  const opens = () =>
    page.evaluate(() =>
      (
        window as unknown as { __invokedCommands: string[] }
      ).__invokedCommands.filter((cmd) => cmd === "plugin:opener|open_url"),
    );
  await expect(page.getByTestId("settings-panel")).toContainText(
    msg("settings.support"),
  );
  expect(await opens()).toHaveLength(0);
  await page.getByTestId("settings-support").click();
  await expect.poll(opens).toHaveLength(1);
});

test("model deletion triggers confirmation modal before deleting", async ({
  page,
}) => {
  await openSettings(page);

  const deleteBtn = page.getByTestId("delete-model-kokoro-82m");
  await expect(deleteBtn).toBeVisible();

  // Clicking delete button opens modal instead of immediate delete
  await deleteBtn.click();
  const modal = page.getByTestId("delete-model-modal");
  await expect(modal).toBeVisible();
  await expect(modal).toContainText("Kokoro TTS");

  // Canceling via cancel button dismisses modal without deleting
  const cancelBtn = page.getByTestId("cancel-delete-model-button");
  await cancelBtn.click();
  await expect(modal).toHaveCount(0);
  await expect(deleteBtn).toBeVisible();

  // Test Escape key dismissal
  await deleteBtn.click();
  await expect(modal).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(modal).toHaveCount(0);
  await expect(deleteBtn).toBeVisible();

  // Confirming deletion deletes the model
  await deleteBtn.click();
  await expect(modal).toBeVisible();
  const confirmBtn = page.getByTestId("confirm-delete-model-button");
  await confirmBtn.click();
  await expect(modal).toHaveCount(0);
  await expect(page.getByTestId("delete-model-kokoro-82m")).toHaveCount(0);
});

test("danger zone uninstall modal requires typing UNINSTALL to confirm self purge", async ({
  page,
}) => {
  await openSettings(page);

  await expect(page.getByTestId("danger-zone")).toBeVisible();
  await page.getByTestId("open-danger-purge").click();
  await expect(page.getByTestId("uninstall-modal")).toBeVisible();

  const confirmBtn = page.getByTestId("confirm-danger-purge");
  const input = page.getByTestId("uninstall-confirm-input");

  // Disabled when empty or wrong keyword
  await expect(confirmBtn).toBeDisabled();
  await input.fill("uninstall");
  await expect(confirmBtn).toBeDisabled();
  await input.fill("NOPE");
  await expect(confirmBtn).toBeDisabled();

  // Enabled only when exact UNINSTALL
  await input.fill("UNINSTALL");
  await expect(confirmBtn).toBeEnabled();
  await confirmBtn.click();
});
