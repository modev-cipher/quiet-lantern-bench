import { expect, test } from "@playwright/test";

import { msg } from "./i18nText";
import { enterReading, openReading, play } from "./readingHarness";

const SUPPRESSED = [
  {
    id: "sb-certain-1",
    jobId: "job-1",
    pageNo: 2,
    role: "header",
    reason: "repetition",
    certainty: "certain",
    text: "RUNNING HEAD",
    words: 2,
  },
  {
    id: "sb-uncertain-1",
    jobId: "job-1",
    pageNo: 9,
    role: "footer",
    reason: "layout-label",
    certainty: "uncertain",
    text: "Access for free at example.org",
    words: 5,
  },
  {
    id: "sb-certain-2",
    jobId: "job-1",
    pageNo: 4,
    role: "footer",
    reason: "repetition",
    certainty: "certain",
    text: "4 CHAPTER ONE",
    words: 3,
  },
];

test("the reading view lists suppressed blocks with uncertain rows first", async ({
  page,
}) => {
  await openReading(page, {}, {}, {}, { suppressedBlocks: SUPPRESSED });
  await play(page);
  await enterReading(page);

  const toggle = page.getByTestId("reading-suppressed-toggle");
  await expect(toggle).toBeVisible();
  await expect(toggle).toHaveText(msg("reader.blocksRemoved", { count: 3 }));

  await toggle.click();
  const panel = page.getByTestId("reading-suppressed-panel");
  await expect(panel).toBeVisible();

  const rows = panel.locator("li");
  await expect(rows).toHaveCount(3);
  await expect(rows.nth(0)).toHaveAttribute("data-certainty", "uncertain");
  await expect(rows.nth(0)).toContainText("Access for free at example.org");
  await expect(rows.nth(0)).toContainText(msg("reader.checkThisOne").trim());
  await expect(rows.nth(1)).toContainText("RUNNING HEAD");
  await expect(rows.nth(2)).toContainText("4 CHAPTER ONE");

  await toggle.click();
  await expect(panel).toHaveCount(0);
});

test("a book with zero suppressions renders no affordance at all", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);

  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(page.getByTestId("reading-suppressed-toggle")).toHaveCount(0);
});
