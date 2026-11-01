import { expect, test, type Page } from "@playwright/test";

import arShell from "../src/i18n/messages/ar/shell.json" with { type: "json" };
import enShell from "../src/i18n/messages/en/shell.json" with { type: "json" };
import { fixtureBook, installTauriMock } from "./tauriMock";

const S: Record<string, string> =
  process.env.I18N_E2E_LOCALE === "ar" ? arShell : enShell;

interface MockEngine {
  status: unknown;
  variant: string | null;
  cancelled: boolean;
  offerError: string | null;
  failNext: string | null;
}

async function boot(page: Page, engine: Partial<MockEngine>) {
  await page.addInitScript((seed) => {
    (window as unknown as { __mockEngine: unknown }).__mockEngine = {
      status: { state: "notInstalled" },
      variant: null,
      cancelled: false,
      offerError: null,
      failNext: null,
      ...seed,
    };
  }, engine);
  await installTauriMock(page, {
    books: [],
    importScenario: {
      path: "/fixtures/Fixture Book.pdf",
      progress: [{ type: "progress", page: 1, total: 1 }],
      progressDelayMs: 10,
      book: fixtureBook(),
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
}

const sheet = (page: Page) => page.getByTestId("engine-setup-sheet");

test("importing without an engine offers setup and resumes the import once installed", async ({
  page,
}) => {
  await boot(page, {});
  await page.getByTestId("library-import-book").click();
  await expect(sheet(page)).toBeVisible();
  await expect(page.getByTestId("engine-option-lite")).toContainText(
    S["engine.setup.liteTitle"],
  );
  await expect(page.getByTestId("engine-option-cpu")).toContainText(
    S["engine.setup.fullTitle"],
  );

  await page.getByTestId("engine-install-lite").click();
  await expect(page.getByTestId("engine-setup-progress")).toBeVisible();
  await expect(sheet(page)).toBeHidden();
  await expect(page.getByTestId("book-card")).toHaveCount(1);
});

test("Not now leaves the app usable without an engine and without an error", async ({
  page,
}) => {
  await boot(page, {});
  await page.getByTestId("library-import-book").click();
  await page.getByTestId("engine-setup-not-now").click();
  await expect(sheet(page)).toBeHidden();
  await expect(page.getByRole("alert")).toHaveCount(0);
  await expect(page.getByTestId("library-import-book")).toBeEnabled();
});

test("an unreachable download explains itself and retries", async ({
  page,
}) => {
  await boot(page, { offerError: "ERR_ENGINE_OFFLINE: no route" });
  await page.getByTestId("library-import-book").click();
  await expect(page.getByTestId("engine-setup-error")).toHaveText(
    S["engine.setup.error.offline"],
  );
  await page.evaluate(() => {
    (
      window as unknown as { __mockEngine: MockEngine }
    ).__mockEngine.offerError = null;
  });
  await page.getByTestId("engine-setup-retry").click();
  await expect(page.getByTestId("engine-setup-error")).toHaveCount(0);
  await expect(page.getByTestId("engine-option-lite")).toBeVisible();
});

test("a failed install keeps the options and cancel returns to them silently", async ({
  page,
}) => {
  await boot(page, { failNext: "ERR_ENGINE_START" });
  await page.getByTestId("library-import-book").click();
  await page.getByTestId("engine-install-cpu").click();
  await expect(page.getByTestId("engine-setup-error")).toHaveText(
    S["engine.setup.error.startFailed"],
  );
  await expect(page.getByTestId("engine-option-cpu")).toBeVisible();

  await page.getByTestId("engine-install-cpu").click();
  await page.getByTestId("engine-setup-cancel").click();
  await expect(page.getByTestId("engine-option-cpu")).toBeVisible();
  await expect(page.getByTestId("engine-setup-error")).toHaveCount(0);
});

test("the reader engine still imports without asking for more", async ({
  page,
}) => {
  await boot(page, { status: { state: "ready", port: 1 }, variant: "lite" });
  await page.getByTestId("library-import-book").click();
  await expect(page.getByTestId("book-card")).toHaveCount(1);
  await expect(sheet(page)).toHaveCount(0);
});

test("a failed launch explains itself instead of showing the app", async ({
  page,
}) => {
  await page.addInitScript(() => {
    (
      window as unknown as { __mockStartupFailure: unknown }
    ).__mockStartupFailure = {
      code: "databaseDamaged",
      detail: "file is not a database",
      dataDir: "/data/txtcaster",
    };
  });
  await installTauriMock(page, { books: [] });
  await page.goto("/");
  await expect(page.getByTestId("startup-failure")).toContainText(
    S["startup.title.databaseDamaged"],
  );
  await expect(page.getByTestId("startup-quit")).toBeVisible();
  await expect(page.getByTestId("library-import-book")).toHaveCount(0);
});

test("a stream restored silent returns to the player's volume, later mixer changes stay", async ({
  page,
}) => {
  await installTauriMock(page, { books: [] });
  await page.goto("/");
  const audio = page.locator("audio");
  await expect(audio).toHaveCount(1);
  const volumeAfter = (event: string, volume: number) =>
    audio.evaluate(
      (element: HTMLAudioElement, args) => {
        element.volume = args.volume;
        element.dispatchEvent(new Event(args.event));
        return element.volume;
      },
      { event, volume },
    );
  expect(await volumeAfter("playing", 0)).toBe(1);
  await page.waitForTimeout(3100);
  expect(await volumeAfter("volumechange", 0.5)).toBe(0.5);
});
