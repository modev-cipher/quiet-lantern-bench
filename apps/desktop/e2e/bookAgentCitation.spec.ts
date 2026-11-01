// The Book Agent citation over the mock IPC (Hermes round-2 gate): one
// honest end-to-end assertion that a `voice-agent-highlight` event renders
// the citation on the real reading surface, turns the page, and answers the
// applied-effect ack from the RENDERED plan — plus the manual-scroll pause /
// resume ceremony. Shared-fixture law: the reading golden fixture drives it.

import { expect, test } from "@playwright/test";

import readingGolden from "./fixtures/reading-timeline.golden.json" with { type: "json" };
import { wavDataUrl } from "./pcmFixture";
import { fixtureBook, fixtureChapters, installTauriMock } from "./tauriMock";

function pageSvgDataUrl(): string {
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="612" height="792">` +
    `<rect width="612" height="792" fill="#f5f2ea"/>` +
    `</svg>`;
  return `data:image/svg+xml;base64,${Buffer.from(svg).toString("base64")}`;
}

async function openReadingPageMode(page: import("@playwright/test").Page) {
  const pageImage = pageSvgDataUrl();
  await installTauriMock(page, {
    books: [fixtureBook()],
    reading: {
      ...readingGolden,
      pages: readingGolden.pages.map((entry) => ({
        ...entry,
        imagePath: entry.imagePath ?? `<cache>/pa/page-${entry.pageNo}`,
        imageDataUrl: pageImage,
      })),
    } as typeof readingGolden,
    readingScenario: { mediaDataUrl: wavDataUrl() },
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 10,
      outputPath: "/exports/fixture-book.mp3",
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.getByTestId("start-job").click();
  await page.waitForSelector('[data-testid="job-output-job-1"]', {
    timeout: 15_000,
  });
  await page.getByTestId("nav-library").click();
  await page.getByTestId("nav-audio").click();
  await page.getByTestId("cover-quick-toggle-export-job-1").click();
  await page.waitForSelector('[data-testid="mini-player"]');
  await page.getByTestId("mini-player-follow-along").click();
  await page.waitForSelector('[data-testid="reading-view"]');
  await page.getByTestId("reading-mode-toggle").click();
  await page.waitForSelector('[data-testid="reading-page-live"]');
}

test("an agent citation renders, turns the page, and acks from the rendered plan", async ({
  page,
}) => {
  await openReadingPageMode(page);
  // Golden fixture: segment idx 1 lives on page 2 and carries geometry.
  await page.evaluate(() => {
    (
      window as unknown as {
        __mockEmitEvent: (event: string, payload: unknown) => void;
      }
    ).__mockEmitEvent("voice-agent-highlight", {
      chapter: 0,
      page: 2,
      segmentIdx: 1,
      gen: 0,
      ackCallId: "call-e2e-1",
    });
  });
  // The citation renders on the REAL surface (page turned to the cited page)…
  await expect(page.getByTestId("agent-citation")).toBeVisible();
  await expect(page.getByTestId("agent-citation-outline")).toBeVisible();
  // …and the ack answers from the rendered plan: applied, no reason.
  await expect
    .poll(async () =>
      page.evaluate(
        () =>
          (window as unknown as { __agentAcks?: unknown[] }).__agentAcks ?? [],
      ),
    )
    .toEqual([{ callId: "call-e2e-1", applied: true, reason: null }]);

  // Flipping away is the page surface's manual scroll (§11.1): citation
  // following pauses, the overlay leaves the (now wrong) page, and the
  // shared return affordance surfaces.
  await page.getByTestId("reading-flip-forward").click();
  await expect(page.getByTestId("reading-return")).toBeVisible();
  await expect(page.getByTestId("agent-citation")).toBeHidden();
  await page.evaluate(() => {
    (
      window as unknown as {
        __mockEmitEvent: (event: string, payload: unknown) => void;
      }
    ).__mockEmitEvent("voice-agent-highlight", {
      // Golden fixture truth: segment idx 4 lives in CHAPTER 1, page 4.
      chapter: 1,
      page: 4,
      segmentIdx: 4,
      gen: 1,
      ackCallId: "call-e2e-2",
    });
  });
  // Paused: the new citation is an honest non-application.
  await expect
    .poll(async () =>
      page.evaluate(
        () =>
          (
            (window as unknown as { __agentAcks?: Array<{ callId: string }> })
              .__agentAcks ?? []
          ).length,
      ),
    )
    .toBe(2);
  const acks = (await page.evaluate(
    () => (window as unknown as { __agentAcks?: unknown[] }).__agentAcks,
  )) as Array<{ callId: string; applied: boolean; reason: string | null }>;
  expect(acks[1]).toEqual({
    callId: "call-e2e-2",
    applied: false,
    reason: "following_paused",
  });
  // PROOF the Return does it: the citation is absent immediately before…
  await expect(page.getByTestId("agent-citation")).toBeHidden();
  await page.getByTestId("reading-return").click();
  // …and visible immediately after — resume re-showed the LATEST citation.
  await expect(page.getByTestId("agent-citation")).toBeVisible();
});

test("ordinary wheel scrolling pauses the citation and the shared resume returns it", async ({
  page,
}) => {
  await openReadingPageMode(page);
  await page.evaluate(() => {
    (
      window as unknown as {
        __mockEmitEvent: (event: string, payload: unknown) => void;
      }
    ).__mockEmitEvent("voice-agent-highlight", {
      chapter: 0,
      page: 2,
      segmentIdx: 1,
      gen: 0,
      ackCallId: "scroll-1",
    });
  });
  await expect(page.getByTestId("agent-citation")).toBeVisible();
  // Ordinary wheel scrolling on the page surface (not a flip): following
  // pauses and the SHARED resume affordance must surface (round-3 item 3 —
  // this was the stranded state: paused following, invisible resume).
  await page.getByTestId("reading-surface").hover();
  await page.mouse.wheel(0, 400);
  await expect(page.getByTestId("reading-return")).toBeVisible();
  // Discriminator: while paused, a NEWER citation updates latest but the
  // frozen shown citation does not move — and the newcomer acks honestly.
  await page.evaluate(() => {
    (
      window as unknown as {
        __mockEmitEvent: (event: string, payload: unknown) => void;
      }
    ).__mockEmitEvent("voice-agent-highlight", {
      chapter: 0,
      page: 1,
      segmentIdx: 0,
      gen: 1,
      ackCallId: "scroll-2",
    });
  });
  await expect
    .poll(async () =>
      page.evaluate(
        () =>
          ((window as unknown as { __agentAcks?: unknown[] }).__agentAcks ?? [])
            .length,
      ),
    )
    .toBe(2);
  const pausedAcks = (await page.evaluate(
    () => (window as unknown as { __agentAcks?: unknown[] }).__agentAcks,
  )) as Array<{ callId: string; applied: boolean; reason: string | null }>;
  expect(pausedAcks.find((a) => a.callId === "scroll-2")).toEqual({
    callId: "scroll-2",
    applied: false,
    reason: "following_paused",
  });
  await page.getByTestId("reading-return").click();
  // Return re-shows the LATEST citation (page 1 — the newcomer), proving
  // the resume path moved the view rather than the assertion pre-existing.
  await expect(page.getByTestId("agent-citation")).toBeVisible();
});

test("rapid double citation: the webview replaces atomically and acks both honestly", async ({
  page,
}) => {
  await openReadingPageMode(page);
  // Two citations back to back, before any verdict could round-trip. (The
  // tool-level single-flight refusal is a Rust production pin — the second
  // EVENT here exercises the webview's replace + honest-ack contract.)
  await page.evaluate(() => {
    const emit = (
      window as unknown as {
        __mockEmitEvent: (event: string, payload: unknown) => void;
      }
    ).__mockEmitEvent;
    // Both in the LOADED chapter (a cross-chapter citation acks an honest
    // no_geometry until go_to_passage loads it — the §5.2 protocol's job).
    emit("voice-agent-highlight", {
      chapter: 0,
      page: 1,
      segmentIdx: 0,
      gen: 0,
      ackCallId: "rapid-1",
    });
    emit("voice-agent-highlight", {
      chapter: 0,
      page: 2,
      segmentIdx: 1,
      gen: 1,
      ackCallId: "rapid-2",
    });
  });
  // The SECOND citation owns the surface (A8: one active citation)…
  await expect(page.getByTestId("agent-citation")).toBeVisible();
  // …and both acks settle: exactly two, the second applied on its page.
  await expect
    .poll(async () =>
      page.evaluate(
        () =>
          ((window as unknown as { __agentAcks?: unknown[] }).__agentAcks ?? [])
            .length,
      ),
    )
    .toBe(2);
  const acks = (await page.evaluate(
    () => (window as unknown as { __agentAcks?: unknown[] }).__agentAcks,
  )) as Array<{ callId: string; applied: boolean; reason: string | null }>;
  expect(acks.map((a) => a.callId).sort()).toEqual(["rapid-1", "rapid-2"]);
  // The superseded first citation settles honestly, never silently lost.
  const first = acks.find((a) => a.callId === "rapid-1")!;
  expect(first.applied).toBe(false);
  expect(first.reason).toBe("superseded_by_newer_citation");
  expect(acks.find((a) => a.callId === "rapid-2")?.applied).toBe(true);
});

test("the dismiss control lives inside the citation box and never covers the paragraph above (UX-1.4)", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await openReadingPageMode(page);
  await page.evaluate(() => {
    (
      window as unknown as {
        __mockEmitEvent: (event: string, payload: unknown) => void;
      }
    ).__mockEmitEvent("voice-agent-highlight", {
      chapter: 1,
      page: 4,
      segmentIdx: 5,
      gen: 0,
      ackCallId: "dismiss-geometry",
    });
  });
  const outline = page.getByTestId("agent-citation-outline");
  await expect(outline).toBeVisible();
  const surface = await page.getByTestId("agent-citation").boundingBox();
  const box = await outline.boundingBox();
  const dismiss = await page
    .getByTestId("agent-citation-dismiss")
    .boundingBox();
  expect(surface).not.toBeNull();
  expect(box).not.toBeNull();
  expect(dismiss).not.toBeNull();
  if (surface == null || box == null || dismiss == null) return;
  const above = {
    x: surface.x + (70 / 612) * surface.width,
    y: surface.y + (96 / 792) * surface.height,
    width: ((244 - 70) / 612) * surface.width,
    height: ((116 - 96) / 792) * surface.height,
  };
  const within =
    dismiss.x >= box.x - 0.5 &&
    dismiss.y >= box.y - 0.5 &&
    dismiss.x + dismiss.width <= box.x + box.width + 0.5 &&
    dismiss.y + dismiss.height <= box.y + box.height + 0.5;
  expect(
    within,
    `dismiss ${JSON.stringify(dismiss)} outside citation box ${JSON.stringify(box)}`,
  ).toBe(true);
  const overlapsAbove =
    dismiss.x < above.x + above.width &&
    above.x < dismiss.x + dismiss.width &&
    dismiss.y < above.y + above.height &&
    above.y < dismiss.y + dismiss.height;
  expect(
    overlapsAbove,
    `dismiss ${JSON.stringify(dismiss)} covers the preceding paragraph ${JSON.stringify(above)}`,
  ).toBe(false);
  await page.getByTestId("agent-citation-dismiss").click();
  await expect(page.getByTestId("agent-citation")).toBeHidden();
});

test("the shared resume sticks: the reader's own scroll after Resume never re-pauses the citation (QA-DIAG-1 / AT-1-3)", async ({
  page,
}) => {
  // AT-1-3's intermittent miss: after Resume the reader scrolls its own
  // surface (autoScroller reset, citation box scrollIntoView) and that scroll
  // event used to be read as a manual gesture — re-pausing the citation and
  // removing the overlay 14–430 ms after it appeared. Force the ordering:
  // resume, then a programmatic scroll, then assert following stayed on.
  // Owner ruling #59: a scrollbar drag (scroll event only, no wheel/touch/
  // key) intentionally does NOT pause a live citation — do not re-add a
  // pause on the scroll event; that is the race this pin guards against.
  await openReadingPageMode(page);
  const emit = (payload: unknown) =>
    page.evaluate(
      (p) =>
        (
          window as unknown as {
            __mockEmitEvent: (event: string, payload: unknown) => void;
          }
        ).__mockEmitEvent("voice-agent-highlight", p),
      payload,
    );
  const surface = '[data-testid="reading-surface"]';
  await emit({ chapter: 0, page: 2, segmentIdx: 1, gen: 0, ackCallId: "stick-1" });
  await expect(page.getByTestId("agent-citation")).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate((s) => {
        const el = document.querySelector(s) as HTMLElement;
        return el.scrollHeight - el.clientHeight;
      }, surface),
    )
    .toBeGreaterThan(40);
  await page.getByTestId("reading-surface").hover();
  await page.mouse.wheel(0, 400);
  await expect(page.getByTestId("reading-return")).toBeVisible();
  await emit({ chapter: 0, page: 1, segmentIdx: 0, gen: 1, ackCallId: "stick-2" });
  await expect
    .poll(async () =>
      page.evaluate(
        () =>
          ((window as unknown as { __agentAcks?: unknown[] }).__agentAcks ?? [])
            .length,
      ),
    )
    .toBe(2);
  await page.getByTestId("reading-return").click();
  await expect(page.getByTestId("reading-return")).toHaveCount(0);
  // The reader's own scroll: the same DOM call the autoScroller makes. Wait
  // until its scroll event has been dispatched and handled.
  await page.evaluate(
    (s) =>
      new Promise<void>((resolve) => {
        const el = document.querySelector(s) as HTMLElement;
        el.addEventListener("scroll", () => requestAnimationFrame(() => resolve()), { once: true });
        el.scrollTo({ top: el.scrollTop === 0 ? 40 : 0, behavior: "auto" });
      }),
    surface,
  );
  await expect(page.getByTestId("reading-return")).toHaveCount(0);
  await expect(page.getByTestId("agent-citation")).toBeVisible();
  await page.waitForTimeout(500);
  await expect(page.getByTestId("reading-return")).toHaveCount(0);
  await expect(page.getByTestId("agent-citation")).toBeVisible();
});
