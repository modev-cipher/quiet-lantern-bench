import { expect, test } from "@playwright/test";

import arSettings from "../src/i18n/messages/ar/settings.json" with { type: "json" };
import arShell from "../src/i18n/messages/ar/shell.json" with { type: "json" };
import enSettings from "../src/i18n/messages/en/settings.json" with { type: "json" };
import enShell from "../src/i18n/messages/en/shell.json" with { type: "json" };
import {
  fixtureBook,
  fixtureChapters,
  installTauriMock,
  type UpdateScenario,
} from "./tauriMock";

// Copy comes from the catalog of the locale under test (I18N_E2E_LOCALE=ar boots
// Arabic), so the same spec pins EN and AR. `parts` splits a template on its
// placeholders; each literal part must appear in the rendered text.
const S = Object.fromEntries(
  Object.entries(
    process.env.I18N_E2E_LOCALE === "ar"
      ? { ...arShell, ...arSettings }
      : { ...enShell, ...enSettings },
  ).filter(([, message]) => typeof message === "string"),
) as Record<string, string>;
const parts = (key: string) =>
  S[key]
    .split(/\{[^}]+\}/)
    .map((x) => x.trim())
    .filter((x) => x.length > 1);
async function expectCopy(
  locator: import("@playwright/test").Locator,
  key: string,
) {
  for (const part of parts(key)) await expect(locator).toContainText(part);
}

// UPD-1 banner + Settings row against a mocked updater (never the real origin).

const AVAILABLE: UpdateScenario = {
  snapshot: {
    phase: "available",
    available_version: "0.2.0",
    notes: "Faster casts",
    install_kind: "restart",
    release_url: "https://example.invalid/releases",
    last_checked_unix: 1_757_000_000,
  },
};

async function boot(
  page: import("@playwright/test").Page,
  updateScenario?: UpdateScenario,
) {
  await installTauriMock(page, { books: [], updateScenario });
  await page.goto("/");
}

async function invoked(page: import("@playwright/test").Page) {
  return page.evaluate(
    () =>
      (window as unknown as { __invokedCommands: string[] }).__invokedCommands,
  );
}

// One installTauriMock per test: a second init script cannot redefine the mock.
for (const [name, snapshot] of [
  ["idle", {}],
  ["up to date", { phase: "up_to_date", last_checked_unix: 1_757_000_000 }],
  ["paused", { phase: "paused", last_checked_unix: 1_757_000_000 }],
  ["disabled", { phase: "disabled", enabled: false }],
] as const) {
  test(`no banner when ${name}`, async ({ page }) => {
    await boot(page, { snapshot });
    await expect(
      page.getByRole("button", { name: S["nav.settings"] }),
    ).toBeVisible();
    await expect(page.getByTestId("app-update-banner")).toHaveCount(0);
  });
}

test("a ready version shows a non-modal banner; Later hides it but Settings still lists it", async ({
  page,
}) => {
  await boot(page, AVAILABLE);
  const banner = page.getByTestId("app-update-banner");
  await expectCopy(banner, "update.readyWithNotes");
  await expect(banner).toContainText("0.2.0");
  await expect(banner).toContainText("Faster casts");
  await expect(page.getByTestId("app-update-install")).toHaveText(
    S["update.install"],
  );
  await expect(page.getByTestId("app-update-casts")).toHaveCount(0);
  await page.getByTestId("app-update-whats-new").click();
  expect(await invoked(page)).toContain("plugin:opener|open_url");
  await expect(page.getByTestId("app-update-install")).toBeVisible();
  await page.getByTestId("app-update-later").click();
  await expect(banner).toHaveCount(0);
  expect(await invoked(page)).toContain("update_dismiss");
  await page.getByRole("button", { name: S["nav.settings"] }).click();
  await expectCopy(
    page.getByTestId("update-status-detail"),
    "settings.updateAvailable",
  );
  await expect(page.getByTestId("update-status-detail")).toContainText("0.2.0");
});

test("with a cast running, the offer says it continues after the restart", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    updateScenario: AVAILABLE,
    jobScenario: {
      chapters: fixtureChapters(),
      seededJobs: [
        {
          id: "running",
          bookId: "book-fixture",
          scope: { kind: "chapters", chapter_ids: ["ch-1"] },
          mode: "narrator",
          voiceProfileId: "preset-kokoro-af-heart",
          outputFormat: "mp3",
          modelOverridesJson: null,
          state: "SYNTHESIZING",
          overnight: false,
          etaSeconds: null,
          reportJson: null,
          outputPathsJson: null,
          createdAt: 1_000,
          updatedAt: 1_000,
        },
      ],
    },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-update-casts")).toHaveText(
    S["update.castsContinue"],
  );
  await expect(page.getByTestId("app-update-whats-new")).toHaveText(
    S["update.whatsNew"],
  );
});

test("one click: progress with cancel, then verifying, then installing", async ({
  page,
}) => {
  await boot(page, AVAILABLE);
  await page.getByTestId("app-update-install").click();
  expect(await invoked(page)).toContain("update_install");
  await expectCopy(
    page.getByTestId("app-update-progress"),
    "update.downloading",
  );
  await expect(page.getByTestId("app-update-cancel")).toBeVisible();
  await expect(page.getByTestId("app-update-progress")).toHaveText(
    S["update.installing"],
  );
  await expect(page.getByTestId("app-update-cancel")).toHaveCount(0);
});

test("cancel mid-download returns to the offer with nothing installed", async ({
  page,
}) => {
  await boot(page, AVAILABLE);
  await page.getByTestId("app-update-install").click();
  await page.getByTestId("app-update-cancel").click();
  expect(await invoked(page)).toContain("update_cancel");
  await expect(page.getByTestId("app-update-install")).toHaveText(
    S["update.install"],
  );
});

test("a failed install says the current version still runs; Dismiss clears it", async ({
  page,
}) => {
  await boot(page, { ...AVAILABLE, installOutcome: "failed" });
  await page.getByTestId("app-update-install").click();
  await expectCopy(page.getByTestId("app-update-failed"), "update.failed");
  await expect(page.getByTestId("app-update-failed")).toContainText(
    "0.1.0-e2e",
  );
  await page.getByTestId("app-update-dismiss").click();
  await expect(page.getByTestId("app-update-banner")).toHaveCount(0);
});

test("an install that cannot self-replace offers the release page instead", async ({
  page,
}) => {
  await boot(page, {
    snapshot: { ...AVAILABLE.snapshot, install_kind: "open" },
  });
  await expect(page.getByTestId("app-update-install")).toHaveText(
    S["update.get"],
  );
  await expect(page.getByTestId("app-update-whats-new")).toHaveCount(0);
  await page.getByTestId("app-update-install").click();
  expect(await invoked(page)).toContain("plugin:opener|open_url");
});

test("Settings: check now reports up to date", async ({ page }) => {
  await boot(page);
  await page.getByRole("button", { name: S["nav.settings"] }).click();
  await expect(page.getByTestId("update-status-line")).toHaveText(
    S["settings.neverChecked"],
  );
  await page.getByTestId("update-check-now").click();
  expect(await invoked(page)).toContain("update_check_now");
  await expectCopy(
    page.getByTestId("update-status-line"),
    "settings.lastChecked",
  );
  await expect(page.getByTestId("update-status-detail")).toHaveText(
    S["settings.upToDate"],
  );
});

test("Settings: a paused publisher shows only the time, no claim", async ({
  page,
}) => {
  await boot(page, {
    snapshot: { phase: "paused", last_checked_unix: 1_757_000_000 },
  });
  await page.getByRole("button", { name: S["nav.settings"] }).click();
  await expectCopy(
    page.getByTestId("update-status-line"),
    "settings.lastChecked",
  );
  await expect(page.getByTestId("update-status-detail")).toHaveCount(0);
});

test("Settings: a build without a pinned key says updates are not set up", async ({
  page,
}) => {
  await boot(page, { snapshot: { phase: "disabled", enabled: false } });
  await page.getByRole("button", { name: S["nav.settings"] }).click();
  await expect(page.getByTestId("update-status-line")).toHaveText(
    S["settings.updatesUnavailable"],
  );
  await expect(page.getByTestId("update-check-now")).toHaveCount(0);
});
