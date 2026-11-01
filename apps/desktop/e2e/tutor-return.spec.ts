import { expect, test, type Page } from "@playwright/test";
import { msg } from "./i18nText";
import { fixtureBook, installTauriMock } from "./tauriMock";
import { emitVoice, VOICE_SESSION_ID } from "./voiceEnvelope";
import { openReading, play, enterReading } from "./readingHarness";

const REPLY = "This conversation belongs to the original book.";

async function openLive(page: Page, locale = "en") {
  await installTauriMock(page, {
    books: [
      fixtureBook(),
      fixtureBook({ id: "book-B", title: "Another book" }),
    ],
    hostLocale: locale,
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
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/");
  await page.getByTestId("talk-book-book-fixture").click();
  await emitVoice(page, { kind: "lifecycle", state: "connected" });
  await emitVoice(page, { kind: "mode", mode: "voice", model: "live-model" });
  await emitVoice(page, { kind: "mic_state", state: "muted", detail: "user" });
  await emitVoice(page, { kind: "transcript", role: "assistant", text: REPLY });
  await emitVoice(page, { kind: "activity", state: "idle", detail: "" });
  await emitVoice(page, {
    kind: "navigation",
    tool: "jump_to_page",
    pos_ms: 500,
    page_no: 2,
    chapter_idx: 0,
  });
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "2",
  );
  await expect(page.getByTestId("voice-session-transcript")).toContainText(
    REPLY,
  );
}

async function leave(page: Page) {
  await page.getByTestId("book-activity").click();
  await page.getByTestId("open-production").click();
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
}

async function commands(page: Page) {
  return page.evaluate(
    () =>
      (window as unknown as { __invokedCommands: string[] }).__invokedCommands,
  );
}

const sessionMutations =
  /^(voice_session_(start|stop|mute|unmute|gate|ungate|enter_interactive|leave_interactive))$/;

for (const locale of ["en", "ar"]) {
  test(`${locale}: Return restores the conversation and page without changing the session or microphone`, async ({
    page,
  }) => {
    await openLive(page, locale);
    const before = (await commands(page)).filter((cmd) =>
      sessionMutations.test(cmd),
    );
    await leave(page);
    await emitVoice(page, {
      kind: "transcript",
      role: "user",
      text: "Still here while browsing Casting",
    });
    await page.getByTestId("return-to-tutor").click();
    await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
      "data-page",
      "2",
    );
    await expect(page.getByTestId("voice-session-transcript")).toContainText(
      REPLY,
    );
    await expect(page.getByTestId("voice-session-transcript")).toContainText(
      "Still here while browsing Casting",
    );
    await expect(page.getByTestId("voice-session-stop")).toBeVisible();
    expect(
      (await commands(page)).filter((cmd) => sessionMutations.test(cmd)),
    ).toEqual(before);
  });
}

test("the corner launcher rejoins the book instead of opening a standalone tutor", async ({
  page,
}) => {
  await openLive(page);
  await leave(page);
  await page.getByTestId("tutor-launcher").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(page.getByTestId("study-standalone-tutor")).toHaveCount(0);
  await expect(page.getByTestId("voice-session-transcript")).toContainText(
    REPLY,
  );
});

test("another book stays isolated and its page reports do not reach the first session", async ({
  page,
}) => {
  await openLive(page);
  await leave(page);
  await page.getByTestId("nav-library").click();
  await page.getByTestId("talk-book-book-B").click();
  await expect(page.getByTestId("voice-session-transcript")).toHaveCount(0);
  await expect(page.getByTestId("voice-session-elsewhere")).toBeVisible();
  const before = (await commands(page)).filter(
    (cmd) => cmd === "voice_session_page_turned",
  ).length;
  await page.getByTestId("reading-flip-forward").click();
  expect(
    (await commands(page)).filter((cmd) => cmd === "voice_session_page_turned"),
  ).toHaveLength(before);
  await page.getByTestId("voice-session-elsewhere").getByRole("button").click();
  await expect(page.getByTestId("voice-session-transcript")).toContainText(
    REPLY,
  );
});

test("Back and Forward keep the reader session while crossing Casting and Settings", async ({
  page,
}) => {
  await openLive(page);
  await leave(page);
  await page.getByTestId("app-back").click();
  await expect(page.getByTestId("voice-session-transcript")).toContainText(
    REPLY,
  );
  await page.getByTestId("app-forward").click();
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("app-back").click();
  await expect(page.getByTestId("return-to-tutor")).toBeVisible();
  await page.getByTestId("app-back").click();
  await expect(page.getByTestId("voice-session-transcript")).toContainText(
    REPLY,
  );
});

test("End arriving while away removes the live return action and does not restart", async ({
  page,
}) => {
  await openLive(page);
  await leave(page);
  await emitVoice(page, { kind: "ended", reason: "stopped" });
  await expect(page.getByTestId("return-to-tutor")).toHaveCount(0);
  expect(
    (await commands(page)).filter((cmd) => cmd === "voice_session_start"),
  ).toEqual([]);
});

test("an unsent draft and page scroll survive a temporary visit to Casting", async ({
  page,
}) => {
  await openLive(page);
  await page.getByTestId("voice-session-input").fill("An unfinished question");
  const surface = page.getByTestId("reading-surface");
  await surface.evaluate((el) => {
    el.scrollTop = 120;
    el.dispatchEvent(new Event("scroll"));
  });
  const top = await surface.evaluate((el) => el.scrollTop);
  expect(top).toBeGreaterThan(0);
  await leave(page);
  await page.getByTestId("return-to-tutor").click();
  await expect(page.getByTestId("voice-session-input")).toHaveValue(
    "An unfinished question",
  );
  await expect
    .poll(() =>
      surface.evaluate(
        (el, expected) => Math.abs(el.scrollTop - expected),
        top,
      ),
    )
    .toBeLessThan(2);
});

test("a navigation request received while away settles unavailable and never replays on Return", async ({
  page,
}) => {
  await openLive(page);
  await page.evaluate(() => {
    const w = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args?: Record<string, unknown>,
        ) => Promise<unknown>;
      };
      __mockEmitEvent: (event: string, payload: unknown) => void;
      __returnAcks: Record<string, unknown>[];
    };
    w.__returnAcks = [];
    const original = w.__TAURI_INTERNALS__.invoke;
    w.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
      if (cmd === "voice_session_navigation_ack" && args != null) {
        w.__returnAcks.push(args);
        w.__mockEmitEvent("voice-session-event", {
          session_id: args.sessionId,
          book_id: "book-fixture",
          event: {
            kind: "navigation_settled",
            call_id: args.callId,
            tool: "jump_to_page",
            applied: false,
            held: false,
          },
        });
      }
      return original(cmd, args);
    };
  });
  await leave(page);
  await emitVoice(page, {
    kind: "navigation_requested",
    tool: "jump_to_page",
    call_id: "away-request",
    generation: 0,
    pos_ms: 900,
    page_no: 3,
  });
  const acks = () =>
    page.evaluate(
      () =>
        (window as unknown as { __returnAcks: Record<string, unknown>[] })
          .__returnAcks,
    );
  await expect.poll(acks).toEqual([
    expect.objectContaining({
      callId: "away-request",
      disposition: "unavailable",
    }),
  ]);
  await page.getByTestId("return-to-tutor").click();
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "2",
  );
  expect(await acks()).toHaveLength(1);
});

test("a UI reload rejoins native identity but discloses that earlier live messages are unavailable", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    bookAgentProbe: "notInBuild",
    activeVoiceSession: {
      sessionId: VOICE_SESSION_ID,
      bookId: "book-fixture",
      jobId: null,
      purpose: "tutor",
    },
  });
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/");
  await page.getByTestId("return-to-tutor").click();
  await emitVoice(page, {
    kind: "transcript",
    role: "user",
    text: "This message exists only before the UI reload",
  });
  await expect(page.getByTestId("voice-session-transcript")).toContainText(
    "only before",
  );
  await page.reload();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(page.getByTestId("voice-session-transcript")).toHaveCount(0);
  await expect(page.getByTestId("voice-session-panel")).toContainText(
    msg("voice.notice.adopted"),
  );
  await page.getByTestId("voice-session-details-toggle").click();
  await expect(page.getByTestId("voice-recovery-limit")).toContainText(
    msg("voice.recovery.limit"),
  );
  expect(
    (await commands(page)).filter((cmd) => sessionMutations.test(cmd)),
  ).toEqual([]);
});

test("a missing book leaves the native session controllable without substituting another book", async ({
  page,
}) => {
  await openLive(page);
  await leave(page);
  await page.evaluate(() => {
    const w = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args?: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    const original = w.__TAURI_INTERNALS__.invoke;
    w.__TAURI_INTERNALS__.invoke = (cmd, args) =>
      cmd === "list_books" ? Promise.resolve([]) : original(cmd, args);
  });
  await page.getByTestId("return-to-tutor").click();
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
  await expect(page.getByTestId("return-end-session")).toBeVisible();
  expect(
    (await commands(page)).filter((cmd) => cmd === "voice_session_start"),
  ).toEqual([]);
});

test("same-book Talk returns to the native cast binding and does not replace it with BookDirect", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await page.evaluate(() => {
    const audio = document.querySelector("audio");
    if (audio != null) {
      audio.pause();
      audio.currentTime = 0.5;
    }
  });
  await emitVoice(page, { kind: "lifecycle", state: "connected" });
  await emitVoice(page, {
    kind: "transcript",
    role: "user",
    text: "Keep the original cast",
  });
  await expect(page.getByTestId("reading-view")).toHaveAttribute(
    "data-job-id",
    "job-1",
  );
  await leave(page);
  await page.getByTestId("nav-library").click();
  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toHaveAttribute(
    "data-job-id",
    "job-1",
  );
  await expect(page.getByTestId("voice-session-transcript")).toContainText(
    "Keep the original cast",
  );
  expect(
    (await commands(page)).filter((cmd) => cmd === "voice_session_start"),
  ).toEqual([]);
  expect(
    await page.evaluate(() => document.querySelector("audio")?.paused),
  ).toBe(true);
});

test("returning during a pending start does not dial a second session", async ({
  page,
}) => {
  await openLive(page);
  await emitVoice(page, { kind: "ended", reason: "stopped" });
  await page.evaluate(() => {
    const w = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args?: Record<string, unknown>,
        ) => Promise<unknown>;
      };
      __releaseTutorStart?: () => void;
      __invokedCommands: string[];
    };
    const original = w.__TAURI_INTERNALS__.invoke;
    w.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
      if (cmd === "voice_session_start") {
        w.__invokedCommands.push(cmd);
        await new Promise<void>((resolve) => {
          w.__releaseTutorStart = resolve;
        });
        return null;
      }
      return original(cmd, args);
    };
  });
  await page.getByTestId("voice-session-start").click();
  await leave(page);
  await page.getByTestId("return-to-tutor").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  expect(
    (await commands(page)).filter((cmd) => cmd === "voice_session_start"),
  ).toHaveLength(1);
  await page.evaluate(() =>
    (
      window as unknown as { __releaseTutorStart?: () => void }
    ).__releaseTutorStart?.(),
  );
});

test("returning to the original cast restores its clock after another audio item was selected", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await page.evaluate(() => {
    const audio = document.querySelector("audio");
    if (audio != null) {
      audio.pause();
      audio.currentTime = 0.5;
      audio.dispatchEvent(new Event("timeupdate"));
    }
  });
  await emitVoice(page, { kind: "lifecycle", state: "connected" });
  await leave(page);
  await page.evaluate(async (moduleUrl) => {
    const store = (await import(
      moduleUrl
    )) as typeof import("../src/playbackStore");
    const item = store.getSnapshot().item;
    if (item == null) throw new Error("fixture audio missing");
    store.dispatch({
      type: "load",
      item: {
        ...item,
        id: "different-export",
        jobId: "different-job",
        path: "/exports/different.mp3",
      },
      autoplay: false,
    });
  }, "/src/playbackStore.ts");
  await page.getByTestId("return-to-tutor").click();
  await expect(page.getByTestId("reading-view")).toHaveAttribute(
    "data-job-id",
    "job-1",
  );
  await expect
    .poll(() =>
      page.evaluate(() =>
        Math.abs((document.querySelector("audio")?.currentTime ?? 0) - 0.5),
      ),
    )
    .toBeLessThan(0.1);
  expect(
    await page.evaluate(() => document.querySelector("audio")?.paused),
  ).toBe(true);
});

test("Return to Tutor preserves an explicit page hold until Go to tutor's page is chosen", async ({
  page,
}) => {
  await openLive(page);
  await page.evaluate(() => {
    const w = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args?: Record<string, unknown>,
        ) => Promise<unknown>;
      };
      __mockEmitEvent: (event: string, payload: unknown) => void;
    };
    const original = w.__TAURI_INTERNALS__.invoke;
    w.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
      if (cmd === "voice_session_navigation_ack" && args != null) {
        w.__mockEmitEvent("voice-session-event", {
          session_id: args.sessionId,
          book_id: "book-fixture",
          event: {
            kind: "navigation_settled",
            call_id: args.callId,
            tool: "jump_to_page",
            applied: args.disposition === "resume",
            held: args.disposition === "held",
          },
        });
      }
      return original(cmd, args);
    };
  });
  await page.getByTestId("tutor-keep-page").click();
  await emitVoice(page, {
    kind: "navigation_requested",
    tool: "jump_to_page",
    call_id: "held-before-leaving",
    generation: 0,
    pos_ms: 900,
    page_no: 3,
  });
  await expect(page.getByTestId("reading-return")).toBeVisible();
  await leave(page);
  await page.getByTestId("return-to-tutor").click();
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "2",
  );
  await page.getByTestId("reading-return").click();
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "3",
  );
});

test("Return restores this book's zoom and fit after browsing a different book", async ({
  page,
}) => {
  await openLive(page);
  await page.getByTestId("reading-zoom-in").click();
  await page.getByTestId("reading-fit-page").click();
  await expect(page.getByTestId("reading-page-zoom")).toHaveAttribute(
    "data-zoom",
    "1.1",
  );
  await leave(page);
  await page.getByTestId("nav-library").click();
  await page.getByTestId("talk-book-book-B").click();
  await page.getByTestId("reading-zoom-in").click();
  await page.getByTestId("reading-fit-width").click();
  await page.getByTestId("voice-session-elsewhere").getByRole("button").click();
  await expect(page.getByTestId("reading-page-zoom")).toHaveAttribute(
    "data-zoom",
    "1.1",
  );
  await expect(page.getByTestId("reading-fit-page")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
});
