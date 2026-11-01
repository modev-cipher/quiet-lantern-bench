import { expect, test } from "@playwright/test";

import readingGolden from "./fixtures/reading-timeline.golden.json" with { type: "json" };
import {
  audioState,
  enterReading,
  openReading,
  play,
  waitForTime,
} from "./readingHarness";
import { installTauriMock, fixtureBook } from "./tauriMock";
import { wavDataUrl } from "./pcmFixture";
import { readingCalls } from "./tauriMock";
import { emitVoice } from "./voiceEnvelope";
import { e2eLocale, msg } from "./i18nText";

// The host locale decides the boot direction, so "system" means rtl under I18N_E2E_LOCALE=ar.
const SYSTEM_DIR = e2eLocale === "ar" ? "rtl" : "ltr";

const HELD_WORD_WINDOW = {
  fromMs: readingGolden.chapters[1].segments[0].words[0].startMs,
  untilMs: readingGolden.chapters[1].segments[0].endMs,
};

test("the follow-along entry appears beside the player and opens the overlay", async ({
  page,
}) => {
  await openReading(page);
  await play(page);

  await expect(page.getByTestId("mini-player-follow-along")).toBeVisible();
  await enterReading(page);

  const calls = await readingCalls(page);
  expect(calls.index).toContain("job-1");
});

test("leaving the Book collapses it to the strip, and the strip leads back into it", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);

  await page.getByTestId("reading-close").click();
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
  await expect(page.getByTestId("mini-player")).toBeVisible();

  await page.getByTestId("mini-player-expand").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(page.getByTestId("mini-player")).toHaveCount(0);
});

test("opening and closing the overlay never touches the audio element", async ({
  page,
}) => {
  await openReading(page);
  await play(page);

  await page.evaluate(() => {
    const audio = document.querySelector("audio");
    (audio as unknown as Record<string, unknown>).__identity = "same-node";
  });
  const before = await audioState(page);

  await enterReading(page);
  const during = await page.evaluate(() => {
    const audio = document.querySelector("audio");
    return {
      identity: (audio as unknown as Record<string, unknown>).__identity,
      count: document.querySelectorAll("audio").length,
      paused: audio?.paused ?? true,
    };
  });

  expect(during.identity).toBe("same-node");
  expect(during.count).toBe(1);
  expect(during.paused).toBe(false);

  await page.getByTestId("reading-close").click();
  await expect(page.getByTestId("reading-view")).toHaveCount(0);

  const after = await page.evaluate(() => {
    const audio = document.querySelector("audio");
    return {
      identity: (audio as unknown as Record<string, unknown>).__identity,
      count: document.querySelectorAll("audio").length,
      paused: audio?.paused ?? true,
      currentTime: audio?.currentTime ?? 0,
    };
  });

  expect(after.identity, "the engine stays mounted at App").toBe("same-node");
  expect(after.count).toBe(1);
  expect(after.paused).toBe(false);
  expect(after.currentTime).toBeGreaterThanOrEqual(before.currentTime);
});

test("escape closes the overlay and returns focus", async ({ page }) => {
  await openReading(page);
  await play(page);
  await page.getByTestId("mini-player-follow-along").focus();
  await enterReading(page);

  await page.keyboard.press("Escape");

  await expect(page.getByTestId("reading-view")).toHaveCount(0);
  await expect(page.getByTestId("mini-player-follow-along")).toBeFocused();
});

test("an export with no job offers no follow-along at all", async ({
  page,
}) => {
  await openReading(page);
  await page.evaluate(() => {
    const hook = (window as unknown as Record<string, unknown>)
      .__mockMarkExportStale as ((path?: string) => void) | undefined;
    hook?.();
  });
  await page.reload();
  await page.getByTestId("nav-library").click();
  await page.getByTestId("nav-audio").click();

  await expect(page.getByTestId("mini-player-follow-along")).toHaveCount(0);
});

test("the [ shortcut folds the chapter drawer to a number rail and back", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);

  const outline = page.getByTestId("reading-outline");
  await expect(outline).toHaveAttribute("data-state", "drawer");
  const drawer = (await outline.boundingBox())!;
  await page.keyboard.press("[");
  await expect(outline).toHaveAttribute("data-state", "rail");
  const rail = (await outline.boundingBox())!;
  expect(rail.width).toBeLessThanOrEqual(64);
  expect(rail.width).toBeLessThan(drawer.width);
  const second = page.getByTestId("outline-chapter-1");
  await expect(second).toHaveText("2");
  await expect(second).toHaveAttribute("title", /\S/);
  await page.keyboard.press("[");
  await expect(outline).toHaveAttribute("data-state", "drawer");
});

test("a chapter number on the folded rail seeks like the drawer row", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await page.keyboard.press("[");
  await expect(page.getByTestId("reading-outline")).toHaveAttribute(
    "data-state",
    "rail",
  );
  await page.getByTestId("outline-chapter-1").click();
  await expect(page.getByTestId("outline-chapter-1")).toHaveAttribute(
    "aria-current",
    "true",
  );
});

test("focus mode collapses both side zones and [ brings them back", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);

  await page.getByTestId("focus-toggle").click();
  await expect(page.getByTestId("reading-outline")).toHaveCount(0);
  await expect(page.getByTestId("study-companion")).toHaveCount(0);
  await expect(page.getByTestId("focus-edge-handle")).toBeVisible();

  await page.keyboard.press("[");
  await expect(page.getByTestId("reading-outline")).toBeVisible();
  await expect(page.getByTestId("study-companion")).toBeVisible();
  await expect(page.getByTestId("focus-edge-handle")).toHaveCount(0);
});

test("an outline chapter click seeks to that chapter's own start", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);

  await page.evaluate(async () => {
    const specifier = ["", "src", "playbackStore.ts"].join("/");
    const store = (await import(specifier)) as unknown as {
      dispatch: (action: unknown) => void;
    };
    store.dispatch({ type: "pause" });
  });
  expect((await audioState(page)).currentTime).toBeLessThan(3.4);

  await page.getByTestId("outline-chapter-1").click();
  await expect
    .poll(async () => (await audioState(page)).currentTime)
    .toBeGreaterThanOrEqual(3.45);
});

test("launch lands on Home and never opens the reader by itself", async ({
  page,
}) => {
  const seeded = {
    id: "export-seeded",
    bookId: "book-fixture",
    jobId: "job-1",
    path: "/exports/fixture-book.mp3",
    bytes: 5_000_000,
    format: "mp3",
    mode: "narrate",
    stale: false,
    createdAt: 1_755_000_000_000,
  };
  await installTauriMock(page, {
    books: [fixtureBook()],
    seededExports: [seeded],
    readingScenario: { mediaDataUrl: wavDataUrl() },
  });
  await page.addInitScript(() => {
    localStorage.setItem(
      "txtcaster.listened",
      JSON.stringify({ "export-seeded": 0.3 }),
    );
    localStorage.setItem(
      "txtcaster.listened.recent",
      JSON.stringify(["export-seeded"]),
    );
  });
  await page.goto("/");

  // Resuming is the user's choice: Home offers the hero card, and nothing
  // mounts the full-screen reader until it is pressed.
  await expect(page.getByTestId("study-resume")).toBeVisible();
  await expect(page.getByTestId("reading-view")).toHaveCount(0);

  await page.getByTestId("study-resume").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
});

test("a live tutor pauses playback and suppresses every text highlight, then restores it", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);

  await waitForTime(page, (HELD_WORD_WINDOW.fromMs + 100) / 1000);
  await expect
    .poll(async () => page.locator("[data-word].bg-reading-highlight").count())
    .toBeGreaterThan(0);

  await emitVoice(page, { kind: "lifecycle", state: "connected" });

  await expect(page.locator("[data-word].bg-reading-highlight")).toHaveCount(0);
  await expect.poll(async () => (await audioState(page)).paused).toBe(true);
  const pausedAtMs = (await audioState(page)).currentTime * 1000;
  expect(
    pausedAtMs,
    `tutor pause landed at ${pausedAtMs} ms, outside the fixture's held-word window ${HELD_WORD_WINDOW.fromMs}–${HELD_WORD_WINDOW.untilMs} ms; no highlight can be restored from a wordless position`,
  ).toBeGreaterThanOrEqual(HELD_WORD_WINDOW.fromMs);
  expect(
    pausedAtMs,
    `tutor pause landed at ${pausedAtMs} ms, outside the fixture's held-word window ${HELD_WORD_WINDOW.fromMs}–${HELD_WORD_WINDOW.untilMs} ms; no highlight can be restored from a wordless position`,
  ).toBeLessThan(HELD_WORD_WINDOW.untilMs);

  await emitVoice(page, {
    kind: "ended",
    reason: "the user ended the session",
  });

  await expect
    .poll(async () => page.locator("[data-word].bg-reading-highlight").count())
    .toBeGreaterThan(0);
  expect((await audioState(page)).currentTime * 1000).toBeCloseTo(
    pausedAtMs,
    0,
  );
});

test("focus mode keeps the tutor entry in the transport at every width", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);

  await page.getByTestId("focus-toggle").click();
  await expect(page.getByTestId("ask-tutor")).toBeVisible();

  await page.setViewportSize({ width: 800, height: 900 });
  await expect(page.getByTestId("voice-session-panel")).toHaveCount(0);
  await expect(page.getByTestId("ask-tutor")).toBeVisible();

  await page.getByTestId("ask-tutor").click();
  await expect(page.getByTestId("voice-session-panel")).toBeVisible();
});

test("focus round-trip restores the pre-focus companion tab with the session intact", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);

  await emitVoice(page, { kind: "lifecycle", state: "connected" });
  await expect(page.getByTestId("voice-session-status")).toContainText(
    msg("voice.status.live"),
  );

  await page.getByTestId("companion-tab-book").click();
  await expect(page.getByTestId("companion-tab-book")).toHaveAttribute(
    "aria-selected",
    "true",
  );

  await page.getByTestId("focus-toggle").click();
  await expect(page.getByTestId("study-companion")).toHaveCount(0);
  await page.getByTestId("focus-edge-handle").click();

  await expect(page.getByTestId("companion-tab-book")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await page.getByTestId("companion-tab-tutor").click();
  await expect(page.getByTestId("voice-session-status")).toContainText(
    msg("voice.status.live"),
  );
});

test("the direction setting mirrors the app and survives a relaunch", async ({
  page,
}) => {
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.goto("/");
  await expect(page.locator("html")).toHaveAttribute("dir", SYSTEM_DIR);

  await page.getByTestId("nav-settings").click();
  await page.getByTestId("direction-choice-rtl").click();
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl");

  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
  await expect(page.getByTestId("library")).toBeVisible();

  await page.getByTestId("nav-settings").click();
  await page.getByTestId("direction-choice-system").click();
  await expect(page.locator("html")).toHaveAttribute("dir", SYSTEM_DIR);
});

test("the study companion docks at the reading end, opposite the outline, and its start edge resizes it", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await openReading(page);
  await play(page);
  await enterReading(page);

  const rtl = e2eLocale === "ar";
  const companion = page.getByTestId("study-companion");
  const outline = page.getByTestId("reading-outline");
  await expect(companion).toBeVisible();
  const c = (await companion.boundingBox())!;
  const o = (await outline.boundingBox())!;
  if (rtl) {
    expect(
      c.x + c.width,
      "RTL: the companion sits left of the outline",
    ).toBeLessThanOrEqual(o.x + 1);
    expect(c.x).toBeLessThanOrEqual(1);
  } else {
    expect(
      c.x,
      "LTR: the companion sits right of the outline",
    ).toBeGreaterThanOrEqual(o.x + o.width - 1);
    expect(c.x + c.width).toBeGreaterThanOrEqual(1440 - 1);
  }

  const handle = page.getByTestId("companion-resize-handle");
  const h = (await handle.boundingBox())!;
  const edge = rtl ? c.x + c.width : c.x;
  expect(Math.abs(h.x + h.width / 2 - edge)).toBeLessThanOrEqual(4);

  const before = Number(await handle.getAttribute("aria-valuenow"));
  await page.mouse.move(h.x + h.width / 2, h.y + h.height / 2);
  await page.mouse.down();
  await page.mouse.move(
    h.x + h.width / 2 + (rtl ? 60 : -60),
    h.y + h.height / 2,
    { steps: 4 },
  );
  await page.mouse.up();
  await expect
    .poll(async () => Number(await handle.getAttribute("aria-valuenow")))
    .toBeGreaterThan(before + 40);

  const dragged = Number(await handle.getAttribute("aria-valuenow"));
  await handle.focus();
  await page.keyboard.press(rtl ? "ArrowRight" : "ArrowLeft");
  await expect
    .poll(async () => Number(await handle.getAttribute("aria-valuenow")))
    .toBeGreaterThan(dragged);
});

test("the script column keeps a book measure and sits centred in a wide reader", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1920, height: 1080 });
  await openReading(page);
  await play(page);
  await enterReading(page);
  const script = page.getByTestId("reading-script");
  await expect(script).toBeVisible();
  const text = (await script.boundingBox())!;
  expect(
    text.width,
    "the sentence never stretches past a reading measure",
  ).toBeLessThanOrEqual(38 * 16 + 1);
  const thumb = (await page.getByTestId("reading-facsimile").boundingBox())!;
  const surface = (await page.getByTestId("reading-surface").boundingBox())!;
  const start = Math.min(text.x, thumb.x);
  const end = Math.max(text.x + text.width, thumb.x + thumb.width);
  const left = start - surface.x;
  const right = surface.x + surface.width - end;
  expect(
    Math.abs(left - right),
    "text and page sit centred as one group",
  ).toBeLessThanOrEqual(24);
});
