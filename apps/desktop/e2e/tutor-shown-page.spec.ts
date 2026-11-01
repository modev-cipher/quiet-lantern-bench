import { expect, test, type Page } from "@playwright/test";

import { enterReading, openReading, play } from "./readingHarness";
import { emitVoice } from "./voiceEnvelope";

async function recordPageShownReports(page: Page): Promise<void> {
  await page.evaluate(() => {
    const w = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args?: Record<string, unknown>,
        ) => Promise<unknown>;
      };
      __pageShown: unknown[];
    };
    w.__pageShown = [];
    const original = w.__TAURI_INTERNALS__.invoke;
    w.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
      if (cmd === "voice_session_page_shown") w.__pageShown.push(args);
      return original(cmd, args);
    };
  });
}

async function pageShownReports(page: Page): Promise<unknown[]> {
  const reports = await page.evaluate(
    () => (window as unknown as { __pageShown: unknown[] }).__pageShown,
  );
  for (const report of reports) {
    expect(report).toMatchObject({
      sessionId: "0192a7c8-1f2e-7c3d-9a4b-5c6d7e8f9a0b",
      bookId: "book-fixture",
      jobId: "job-1",
    });
  }
  return reports.map((report) => ({
    pageNo: (report as { pageNo: number | null }).pageNo,
  }));
}

async function emitAnchor(page: Page): Promise<void> {
  await page.evaluate(() => {
    (
      window as unknown as {
        __mockEmitEvent: (name: string, data: unknown) => void;
      }
    ).__mockEmitEvent("voice-teacher-anchor", {
      kind: "figure",
      id: "figure-9",
      gen: 0,
    });
  });
}

test("the tutor hears which page the reader shows: anchored, flipped, then guided to the newest destination", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await page.evaluate(() => {
    const audio = document.querySelector("audio");
    if (audio != null) {
      audio.pause();
      audio.currentTime = 0.25;
    }
  });
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "1",
  );
  await recordPageShownReports(page);
  await emitVoice(page, { kind: "lifecycle", state: "connected" });
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "1",
  );
  await expect.poll(() => pageShownReports(page)).toEqual([{ pageNo: 1 }]);

  await emitAnchor(page);
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "2",
  );
  await expect
    .poll(() => pageShownReports(page))
    .toEqual([{ pageNo: 1 }, { pageNo: 2 }]);

  await page.getByTestId("reading-flip-forward").click();
  await expect(page.getByTestId("tutor-navigation-controls")).toBeVisible();
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "3",
  );
  await page.getByTestId("reading-flip-forward").click();
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "4",
  );
  await expect
    .poll(() => pageShownReports(page))
    .toEqual([{ pageNo: 1 }, { pageNo: 2 }, { pageNo: 3 }, { pageNo: 4 }]);

  await emitVoice(page, {
    kind: "navigation",
    tool: "jump_to_page",
    pos_ms: 3600,
    page_no: 4,
    chapter_idx: 1,
  });
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "4",
  );
  await expect
    .poll(() => pageShownReports(page))
    .toEqual([{ pageNo: 1 }, { pageNo: 2 }, { pageNo: 3 }, { pageNo: 4 }]);
  await page.getByTestId("tutor-keep-page").click();
  await page.getByTestId("reading-flip-back").click();
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "3",
  );
  await page.getByTestId("reading-return").click();
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "4",
  );
  await expect
    .poll(() => pageShownReports(page))
    .toEqual([
      { pageNo: 1 },
      { pageNo: 2 },
      { pageNo: 3 },
      { pageNo: 4 },
      { pageNo: 3 },
      { pageNo: 4 },
    ]);
});
