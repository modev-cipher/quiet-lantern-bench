import { expect, test, type Page } from "@playwright/test";
import { fixtureBook, installTauriMock } from "./tauriMock";
import { emitVoice, VOICE_SESSION_ID } from "./voiceEnvelope";
import { openReading, play, enterReading } from "./readingHarness";
import golden from "./fixtures/reading-timeline.golden.json" with { type: "json" };

async function recordAcks(page: Page) {
  await page.evaluate(
    ({ sessionId }) => {
      const w = window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args?: Record<string, unknown>,
          ) => Promise<unknown>;
        };
        __mockEmitEvent: (name: string, payload: unknown) => void;
        __navAcks: Record<string, unknown>[];
      };
      w.__navAcks = [];
      const original = w.__TAURI_INTERNALS__.invoke;
      w.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
        if (cmd === "voice_session_navigation_ack" && args != null) {
          w.__navAcks.push(args);
          w.__mockEmitEvent("voice-session-event", {
            session_id: sessionId,
            book_id: "book-fixture",
            event: {
              kind: "navigation_settled",
              call_id: args.callId,
              tool: "jump_to_page",
              applied:
                args.disposition === "applied" || args.disposition === "resume",
              held: args.disposition === "held",
            },
          });
        }
        return original(cmd, args);
      };
    },
    { sessionId: VOICE_SESSION_ID },
  );
}

async function open(page: Page, locale = "en", brokenPages: number[] = []) {
  await installTauriMock(page, {
    books: [fixtureBook({ pageCount: 513 })],
    hostLocale: locale,
    bookAgentProbe: "notInBuild",
    readingScenario: { brokenPages },
    reading: {
      ...golden,
      pages: [
        ...golden.pages,
        ...[16, 28, 30].map((pageNo) => ({
          ...golden.pages[0],
          pageNo,
          imagePath: `/cache/page-${pageNo}.png`,
        })),
      ],
      figures: golden.figures.map((figure) => ({ ...figure, pageNo: 16 })),
    },
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.emulateMedia({ colorScheme: locale === "ar" ? "dark" : "light" });
  await page.goto("/");
  await expect(page.locator("html")).toHaveAttribute("lang", locale);
  await page.getByTestId("nav-library").click();
  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await recordAcks(page);
  await emitVoice(page, { kind: "lifecycle", state: "connected" });
  await emitVoice(page, {
    kind: "navigation",
    tool: "jump_to_page",
    pos_ms: 12553,
    page_no: 16,
    chapter_idx: 7,
  });
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "16",
  );
  await page.evaluate(() =>
    (
      window as unknown as {
        __mockEmitEvent: (name: string, payload: unknown) => void;
      }
    ).__mockEmitEvent("voice-teacher-anchor", {
      kind: "figure",
      id: "figure-9",
      gen: 0,
    }),
  );
  await expect(page.getByTestId("reading-spotlight")).toBeVisible();
}

async function request(page: Page, callId: string, pageNo: number) {
  await emitVoice(page, {
    kind: "activity",
    state: "tool",
    detail: "jump_to_page",
    target: String(pageNo),
  });
  await emitVoice(page, {
    kind: "navigation_requested",
    call_id: callId,
    generation: 0,
    tool: "jump_to_page",
    pos_ms: 32420,
    page_no: pageNo,
  });
}

async function acks(page: Page) {
  return page.evaluate(
    () =>
      (window as unknown as { __navAcks: Record<string, unknown>[] }).__navAcks,
  );
}

for (const locale of ["en", "ar"]) {
  test(`${locale}: scroll and an old figure do not block the next tutor destination`, async ({
    page,
  }) => {
    await open(page, locale);
    await page.getByTestId("reading-surface").hover();
    await page.mouse.wheel(0, 280);
    await expect(page.getByTestId("reading-return")).toHaveCount(0);
    await request(page, "scroll-next", 28);
    await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
      "data-page",
      "28",
    );
    await expect(page.getByTestId("reading-spotlight")).toHaveCount(0);
    await expect
      .poll(() => acks(page))
      .toContainEqual({
        sessionId: VOICE_SESSION_ID,
        callId: "scroll-next",
        generation: 0,
        pageNo: 28,
        disposition: "applied",
      });
    await expect(page.getByTestId("tutor-navigation-controls")).toContainText(
      "28",
    );
  });
}

test("explicitly holding a page keeps the newest destination and Return catches up", async ({
  page,
}) => {
  await open(page);
  await page.getByTestId("tutor-keep-page").click();
  await request(page, "held-28", 28);
  await expect
    .poll(() => acks(page))
    .toContainEqual(
      expect.objectContaining({ callId: "held-28", disposition: "held" }),
    );
  await request(page, "held-30", 30);
  await expect
    .poll(() => acks(page))
    .toContainEqual(
      expect.objectContaining({ callId: "held-30", disposition: "held" }),
    );
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "16",
  );
  await expect(page.getByTestId("tutor-navigation-controls")).toContainText(
    "30",
  );
  await page.getByTestId("reading-return").click();
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "30",
  );
  await expect
    .poll(() => acks(page))
    .toContainEqual(
      expect.objectContaining({ callId: "held-30", disposition: "resume" }),
    );
});

test("an unreadable destination reports failure and keeps the last confirmed page", async ({
  page,
}) => {
  await open(page, "en", [28]);
  await request(page, "broken-28", 28);
  await expect
    .poll(() => acks(page))
    .toContainEqual(
      expect.objectContaining({
        callId: "broken-28",
        disposition: "unavailable",
      }),
    );
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "16",
  );
  await expect(page.getByTestId("voice-session-activity")).not.toContainText(
    "Turned to document page",
  );
});

test("an already applied destination remains available after manual browsing", async ({
  page,
}) => {
  await open(page);
  await request(page, "visited-28", 28);
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "28",
  );
  await page.getByTestId("reading-flip-back").click();
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "27",
  );
  await page.getByTestId("reading-return").click();
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "28",
  );
  await expect
    .poll(() => acks(page))
    .toContainEqual(
      expect.objectContaining({ callId: "visited-28", disposition: "resume" }),
    );
  const initial = (await acks(page)).filter(
    (ack) => ack.callId === "visited-28" && ack.disposition === "applied",
  );
  expect(initial).toHaveLength(1);
  await request(page, "same-page-28", 28);
  await expect
    .poll(() => acks(page))
    .toContainEqual(
      expect.objectContaining({
        callId: "same-page-28",
        disposition: "applied",
      }),
    );
});

for (const gesture of ["keyboard", "touch", "none"]) {
  test(`${gesture} does not let an old figure override the next page`, async ({
    page,
  }) => {
    await open(page);
    const surface = page.getByTestId("reading-surface");
    if (gesture === "keyboard") {
      await surface.focus();
      await page.keyboard.press("PageDown");
    }
    if (gesture === "touch") await surface.dispatchEvent("touchmove");
    await request(page, `next-${gesture}`, 28);
    await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
      "data-page",
      "28",
    );
    await expect
      .poll(() => acks(page))
      .toContainEqual(
        expect.objectContaining({
          callId: `next-${gesture}`,
          disposition: "applied",
        }),
      );
  });
}

test("ending a session while its image loads prevents a late page change", async ({
  page,
}) => {
  let release: () => void = () => {};
  let served: () => void = () => {};
  let started = false;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  const finished = new Promise<void>((resolve) => {
    served = resolve;
  });
  await page.route("https://asset.localhost/**", async (route) => {
    started = true;
    await held;
    await route.fulfill({
      status: 200,
      contentType: "image/svg+xml",
      body: '<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>',
    });
    served();
  });
  try {
    await open(page, "en", [28]);
    await request(page, "late-image", 28);
    await expect.poll(() => started).toBe(true);
    await emitVoice(page, { kind: "ended", reason: "user" });
    release();
    await finished;
    await page.evaluate(
      () =>
        new Promise<void>((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
        ),
    );
    await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
      "data-page",
      "16",
    );
    expect(
      (await acks(page)).some(
        (ack) => ack.callId === "late-image" && ack.disposition === "applied",
      ),
    ).toBe(false);
  } finally {
    release();
  }
});

test("Arabic navigation controls fit a narrow window and work from the keyboard", async ({
  page,
}) => {
  await open(page, "ar");
  await page.setViewportSize({ width: 900, height: 720 });
  const keep = page.getByTestId("tutor-keep-page");
  await keep.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("reading-return")).toBeFocused();
  await request(page, "narrow-held", 28);
  const controls = page.getByTestId("tutor-navigation-controls");
  await expect(controls).toContainText("28");
  const size = await controls.evaluate((el) => ({
    width: el.clientWidth,
    content: el.scrollWidth,
  }));
  expect(size.content).toBeLessThanOrEqual(size.width + 1);
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "28",
  );
  await expect(page.getByTestId("tutor-keep-page")).toBeFocused();
});

test("the native Agent purpose preserves the legacy reader follow controls", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await page.evaluate(
    ({ sessionId }) => {
      (
        window as unknown as {
          __mockEmitEvent: (name: string, payload: unknown) => void;
        }
      ).__mockEmitEvent("voice-session-event", {
        session_id: sessionId,
        book_id: "book-fixture",
        purpose: "agent",
        event: { kind: "lifecycle", state: "connected" },
      });
    },
    { sessionId: VOICE_SESSION_ID },
  );
  await expect(page.getByTestId("tutor-navigation-controls")).toHaveCount(0);
  await expect(page.getByTestId("reading-page-controls")).toHaveCount(1);
  await page.getByTestId("reading-flip-forward").click();
  await expect(page.getByTestId("reading-page-controls")).toHaveAttribute(
    "data-browsing",
    "yes",
  );
  await page.getByTestId("reading-return").click();
  await expect(page.getByTestId("reading-page-controls")).toHaveAttribute(
    "data-browsing",
    "no",
  );
  await expect(page.getByTestId("voice-session-stop")).toBeVisible();
});

test("a cast reader acknowledges the actual page preview in Script view", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  const surface = page.getByTestId("reading-surface");
  if ((await surface.getAttribute("data-mode")) !== "script")
    await page.getByTestId("reading-mode-toggle").click();
  await expect(surface).toHaveAttribute("data-mode", "script");
  await recordAcks(page);
  await emitVoice(page, { kind: "lifecycle", state: "connected" });
  await request(page, "cast-script-page", 4);
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "4",
  );
  await expect
    .poll(() => acks(page))
    .toContainEqual(
      expect.objectContaining({
        callId: "cast-script-page",
        disposition: "applied",
      }),
    );
});
