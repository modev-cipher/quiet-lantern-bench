import { expect, test, type Page } from "@playwright/test";

import { msg } from "./i18nText";
import { RAIL_MIN_PX, TUTOR_RAIL_WIDTH_KEY } from "../src/tutorRailModel";
import { enterReading, openReading, play } from "./readingHarness";
import { readingCalls, type MockSessionRow } from "./tauriMock";

const DAY = 86_400_000;
const NOW = Date.now();
const SHOTS = process.env.F11_SHOTS === "1";
const OUT = "../../docs/design/gallery";

function row(at: number, extra: Partial<MockSessionRow> = {}): MockSessionRow {
  return {
    id: `session-${at}`,
    book_id: "book-fixture",
    job_id: null,
    started_at: NOW - (at + 1) * DAY - 38 * 60_000,
    ended_at: NOW - (at + 1) * DAY,
    end_reason: "ended",
    duration_ms: 38 * 60_000 + at * 60_000,
    turn_count: 6,
    summary_excerpt:
      "We proved Lemma 3.2 and started on measure spaces, then took a detour through σ-algebras.",
    position: { kind: "offset", value: 120 },
    job_resolves: false,
    provider_changed: false,
    ...extra,
  };
}

const ROWS = [
  row(0, { end_reason: "unexpected" }),
  row(1, { summary_excerpt: null }),
  row(2),
  row(3),
  row(4),
  row(5),
];

const TURNS = {
  "session-0": [
    {
      turn_idx: 0,
      role: "user" as const,
      text: "Why does σ-additivity matter?",
      created_at: 1,
    },
    {
      turn_idx: 1,
      role: "assistant" as const,
      text: "Because a countable union must stay measurable.",
      created_at: 2,
    },
  ],
};

interface Fit {
  testid: string;
  overflow: number;
  insidePanel: boolean;
}

async function measure(page: Page): Promise<Fit[]> {
  return page.evaluate(
    (testids) => {
      const panel = document
        .querySelector('[data-testid="voice-session-panel"]')!
        .getBoundingClientRect();
      return testids.flatMap((testid) =>
        Array.from(
          document.querySelectorAll<HTMLElement>(`[data-testid="${testid}"]`),
        ).map((el) => {
          const r = el.getBoundingClientRect();
          return {
            testid,
            overflow: el.scrollWidth - el.clientWidth,
            insidePanel:
              r.left >= panel.left - 0.5 && r.right <= panel.right + 0.5,
          };
        }),
      );
    },
    ["session-history-date", "session-history-duration"],
  );
}

async function expectWhole(page: Page): Promise<void> {
  const fits = await measure(page);
  expect(fits.length).toBeGreaterThanOrEqual(10);
  for (const fit of fits) {
    expect(
      fit.overflow,
      `${fit.testid} overflowed its box`,
    ).toBeLessThanOrEqual(0);
    expect(fit.insidePanel, `${fit.testid} escaped the panel`).toBe(true);
  }
}

async function openWithHistory(page: Page, railPx?: number): Promise<void> {
  if (railPx != null) {
    await page.addInitScript(({ key, px }) => localStorage.setItem(key, px), {
      key: TUTOR_RAIL_WIDTH_KEY,
      px: String(railPx),
    });
  }
  await openReading(
    page,
    {},
    {},
    {},
    {
      voiceSessionsScenario: { rows: ROWS, turns: TURNS },
    },
  );
  await play(page);
  await enterReading(page);
}

async function shot(page: Page, name: string): Promise<void> {
  if (!SHOTS) return;
  const panel = page.getByTestId("voice-session-panel");
  await panel.screenshot({ path: `${OUT}/${name}.png` });
  await page.evaluate(() =>
    document.documentElement.setAttribute("data-theme", "dark"),
  );
  await page.waitForTimeout(250);
  await panel.screenshot({ path: `${OUT}/${name}-dark.png` });
  await page.evaluate(() =>
    document.documentElement.setAttribute("data-theme", "light"),
  );
  await page.waitForTimeout(250);
}

for (const width of [1280, 1024]) {
  test(`A10: the rail at ${RAIL_MIN_PX}px in a ${width}px viewport keeps every date and duration whole`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    await openWithHistory(page, RAIL_MIN_PX);
    await expect(page.getByTestId("companion-resize-handle")).toHaveAttribute(
      "aria-valuenow",
      String(RAIL_MIN_PX),
    );
    await expect(
      page.getByTestId("session-history-row-session-0"),
    ).toBeVisible();
    await expect(page.getByTestId("session-history-show-all")).toBeVisible();
    await expect(page.getByTestId("session-history-row-session-5")).toHaveCount(
      0,
    );
    await expectWhole(page);
    await shot(page, `session-history-${width}`);

    await page.getByTestId("session-history-show-all").click();
    await expect(
      page.getByTestId("session-history-row-session-5"),
    ).toBeVisible();
    await expect(page.getByTestId("session-history-show-all")).toHaveCount(0);
    await expectWhole(page);

    await page.getByTestId("session-history-row-session-0").click();
    await expect(page.getByTestId("session-view")).toBeVisible();
    await expect(page.getByTestId("session-view-notice")).toHaveCount(0);
    const log = page.getByRole("log");
    await expect(log).toContainText("Why does σ-additivity matter?");
    await expect(log).toContainText(
      "Because a countable union must stay measurable.",
    );
    await shot(page, `session-view-${width}`);
    await page.getByTestId("session-view-back").click();
    await expect(
      page.getByTestId("session-history-row-session-0"),
    ).toBeVisible();
  });
}

for (const width of [768, 390]) {
  test(`A10: the strip panel at ${width}px keeps every date and duration whole`, async ({
    page,
  }) => {
    await openWithHistory(page);
    await page.setViewportSize({ width, height: 900 });
    await expect(page.getByTestId("study-companion")).toHaveCount(0);
    await page.getByTestId("voice-session-toggle").click();
    await expect(
      page.getByTestId("session-history-row-session-0"),
    ).toBeVisible();
    await expectWhole(page);
    await shot(page, `session-history-${width}`);
  });
}

test("A2 in the rail: zero rows shows the empty line and no list chrome", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await expect(page.getByTestId("session-history-empty")).toHaveText(
    msg("session.historyEmptyBook"),
  );
  await expect(page.getByTestId("session-history-show-all")).toHaveCount(0);
  await expect(page.getByTestId("delete-history-forget")).toHaveCount(0);
  await expect(page.getByTestId("voice-session-start")).toHaveText(msg("voice.startSession"));
});

test("Forget removes every row for the book after the inline confirmation", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await openWithHistory(page);
  await page.getByTestId("delete-history-forget").click();
  await expect(page.getByRole("status")).toContainText(
    msg("session.forgetConfirmQuestion"),
  );
  await page.getByTestId("confirm-delete-history-forget").click();
  await expect(page.getByTestId("session-history-empty")).toBeVisible();
  await expect(page.getByTestId("session-history-row-session-0")).toHaveCount(
    0,
  );
});

const READER_JOB = "job-1";

const PAGE_ROWS = [
  row(0, {
    job_id: READER_JOB,
    position: { kind: "ms", value: 4000 },
    job_resolves: true,
  }),
  row(1, {
    job_id: "job-elsewhere",
    position: { kind: "ms", value: 4000 },
    job_resolves: false,
  }),
];

async function openAtPageOne(page: Page): Promise<void> {
  await page.setViewportSize({ width: 1280, height: 900 });
  await openReading(
    page,
    {},
    {},
    {},
    { voiceSessionsScenario: { rows: PAGE_ROWS, turns: {} } },
  );
  await play(page);
  await enterReading(page);
  await page.evaluate(() => {
    const audio = document.querySelector("audio");
    if (audio != null) {
      audio.pause();
      audio.currentTime = 0.25;
    }
  });
  await page.getByTestId("reading-mode-toggle").click();
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "1",
  );
  await expect(page.getByTestId("reading-page-controls")).toHaveAttribute(
    "data-browsing",
    "no",
  );
}

test("A7: Open on a row of the reader's cast turns the reader to the stored page through the follow reducer", async ({
  page,
}) => {
  await openAtPageOne(page);
  await page.getByTestId("session-history-row-session-0").click();
  await expect(page.getByTestId("session-view")).toBeVisible();
  await expect(page.getByTestId("session-view-notice")).toHaveCount(0);
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "3",
  );
  await expect(page.getByTestId("reading-page-controls")).toHaveAttribute(
    "data-browsing",
    "yes",
  );
  await expect(page.getByTestId("reading-return")).toHaveText(
    msg("session.historyReturnLabel"),
  );
  expect((await readingCalls(page)).followState).toEqual([
    "state=browsing trigger=flip from=following to=browsing:3 seek=no",
  ]);
});

test("A7: an H9 row (another cast) leaves the page alone and shows the H9 notice", async ({
  page,
}) => {
  await openAtPageOne(page);
  await page.getByTestId("session-history-row-session-1").click();
  await expect(page.getByTestId("session-view-notice")).toHaveText(
    msg("session.h9Notice"),
  );
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "1",
  );
  await expect(page.getByTestId("reading-page-controls")).toHaveAttribute(
    "data-browsing",
    "no",
  );
  expect((await readingCalls(page)).followState).toEqual([]);
});
