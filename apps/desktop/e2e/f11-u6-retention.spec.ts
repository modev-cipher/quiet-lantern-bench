// F11-U6 — transcript retention (spec §6.1/§6.2, D-8 ruled (c)): the Settings
// row writes the runtime's setting; the History surface's foot line follows
// it (H12 line alone, H13 line beneath rows, no claim when unreadable).
// Testids + catalog text only, so the suite runs under I18N_E2E_LOCALE=ar.
import { expect, test, type Page } from "@playwright/test";

import { msg } from "./i18nText";
import { enterReading, openReading, play } from "./readingHarness";
import {
  fixtureBook,
  installTauriMock,
  type MockSessionRow,
} from "./tauriMock";

const ROW: MockSessionRow = {
  id: "session-kept",
  book_id: "book-fixture",
  job_id: "job-1",
  started_at: Date.now() - 7_200_000,
  ended_at: Date.now() - 6_600_000,
  end_reason: "ended",
  duration_ms: 600_000,
  turn_count: 3,
  summary_excerpt: "Kept before the switch.",
  position: { kind: "ms", value: 1000 },
  job_resolves: true,
  provider_changed: false,
};

async function openRail(
  page: Page,
  retention: { value?: "keep" | "never"; unreadable?: boolean },
  rows: MockSessionRow[],
): Promise<void> {
  await openReading(
    page,
    {},
    {},
    {},
    {
      retentionScenario: retention,
      voiceSessionsScenario: { rows },
    },
  );
  await play(page);
  await enterReading(page);
  // At the default viewport the companion rail is open on the Tutor tab and
  // the idle panel lists history directly (history-rail-width.spec, A10 wide).
  await expect(page.getByTestId("session-history")).toBeVisible();
}

test("Settings: choosing Never stores it, locks Personalize, and survives a reload", async ({
  page,
}) => {
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  const row = page.getByTestId("retention-settings");
  await expect(row).toBeVisible();
  await expect(row).toContainText(msg("settings.retentionLabel"));
  await expect(page.getByTestId("retention-choice-keep")).toHaveAttribute(
    "aria-checked",
    "true",
  );
  await expect(page.getByTestId("personalize-note")).toHaveCount(0);

  await page.getByTestId("retention-choice-never").click();
  await expect(page.getByTestId("retention-choice-never")).toHaveAttribute(
    "aria-checked",
    "true",
  );
  await expect(page.getByTestId("personalize-note")).toHaveText(
    msg("settings.neverPersistNote"),
  );
  expect(
    await page.evaluate(() => sessionStorage.getItem("mock-retention")),
  ).toBe("never");

  await page.reload();
  await page.getByTestId("nav-settings").click();
  await expect(page.getByTestId("retention-choice-never")).toHaveAttribute(
    "aria-checked",
    "true",
  );
  await page.getByTestId("retention-choice-keep").click();
  await expect(page.getByTestId("personalize-note")).toHaveCount(0);
});

test("H12: under Never with no rows the rail shows the retention line, not the empty line", async ({
  page,
}) => {
  await openRail(page, { value: "never" }, []);
  const line = page.getByTestId("session-history-retention");
  await expect(line).toBeVisible();
  await expect(line).toContainText(msg("session.retentionNever"));
  await expect(page.getByTestId("session-history-empty")).toHaveCount(0);
  await page.getByTestId("session-history-retention-settings").click();
  await expect(page.getByTestId("retention-settings")).toBeVisible();
});

test("H13: rows kept before the switch stay listed above the retention line", async ({
  page,
}) => {
  await openRail(page, { value: "never" }, [ROW]);
  await expect(
    page.getByTestId("session-history-row-session-kept"),
  ).toBeVisible();
  await expect(page.getByTestId("session-history-retention")).toBeVisible();
  await expect(page.getByTestId("session-history-empty")).toHaveCount(0);
});

test("Keep with no rows shows the empty line and no retention line", async ({
  page,
}) => {
  await openRail(page, { value: "keep" }, []);
  await expect(page.getByTestId("session-history-empty")).toHaveText(
    msg("session.historyEmptyBook"),
  );
  await expect(page.getByTestId("session-history-retention")).toHaveCount(0);
});

test("an unreadable retention setting makes no claim: neither line renders", async ({
  page,
}) => {
  await openRail(page, { unreadable: true }, []);
  await expect(page.getByTestId("session-history")).toBeVisible();
  await expect(page.getByTestId("session-history-retention")).toHaveCount(0);
  await expect(page.getByTestId("session-history-empty")).toHaveCount(0);
});
