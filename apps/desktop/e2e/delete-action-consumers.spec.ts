import { expect, test } from "@playwright/test";

import { fixtureBook, fixtureChapters, installTauriMock } from "./tauriMock";

const VIEWPORTS = [390, 768, 876, 1280];

interface Box {
  left: number;
  right: number;
}

interface Geometry {
  testid: string;
  control: Box;
  glyphs: Box[];
  scrollOverflow: number;
  clip: Box;
  viewportWidth: number;
}

async function measure(
  page: import("@playwright/test").Page,
  testids: string[],
): Promise<Geometry[]> {
  return page.evaluate((testids) => {
    const box = (r: DOMRect) => ({ left: r.x, right: r.x + r.width });
    return testids.map((testid) => {
      const el = document.querySelector<HTMLElement>(
        `[data-testid="${testid}"]`,
      );
      if (el == null) throw new Error(`${testid} not in the DOM`);
      const glyphs: Box[] = [];
      const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
      for (let node = walker.nextNode(); node; node = walker.nextNode()) {
        if (!node.textContent?.trim()) continue;
        const range = document.createRange();
        range.selectNodeContents(node);
        for (const r of Array.from(range.getClientRects())) glyphs.push(box(r));
      }
      let clip = { left: 0, right: window.innerWidth };
      for (
        let node = el.parentElement;
        node != null;
        node = node.parentElement
      ) {
        const overflowX = getComputedStyle(node).overflowX;
        if (overflowX !== "visible") {
          const r = box(node.getBoundingClientRect());
          clip = {
            left: Math.max(clip.left, r.left),
            right: Math.min(clip.right, r.right),
          };
        }
      }
      return {
        testid,
        control: box(el.getBoundingClientRect()),
        glyphs,
        scrollOverflow: el.scrollWidth - el.clientWidth,
        clip,
        viewportWidth: window.innerWidth,
      };
    });
  }, testids);
}

async function settle(page: import("@playwright/test").Page, width: number) {
  await page.setViewportSize({ width, height: 900 });
  await expect
    .poll(
      async () => {
        const first = await page.evaluate(() => document.body.scrollHeight);
        await page.waitForTimeout(60);
        const second = await page.evaluate(() => document.body.scrollHeight);
        return first === second ? second : -1;
      },
      { message: `layout must settle after resizing to ${width}` },
    )
    .toBeGreaterThan(0);
}

async function expectUnclipped(
  page: import("@playwright/test").Page,
  view: string,
  testids: string[],
) {
  for (const width of VIEWPORTS) {
    await settle(page, width);
    for (const g of await measure(page, testids)) {
      const where = `${g.testid} in ${view} at viewport ${width}`;
      expect(
        g.control.right,
        `${where} exceeds its clipping ancestor`,
      ).toBeLessThanOrEqual(g.clip.right + 0.5);
      expect(
        g.control.left,
        `${where} starts before its clipping ancestor`,
      ).toBeGreaterThanOrEqual(g.clip.left - 0.5);
      expect(
        g.control.right,
        `${where} exceeds the viewport`,
      ).toBeLessThanOrEqual(g.viewportWidth + 0.5);
      expect(
        g.scrollOverflow,
        `${where} overflows horizontally inside itself`,
      ).toBeLessThanOrEqual(1);
      for (const glyph of g.glyphs) {
        expect(
          glyph.right,
          `${where}: label glyphs cross the clipping ancestor`,
        ).toBeLessThanOrEqual(g.clip.right + 0.5);
        expect(
          glyph.right,
          `${where}: label glyphs cross the control`,
        ).toBeLessThanOrEqual(g.control.right + 0.5);
      }
    }
  }
}

test("every Delete affordance and its confirm pair stay unclipped at 390, 768, 876 and 1280", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 50,
      outputPath: "/exports/fixture-book.mp3",
    },
    // F11-U6 J7: a cast with tutor sessions opens the confirm pair (spec §9
    // U6: this spec "gains the Jobs-panel confirm case").
    voiceSessionsScenario: {
      rows: [
        {
          id: "session-cast",
          book_id: "book-fixture",
          job_id: "job-1",
          started_at: Date.now() - 3_600_000,
          ended_at: Date.now() - 3_000_000,
          end_reason: "ended",
          duration_ms: 600_000,
          turn_count: 4,
          summary_excerpt: null,
          position: { kind: "ms", value: 1000 },
          job_resolves: true,
          provider_changed: false,
        },
      ],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-output-job-1")).toBeVisible({
    timeout: 10_000,
  });
  await expectUnclipped(page, "jobs", ["delete-job-job-1"]);
  await expect(page.getByTestId("job-delete-gate-job-1")).toHaveAttribute(
    "data-gate",
    "confirm",
  );
  await page.getByTestId("delete-job-job-1").click();
  await expectUnclipped(page, "jobs confirm pair", [
    "confirm-delete-job-job-1",
    "cancel-delete-job-job-1",
  ]);
  await page.getByTestId("cancel-delete-job-job-1").click();
  await expect(page.getByTestId("delete-job-job-1")).toBeVisible();

  await page.getByTestId("nav-library").click();
  await expect(page.getByTestId("book-card")).toBeVisible();
  await page.getByTestId("open-book-book-fixture").hover();
  await expectUnclipped(page, "book card", [
    "talk-book-book-fixture",
    "create-audio-book-fixture",
    "book-menu-book-fixture",
  ]);
  await page.getByTestId("book-menu-book-fixture").click();
  await page.getByTestId("delete-book-book-fixture").click();
  await expectUnclipped(page, "book card confirming", [
    "confirm-delete-book-book-fixture",
    "cancel-delete-book-book-fixture",
  ]);
  await page.getByTestId("cancel-delete-book-book-fixture").click();

  await page.getByTestId("nav-audio").click();
  await expect(page.getByTestId("audio-card-export-job-1")).toBeVisible();
  await expectUnclipped(page, "audios", [
    "send-to-phone",
    "delete-all-audio",
    "delete-audio-export-job-1",
  ]);
  await page.getByTestId("delete-all-audio").click();
  await expectUnclipped(page, "audios header confirming", [
    "send-to-phone",
    "confirm-delete-all-audio",
    "cancel-delete-all-audio",
  ]);
  await page.getByTestId("cancel-delete-all-audio").click();
  await page.getByTestId("delete-audio-export-job-1").click();
  await expectUnclipped(page, "audio card confirming", [
    "confirm-delete-audio-export-job-1",
    "cancel-delete-audio-export-job-1",
  ]);
  await page.getByTestId("cancel-delete-audio-export-job-1").click();
  await expect(page.getByTestId("delete-audio-export-job-1")).toBeVisible();
});
