import { expect, test, type Page } from "@playwright/test";

import { e2eLocale, msg } from "./i18nText";
import {
  fixtureBook,
  installTauriMock,
  type MockSessionRow,
  type VoiceSessionsScenario,
} from "./tauriMock";
import { emitVoice } from "./voiceEnvelope";

const BARS = [
  40, 90, 160, 220, 255, 230, 180, 120, 150, 210, 240, 200, 140, 90, 120, 170,
  130, 80, 50, 30,
];
const PEEK_TEXT =
  e2eLocale === "ar"
    ? "النظام الموزّع مجموعة من عناصر الحوسبة المستقلة"
    : "A distributed system is a collection of autonomous computing elements";

async function openTutor(
  page: Page,
  voiceSessionsScenario?: VoiceSessionsScenario,
): Promise<void> {
  await installTauriMock(page, {
    ...(voiceSessionsScenario != null ? { voiceSessionsScenario } : {}),
    books: [fixtureBook()],
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
    pagePeeks: {
      12: {
        pageNo: 12,
        chapterTitle: e2eLocale === "ar" ? "مقدمة" : "Introduction",
        text: PEEK_TEXT,
        clipped: true,
      },
    },
  });
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
}

async function openLive(page: Page): Promise<void> {
  await openTutor(page);
  await emitVoice(page, { kind: "lifecycle", state: "connected" });
  await emitVoice(page, { kind: "mode", mode: "text", model: "text-model" });
  await expect(page.getByTestId("voice-session-stop")).toBeVisible();
}

async function invoked(page: Page, command: string): Promise<boolean> {
  return page.evaluate(
    (name) =>
      (
        window as unknown as { __invokedCommands: string[] }
      ).__invokedCommands.includes(name),
    command,
  );
}

async function replyAfterSteps(
  page: Page,
  steps: Array<{ detail: string; target: string }>,
): Promise<void> {
  for (const step of steps) {
    await emitVoice(page, { kind: "activity", state: "tool", ...step });
  }
  await emitVoice(page, {
    kind: "activity",
    state: "thinking",
    detail: "after_tool",
  });
  await emitVoice(page, {
    kind: "transcript",
    role: "assistant",
    text: "Here it is.",
  });
  await emitVoice(page, { kind: "activity", state: "idle", detail: "" });
}

test("a spoken turn shows the shape of the speech; a typed turn shows none", async ({
  page,
}) => {
  await openLive(page);
  const transcript = page.getByTestId("voice-session-transcript");

  await page.getByTestId("voice-session-input").fill("typed question");
  await page.getByTestId("voice-session-send").click();
  await expect
    .poll(() => invoked(page, "voice_session_send_text"))
    .toBe(true);
  await emitVoice(page, {
    kind: "transcript",
    role: "user",
    text: "typed question",
  });
  await expect(transcript).toContainText("typed question");
  await emitVoice(page, { kind: "voiceprint", bars: BARS });
  await expect(transcript.getByTestId("voice-transcript-approx")).toHaveCount(0);
  await expect(transcript.getByTestId("tutor-voiceprint")).toHaveCount(0);
  await emitVoice(page, {
    kind: "transcript",
    role: "assistant",
    text: "A written answer.",
  });

  await emitVoice(page, {
    kind: "transcript",
    role: "user",
    text: "spoken question",
    spanFinal: true,
  });
  const approx = transcript.getByTestId("voice-transcript-approx");
  await expect(approx).toHaveCount(1);
  await expect(approx).toContainText(msg("voice.spoken"));
  await expect(transcript.getByTestId("tutor-voiceprint")).toHaveCount(0);

  await emitVoice(page, { kind: "voiceprint", bars: BARS });
  const print = transcript.getByTestId("tutor-voiceprint");
  await expect(print).toHaveCount(1);
  await expect(print.locator("b")).toHaveCount(20);
  await expect(approx).toContainText(msg("voice.spoken"));

  await emitVoice(page, { kind: "voiceprint", bars: [1, 2, 3] });
  await expect(print.locator("b")).toHaveCount(20);
});

test("a page the tutor read opens as a peek of that page's text", async ({
  page,
}) => {
  await openLive(page);
  await replyAfterSteps(page, [
    { detail: "jump_to_page", target: "12" },
    { detail: "look_at_page", target: "12" },
  ]);
  const pills = page.getByTestId("tutor-page-pill");
  await expect(pills).toHaveCount(1);
  await expect(pills.first()).toHaveText(msg("voice.peek.pill", { page: 12 }));
  await expect(pills.first()).toHaveAttribute("aria-expanded", "false");

  await pills.first().click();
  const peek = page.getByTestId("tutor-page-peek");
  await expect(peek).toContainText(PEEK_TEXT);
  await expect(peek).toContainText(msg("voice.peek.page", { page: 12 }));
  await expect(pills.first()).toHaveAttribute("aria-expanded", "true");
  const controls = await pills.first().getAttribute("aria-controls");
  await expect(peek).toHaveAttribute("id", controls ?? "missing");
  const calls = await page.evaluate(
    () =>
      (window as unknown as { __pagePeekCalls: Array<Record<string, unknown>> })
        .__pagePeekCalls,
  );
  expect(calls).toEqual([
    expect.objectContaining({ bookId: "book-fixture", pageNo: 12 }),
  ]);

  await page.keyboard.press("Escape");
  await expect(peek).toHaveCount(0);
  await expect(pills.first()).toHaveAttribute("aria-expanded", "false");
});

test("a failed jump earns no pill and an unreadable page says so", async ({
  page,
}) => {
  await openLive(page);
  await emitVoice(page, {
    kind: "activity",
    state: "tool",
    detail: "jump_to_page",
    target: "99",
  });
  await emitVoice(page, {
    kind: "failed",
    code: "navigation_out_of_range",
    message: "no such page",
  });
  await replyAfterSteps(page, [{ detail: "look_at_page", target: "40" }]);
  const pills = page.getByTestId("tutor-page-pill");
  await expect(pills).toHaveCount(1);
  await expect(pills).toHaveText(msg("voice.peek.pill", { page: 40 }));
  await pills.click();
  await expect(page.getByTestId("tutor-page-peek")).toContainText(
    msg("voice.peek.empty"),
  );
});

test("the mic toggle reports its pressed state and the now-line follows", async ({
  page,
}) => {
  await openLive(page);
  await emitVoice(page, { kind: "mode", mode: "voice", model: "voice-model" });
  await emitVoice(page, { kind: "mic_state", state: "listening", detail: "" });
  const mute = page.getByTestId("voice-session-mute");
  const now = page.getByTestId("voice-session-now");
  await expect(mute).toHaveAttribute("aria-pressed", "false");
  await expect(mute).toHaveAccessibleName(msg("voice.mute"));
  await expect(now).toHaveAttribute("data-now", "listen");
  await expect(now).toContainText(msg("voice.now.listening"));

  await mute.click();
  await expect.poll(() => invoked(page, "voice_session_mute")).toBe(true);
  await emitVoice(page, { kind: "mic_state", state: "muted", detail: "" });
  await expect(mute).toHaveAttribute("aria-pressed", "true");
  await expect(now).toHaveAttribute("data-now", "rest");
  await expect(now).toContainText(msg("voice.now.muted"));

  await emitVoice(page, { kind: "audio_state", state: "speaking", detail: "" });
  await expect(now).toHaveAttribute("data-now", "speak");
  await expect(page.getByTestId("voice-session-interrupt")).toBeVisible();
});

test("the Type/Talk switch answers the arrow key that points at Talk", async ({
  page,
}) => {
  await openLive(page);
  const typeOption = page.getByTestId("voice-session-back-to-typing");
  await typeOption.focus();
  await page.keyboard.press(e2eLocale === "ar" ? "ArrowLeft" : "ArrowRight");
  await expect
    .poll(() => invoked(page, "voice_session_enter_interactive"))
    .toBe(true);
  await expect(page.getByTestId("voice-session-interactive")).toBeFocused();
});

test("collapsing the transcript keeps Interrupt and the mic within reach", async ({
  page,
}) => {
  await openLive(page);
  await emitVoice(page, { kind: "mode", mode: "voice", model: "voice-model" });
  await emitVoice(page, { kind: "mic_state", state: "listening", detail: "" });
  await emitVoice(page, {
    kind: "transcript",
    role: "user",
    text: "spoken question",
    spanFinal: true,
  });
  await emitVoice(page, { kind: "audio_state", state: "speaking", detail: "" });
  const toggle = page.getByTestId("voice-session-toggle");
  await expect(toggle).toHaveAttribute("aria-expanded", "true");

  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await expect(page.getByTestId("voice-session-transcript")).toHaveCount(0);
  const interrupt = page.getByTestId("voice-session-interrupt");
  await expect(interrupt).toBeVisible();
  await interrupt.click();
  await expect.poll(() => invoked(page, "voice_session_interrupt")).toBe(true);
  const mute = page.getByTestId("voice-session-mute");
  await expect(mute).toBeVisible();
  await mute.click();
  await expect.poll(() => invoked(page, "voice_session_mute")).toBe(true);
  await expect(page.getByTestId("voice-session-input")).toBeVisible();
});

test("a session opened from History shows each spoken turn's stored voiceprint", async ({
  page,
}) => {
  const row: MockSessionRow = {
    id: "01920000-0000-7000-8000-000000000002",
    book_id: "book-fixture",
    job_id: null,
    started_at: 1_791_569_537_513,
    ended_at: 1_791_569_693_424,
    end_reason: "ended",
    duration_ms: 155_911,
    turn_count: 3,
    summary_excerpt: "transparency",
    position: { kind: "offset", value: 28_417 },
    job_resolves: false,
    provider_changed: false,
  };
  await openTutor(page, {
    rows: [row],
    turns: {
      [row.id]: [
        { turn_idx: 0, role: "user", text: "spoken first", created_at: 1, voiceprint: BARS },
        { turn_idx: 1, role: "assistant", text: "a reply", created_at: 2, voiceprint: BARS },
        { turn_idx: 2, role: "user", text: "malformed print", created_at: 3, voiceprint: [1, 2, 300] },
      ],
    },
  });
  await page.getByTestId(`session-history-row-${row.id}`).click();
  const view = page.getByTestId("session-view");
  await expect(view).toBeVisible();
  await expect(view).toContainText("malformed print");
  const prints = view.getByTestId("tutor-voiceprint");
  await expect(prints).toHaveCount(1);
  await expect(prints.locator("b")).toHaveCount(20);
  const turns = view.getByRole("log").locator("li");
  await expect(turns.nth(0).getByTestId("tutor-voiceprint")).toHaveCount(1);
  await expect(turns.nth(0)).toContainText(msg("voice.spoken"));
  await expect(turns.nth(1).getByTestId("tutor-voiceprint")).toHaveCount(0);
  await expect(turns.nth(2).getByTestId("tutor-voiceprint")).toHaveCount(0);
});

test("a page the tutor could not see earns no pill and its step says so", async ({
  page,
}) => {
  await openLive(page);
  await emitVoice(page, {
    kind: "activity",
    state: "tool",
    detail: "look_at_page",
    target: "12",
  });
  await emitVoice(page, { kind: "step_failed", tool: "look_at_page" });
  await replyAfterSteps(page, []);
  const transcript = page.getByTestId("voice-session-transcript");
  await expect(transcript).toContainText("Here it is.");
  await expect(page.getByTestId("tutor-page-pill")).toHaveCount(0);
  await expect(
    transcript.locator('[data-testid="tutor-steps"] [data-failed="true"]'),
  ).toHaveCount(1);
  await expect(page.getByTestId("voice-session-notice")).toHaveCount(0);
});
