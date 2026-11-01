import { expect, test } from "@playwright/test";

import { buildWav, wavDataUrl } from "./pcmFixture";
import { audioState } from "./readingHarness";
import { fixtureBook, installTauriMock } from "./tauriMock";
import { emitVoice } from "./voiceEnvelope";

// Talk entry (owner decision 2026-10-09): pressing Talk on a book opens a
// fresh tutor session with the BOOK ITSELF — its rendered pages — even when
// the user has a half-listened cast of that book loaded in the player. The
// cast reader (with its narration transport) is never what Talk opens, and
// the cast already in the player is left exactly as it was. Locale-agnostic:
// testids only.

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

test("Talk on a book with a listened cast opens the book's pages, not the cast", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    seededExports: [seeded],
    readingScenario: { mediaDataUrl: wavDataUrl() },
    bookAgentProbe: "notInBuild",
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
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/");

  // Put the cast in the player the way a listener does: resume it from Home.
  await page.getByTestId("study-resume").click();
  const reader = page.getByTestId("reading-view");
  await expect(reader).toBeVisible();
  await expect(reader.getByTestId("player-toggle")).toBeVisible();
  await page.getByTestId("reading-close").click();
  await expect(page.getByTestId("mini-player")).toBeVisible();

  await page.getByTestId("nav-library").click();
  await page.getByTestId("talk-book-book-fixture").click();

  // The book's own pages: the page surface, with no narration transport.
  await expect(reader).toBeVisible();
  await expect(reader).toHaveAttribute("data-surface", "page");
  await expect(reader.getByTestId("player-toggle")).toHaveCount(0);

  // The cast the user was listening to is still loaded, untouched.
  await page.getByTestId("reading-close").click();
  await expect(page.getByTestId("mini-player")).toBeVisible();
});

test("Talk starts a fresh tutor: the book's finished conversation does not greet it", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    bookAgentProbe: "notInBuild",
  });
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();

  // A whole conversation happens, then ends.
  await emitVoice(page, { kind: "lifecycle", state: "connected" });
  await expect(page.getByTestId("voice-session-stop")).toBeVisible();
  await emitVoice(page, {
    kind: "transcript",
    role: "user",
    text: "an earlier question",
  });
  await emitVoice(page, { kind: "ended", reason: "user" });
  const transcript = page.getByTestId("voice-session-transcript");
  await expect(transcript).toContainText("an earlier question");

  await page.getByTestId("reading-close").click();
  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(page.getByText("an earlier question")).toHaveCount(0);
});

// Review #394 (P2): a book-only tutor (no narration job) reads
// `playbackPosMs` as a CHARACTER OFFSET into the book (lib.rs BookCursor).
// A cast playing in the background must not leak its audio clock into it:
// the fresh tutor starts from the first page it shows.
test("a book-only tutor starts at the book's beginning, not the background cast's clock", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    seededExports: [seeded],
    readingScenario: { mediaDataUrl: wavDataUrl() },
    bookAgentProbe: "notInBuild",
    voiceKeysScenario: {
      entries: [
        {
          baseUrl: "https://generativelanguage.googleapis.com",
          provider: "gemini",
          last4: "abcd",
          state: "active",
          keyPresent: true,
          custom: false,
        },
      ],
    },
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
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/");

  await page.getByTestId("study-resume").click();
  await expect
    .poll(async () => (await audioState(page)).currentTime)
    .toBeGreaterThan(1);
  await page.getByTestId("reading-close").click();

  await page.getByTestId("nav-library").click();
  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toHaveAttribute(
    "data-surface",
    "page",
  );
  await page.getByTestId("voice-session-start").first().click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __voiceStarts: unknown[] }).__voiceStarts
            .length,
      ),
    )
    .toBe(1);
  const [started] = await page.evaluate(
    () =>
      (window as unknown as { __voiceStarts: Array<Record<string, unknown>> })
        .__voiceStarts,
  );
  expect(started).toMatchObject({ bookId: "book-fixture", jobId: null });
  expect(started.playbackPosMs).toBe(0);
});

// Review #394 (P2): a Home resume still waiting for media metadata parks a
// seek to the old listening spot. Pressing Talk on a book whose only
// surface is that cast must cancel it — the fresh session starts at 0:00,
// and late metadata may not drag it back to where the user stopped.
test("Talk cancels a resume seek still waiting for the cast's metadata", async ({
  page,
}) => {
  const MEDIA = "http://127.0.0.1:9/media/held-cast.wav";
  const wav = Buffer.from(buildWav());
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route(MEDIA, async (route) => {
    await held;
    // Range support, so the element is seekable (as the real media server).
    const range = route
      .request()
      .headers()
      .range?.match(/bytes=(\d+)-(\d*)/);
    const start = range ? Number(range[1]) : 0;
    const end = range?.[2]
      ? Math.min(Number(range[2]), wav.length - 1)
      : wav.length - 1;
    await route.fulfill({
      status: range ? 206 : 200,
      headers: {
        "Accept-Ranges": "bytes",
        "Content-Type": "audio/wav",
        "Content-Length": String(end - start + 1),
        ...(range
          ? { "Content-Range": `bytes ${start}-${end}/${wav.length}` }
          : {}),
      },
      body: wav.subarray(start, end + 1),
    });
  });
  await installTauriMock(page, {
    // No page renders: Talk's only surface is the cast reader.
    books: [fixtureBook({ pageCount: null })],
    seededExports: [seeded],
    readingScenario: { mediaDataUrl: MEDIA },
    bookAgentProbe: "notInBuild",
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
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/");

  await page.getByTestId("study-resume").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await page.getByTestId("reading-close").click();
  await page.getByTestId("nav-library").click();
  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();

  release();
  await expect
    .poll(async () => (await audioState(page)).duration)
    .toBeGreaterThan(0);
  // Give a parked seek every chance to land before judging. Home's resume
  // card is a play press, and Talk never stops audio the user started, so
  // the cast may be running — but from 0:00, never from the old 30% spot
  // (2.7 s of this 9 s fixture).
  await page.waitForTimeout(300);
  expect((await audioState(page)).currentTime).toBeLessThan(2);
});
