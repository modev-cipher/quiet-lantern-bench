import { expect, test, type Page } from "@playwright/test";
import {
  fixtureCatalogModels,
  fixtureHardware,
  installTauriMock,
} from "./tauriMock";

const MODEL = "kokoro-82m";

async function openModels(page: Page, locale: "en" | "ar", command: string) {
  await page.setViewportSize({ width: 900, height: 600 });
  await installTauriMock(page, {
    books: [],
    hostLocale: locale,
    modelScenario: {
      catalogModels: fixtureCatalogModels(),
      hardware: fixtureHardware(),
      installed: [{ modelId: MODEL, diskBytes: 355000000 }],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.evaluate((target) => {
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
    const fault = window as unknown as { __modelsRecoveryFault: boolean };
    fault.__modelsRecoveryFault = true;
    internals.invoke = (command, args, options) =>
      command === target && fault.__modelsRecoveryFault
        ? Promise.reject("fixture request refused")
        : original(command, args, options);
  }, command);
  await page.getByTestId("nav-models").click();
}

for (const locale of ["en", "ar"] as const) {
  const retry = locale === "ar" ? "إعادة المحاولة" : "Retry";

  test(`${locale} catalog failure recovers in place at the minimum window`, async ({
    page,
  }) => {
    await openModels(page, locale, "get_catalog");
    const error = page.getByTestId("models-read-error-catalog");
    await expect(error).toBeVisible();
    await expect(error).toContainText(
      locale === "ar"
        ? "تعذّر تحميل قائمة"
        : "Could not load the model catalog",
    );
    const button = error.getByRole("button", { name: retry });
    await expect(button).toBeInViewport();
    await page.evaluate(() => {
      (
        window as unknown as { __modelsRecoveryFault: boolean }
      ).__modelsRecoveryFault = false;
    });
    await button.click();
    await expect(page.getByTestId("installed-models")).toBeVisible();
    await expect(error).toHaveCount(0);
    await expect(
      page.getByTestId("models-panel").getByRole("heading").first(),
    ).toBeFocused();
  });

  test(`${locale} a refused removal retains its row and the same action can be retried`, async ({
    page,
  }) => {
    await openModels(page, locale, "delete_model");
    const installed = page.getByTestId("installed-models");
    await installed.getByTestId(`delete-model-${MODEL}`).click();
  await page.getByTestId("confirm-delete-model-button").click();
    const error = page.getByTestId("models-action-error");
    await expect(error).toContainText(
      locale === "ar" ? "تعذّرت إزالة" : "Could not finish removing",
    );
    await expect(installed.getByTestId(`model-row-${MODEL}`)).toBeVisible();
    await expect(page.getByTestId(`download-resume-${MODEL}`)).toHaveCount(0);
    await page.evaluate(() => {
      (
        window as unknown as { __modelsRecoveryFault: boolean }
      ).__modelsRecoveryFault = false;
    });
    await error.getByRole("button", { name: retry }).click();
    await expect(page.getByTestId("installed-models")).toHaveCount(0);
    await expect(error).toHaveCount(0);
    await expect(
      page.getByTestId(`download-model-${MODEL}`).last(),
    ).toBeEnabled();
  });

  test(`${locale} a refused default change is explained and retried without losing the model`, async ({
    page,
  }) => {
    await openModels(page, locale, "set_default_model");
    const installed = page.getByTestId("installed-models");
    await installed.getByTestId(`set-default-${MODEL}`).click();
    const error = page.getByTestId("models-action-error");
    await expect(error).toContainText(
      locale === "ar"
        ? "تعذّر تغيير الاختيار الافتراضي"
        : "Could not change the default",
    );
    await expect(installed.getByTestId(`model-default-${MODEL}`)).toHaveCount(
      0,
    );
    await page.evaluate(() => {
      (
        window as unknown as { __modelsRecoveryFault: boolean }
      ).__modelsRecoveryFault = false;
    });
    await error.getByRole("button", { name: retry }).click();
    await expect(installed.getByTestId(`model-default-${MODEL}`)).toBeVisible();
    await expect(error).toHaveCount(0);
  });
}
