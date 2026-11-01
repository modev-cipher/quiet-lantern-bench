import { expect, test } from "@playwright/test";
import { installTauriMock } from "./tauriMock";

test.beforeEach(async ({ page }, testInfo) => {
  await installTauriMock(page, {
    books: [],
    providersScenario: {
      families: [],
      setupDelayMs:
        testInfo.title.includes("leaving pending") ||
        testInfo.title.includes("changing purpose")
          ? 1000
          : undefined,
      setupError: testInfo.title.startsWith("an invalid")
        ? "PROVIDER_INVALID_KEY"
        : undefined,
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-providers").click();
});

test("named services have separate identities and empty OpenAI setup asks only for a key", async ({
  page,
}) => {
  await expect(
    page.getByTestId("provider-group-main").getByTestId("provider-card-openai"),
  ).toBeVisible();
  await expect(
    page.getByTestId("provider-group-others").getByTestId("provider-card-groq"),
  ).toBeVisible();
  await expect(
    page
      .getByTestId("provider-group-local")
      .getByTestId("provider-card-ollama"),
  ).toBeVisible();
  await page.getByTestId("provider-card-openai").click();
  await expect(page.getByTestId("provider-setup-key")).toBeVisible();
  await expect(page.getByTestId("add-connection-name")).toHaveCount(0);
  await expect(page.getByTestId("add-connection-url")).toHaveCount(0);
  await expect(page.getByTestId("add-connection-model")).toHaveCount(0);
});

test("Cartesia never shows another provider's local privacy claim", async ({
  page,
}) => {
  await page.getByTestId("provider-card-cartesia").click();
  await expect(page.getByTestId("provider-setup-key")).toBeVisible();
  await expect(page.getByTestId("provider-detail-cartesia")).not.toContainText(
    "Ollama",
  );
  await expect(page.getByTestId("add-connection-local-note")).toHaveCount(0);
});

test("Connect provisions models and Refresh waits for Apply", async ({
  page,
}) => {
  await page.getByTestId("provider-card-openai").click();
  await page
    .getByTestId("provider-setup-key")
    .fill("fixture-provider-key-1234");
  await page.getByTestId("provider-setup-connect").click();
  await expect(page.getByTestId("provider-connected")).toBeVisible();
  await expect(page.getByTestId("provider-setup-key")).toHaveCount(0);
  await page.getByTestId("provider-refresh").click();
  await expect(page.getByTestId("provider-apply")).toBeVisible();
  const calls = await page.evaluate(
    () =>
      (window as unknown as { __invokedCommands: string[] }).__invokedCommands,
  );
  expect(calls).not.toContain("provider_apply_recommendations");
  await page.getByTestId("provider-apply").click();
  await expect(page.getByTestId("provider-apply")).toHaveCount(0);
});

test("customizing a model keeps an empty field available for typing", async ({
  page,
}) => {
  await page.getByTestId("provider-card-openai").click();
  await page.getByTestId("provider-setup-key").fill("fixture-key");
  await page.getByTestId("provider-setup-connect").click();
  await page.getByTestId("provider-customize").click();
  const model = page.getByTestId("provider-model-script");
  await model.fill("");
  await expect(model).toBeVisible();
  await model.fill("gpt-6-luna");
  await expect(model).toHaveValue("gpt-6-luna");
});

test("opening settings sends no metadata requests and leaving pending setup cancels it", async ({
  page,
}) => {
  const initial = await page.evaluate(
    () =>
      (window as unknown as { __invokedCommands: string[] }).__invokedCommands,
  );
  expect(initial).not.toContain("provider_refresh_catalog");
  expect(initial).not.toContain("provider_connect");
  await page.getByTestId("provider-card-openai").click();
  await page.getByTestId("provider-setup-key").fill("fixture-key");
  await page.getByTestId("provider-setup-connect").click();
  await expect(page.getByTestId("provider-setup-connect")).toBeDisabled();
  await page.getByTestId("providers-back").click();
  await page.getByTestId("provider-card-cartesia").click();
  await expect(page.getByTestId("provider-setup-key")).toHaveValue("");
  await expect
    .poll(() =>
      page.evaluate(() =>
        (
          window as unknown as { __invokedCommands: string[] }
        ).__invokedCommands.includes("provider_cancel_setup"),
      ),
    )
    .toBe(true);
  await expect(page.getByTestId("provider-detail-cartesia")).not.toContainText(
    "gpt-",
  );
});

test("an invalid supplied key has an actionable error and remains editable", async ({
  page,
}) => {
  await page.getByTestId("provider-card-openai").click();
  await page.getByTestId("provider-setup-key").fill("invalid-fixture-key");
  await page.getByTestId("provider-setup-connect").click();
  await expect(page.getByTestId("provider-setup-error")).toBeVisible();
  await expect(page.getByTestId("provider-setup-key")).toBeEnabled();
  await expect(page.getByTestId("provider-connected")).toHaveCount(0);
});

test("changing purpose while discovery is pending cannot restore another role's model", async ({
  page,
}) => {
  await page.clock.install();
  await page.getByTestId("provider-card-openai").click();
  await page.getByTestId("provider-setup-key").fill("fixture-key");
  await page.getByTestId("provider-setup-connect").click();
  await page.clock.runFor(1100);
  await expect(page.getByTestId("provider-connected")).toBeVisible();
  await page.getByTestId("provider-manage-keys").click();
  await page.getByTestId("add-connection-list-models").click();
  await page.getByTestId("add-connection-kind-button").click();
  await page.getByTestId("add-connection-kind-option-openai_tts").click();
  await page.clock.runFor(1100);
  await expect(page.getByTestId("add-connection-model")).toHaveValue("");
  await expect(page.getByTestId("add-connection-models-panel")).toHaveCount(0);
});

for (const locale of ["en", "ar"] as const) {
  for (const theme of ["light", "dark"] as const) {
    test(`${locale} ${theme} setup fits a narrow window and keeps keys LTR`, async ({
      page,
    }) => {
      await page.setViewportSize({ width: 720, height: 900 });
      await page.evaluate(
        ({ locale, theme }) => {
          localStorage.setItem("txtcaster.locale", locale);
          sessionStorage.setItem("mock-locale-preference", locale);
          localStorage.setItem("txtcaster.themePreference", theme);
          sessionStorage.setItem("mock-theme-preference", theme);
        },
        { locale, theme },
      );
      await page.reload();
      await page.getByTestId("nav-settings").click();
      await page.getByTestId("nav-providers").click();
      await page.getByTestId("provider-card-openai").click();
      await expect(page.locator("html")).toHaveAttribute(
        "dir",
        locale === "ar" ? "rtl" : "ltr",
      );
      await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
      const key = page.getByTestId("provider-setup-key");
      await expect(key).toHaveAttribute("dir", "ltr");
      await key.fill("fixture-key");
      await key.press("Tab");
      await expect(page.getByTestId("provider-setup-connect")).toBeFocused();
      const bounds = await page
        .getByTestId("provider-detail-openai")
        .boundingBox();
      const button = await page
        .getByTestId("provider-setup-connect")
        .boundingBox();
      expect(bounds).not.toBeNull();
      expect(button!.x).toBeGreaterThanOrEqual(bounds!.x);
      expect(button!.x + button!.width).toBeLessThanOrEqual(
        bounds!.x + bounds!.width + 1,
      );
      await page.screenshot({
        animations: "disabled",
        path: `/tmp/txtcast-provider-${locale}-${theme}.png`,
      });
    });
  }
}
