import { expect, test, type Page } from "@playwright/test";

import { msg } from "./i18nText";
import {
  fixtureBook,
  installTauriMock,
  type MockSessionRow,
} from "./tauriMock";
import { emitVoice } from "./voiceEnvelope";

// Field sessions 2026-10-09 (01a121d5, 01a121dd, 01a121e0): Arabic answers
// with English terms read left to right, a minute of waiting showed only an
// error, the user could not tell the writing tutor from the voice tutor or
// go back to typing, and Continue opened an empty chat. Locale-agnostic:
// testids and catalog text only, so the spec runs under
// I18N_E2E_LOCALE=ar as well.

const FIELD_REPLY = [
  "الكتاب بيغطي مفاهيم الـ Distributed Systems الأساسية بتسلسل تصاعدي.",
  "",
  "1. Introduction: بنتعرف فيه على تعريف الـ distributed system، والأهداف الجوهرية لبنائه زي الـ resource sharing والـ openness والـ scalability.",
].join("\n");

async function openTutor(
  page: Page,
  voiceSessionsScenario?: {
    rows: MockSessionRow[];
    turns?: Record<
      string,
      Array<{
        turn_idx: number;
        role: "user" | "assistant";
        text: string;
        created_at: number;
      }>
    >;
  },
): Promise<void> {
  await installTauriMock(page, {
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
    ...(voiceSessionsScenario != null ? { voiceSessionsScenario } : {}),
  });
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
}

async function goLive(page: Page): Promise<void> {
  await emitVoice(page, { kind: "lifecycle", state: "connected" });
  await emitVoice(page, {
    kind: "mode",
    mode: "text",
    model: "text-model",
  });
  await expect(page.getByTestId("voice-session-stop")).toBeVisible();
}

test("Arabic with English terms reads right to left; English reads left to right", async ({
  page,
}) => {
  await openTutor(page);
  await goLive(page);
  await emitVoice(page, {
    kind: "transcript",
    role: "user",
    text: "اشرحلي ال layout بتاعة الكتاب دا",
  });
  await emitVoice(page, { kind: "activity", state: "idle", detail: "" });
  await emitVoice(page, {
    kind: "transcript",
    role: "assistant",
    text: FIELD_REPLY,
  });
  await emitVoice(page, { kind: "activity", state: "idle", detail: "" });

  const transcript = page.getByTestId("voice-session-transcript");
  const blocks = transcript.locator("[data-dir]");
  await expect(blocks).toHaveCount(3);
  for (const block of await blocks.all()) {
    await expect(block).toHaveAttribute("dir", "rtl");
  }
  // The numbered item opens with "Introduction", yet its number sits on
  // the right, where an Arabic sentence starts.
  const item = blocks.nth(2);
  await expect(item).toContainText("Introduction");
  const marker = await item.locator("span").first().boundingBox();
  const body = await item.locator("span").nth(1).boundingBox();
  expect(marker != null && body != null).toBe(true);
  expect(marker!.x).toBeGreaterThan(body!.x);

  await emitVoice(page, {
    kind: "transcript",
    role: "user",
    text: "What does a layered architecture look like?",
  });
  const english = transcript.locator("li").last().locator("[data-dir]");
  await expect(english).toHaveAttribute("dir", "ltr");
});

test("the panel says what the tutor is doing while a reply is owed", async ({
  page,
}) => {
  await openTutor(page);
  await goLive(page);
  await expect(page.getByTestId("voice-session-mode")).toHaveAttribute(
    "data-mode",
    "text",
  );

  const activity = page.getByTestId("voice-session-activity");
  await emitVoice(page, { kind: "activity", state: "thinking", detail: "" });
  await expect(activity).toHaveAttribute("data-activity", "thinking");
  await expect(activity).toContainText(msg("voice.activity.thinking"));

  await emitVoice(page, { kind: "activity", state: "retrying", detail: "1" });
  // The label carries the attempt number inside a direction isolate (the
  // `{attempt|n}` island), so pin the state and the number, not the bytes.
  await expect(activity).toHaveAttribute("data-activity", "retrying");
  await expect(activity).toContainText("1");

  await emitVoice(page, {
    kind: "activity",
    state: "tool",
    detail: "jump_to_page",
  });
  await expect(activity).toContainText(msg("voice.activity.page"));

  // Review 2026-10-09, U1: the step names its page, and once it answered,
  // the strip says the reply is being prepared and lists the finished step.
  await emitVoice(page, {
    kind: "activity",
    state: "tool",
    detail: "jump_to_page",
    target: "23",
  });
  await expect(activity).toHaveAttribute("data-activity", "tool");
  await expect(
    page.locator(
      '[data-testid="voice-session-activity"] svg[data-glyph="page"]',
    ),
  ).toHaveCount(1);
  await emitVoice(page, {
    kind: "activity",
    state: "thinking",
    detail: "after_tool",
  });
  await expect(activity).toContainText(msg("voice.activity.prepare"));
  await expect(activity.getByTestId("tutor-steps")).toHaveCount(1);
  // The live region is one persistent element, announcing state changes.
  await expect(
    page.getByTestId("voice-session-activity-announcer"),
  ).toContainText(msg("voice.activity.prepare"));

  // The reply itself is the progress from here: the line gives way to the
  // streaming bubble and its caret.
  await emitVoice(page, {
    kind: "transcript",
    role: "assistant",
    text: "Here is the layered style, on page 23.",
  });
  await expect(activity).toHaveCount(0);
  await expect(page.getByTestId("tutor-caret")).toHaveCount(1);
  // The reply keeps the steps that led to it.
  await expect(
    page.getByTestId("voice-session-transcript").getByTestId("tutor-steps"),
  ).toHaveCount(1);
  await emitVoice(page, { kind: "activity", state: "idle", detail: "" });
  await expect(page.getByTestId("tutor-caret")).toHaveCount(0);
  await expect(page.getByTestId("voice-session-transcript")).toContainText(
    "Here is the layered style, on page 23.",
  );
});

test("Back to typing returns the voice tutor to the writing tutor", async ({
  page,
}) => {
  await openTutor(page);
  await goLive(page);
  const typeOption = page.getByTestId("voice-session-back-to-typing");
  const talkOption = page.getByTestId("voice-session-interactive");
  await expect(typeOption).toHaveAttribute("aria-checked", "true");
  await expect(talkOption).toHaveAttribute("aria-checked", "false");
  await expect(talkOption).toBeEnabled();

  await emitVoice(page, { kind: "mode", mode: "voice", model: "voice-model" });
  await emitVoice(page, { kind: "mic_state", state: "listening", detail: "" });
  const mode = page.getByTestId("voice-session-mode");
  await expect(mode).toHaveAttribute("data-mode", "voice");
  await expect(talkOption).toHaveAttribute("aria-checked", "true");
  await expect(talkOption).toHaveText(msg("voice.mode.talk"));
  await expect(page.getByTestId("voice-session-now")).toHaveAttribute(
    "data-now",
    "listen",
  );
  await expect(page.getByTestId("voice-session-mute")).toHaveAttribute(
    "aria-pressed",
    "false",
  );

  await typeOption.click();
  await expect
    .poll(() =>
      page.evaluate(() =>
        (
          window as unknown as { __invokedCommands: string[] }
        ).__invokedCommands.includes("voice_session_leave_interactive"),
      ),
    )
    .toBe(true);
  await emitVoice(page, { kind: "mic_state", state: "idle", detail: "" });
  await emitVoice(page, { kind: "mode", mode: "text", model: "text-model" });
  await expect(mode).toHaveAttribute("data-mode", "text");
  await expect(typeOption).toHaveAttribute("aria-checked", "true");
  await expect(typeOption).toHaveText(msg("voice.mode.type"));
  await expect(page.getByTestId("voice-session-mute")).toHaveCount(0);
  await expect(page.getByTestId("voice-session-now")).toHaveCount(0);
});

test("Continue shows the conversation it picks up", async ({ page }) => {
  const source: MockSessionRow = {
    id: "01920000-0000-7000-8000-000000000001",
    book_id: "book-fixture",
    job_id: null,
    started_at: 1_791_569_537_513,
    ended_at: 1_791_569_693_424,
    end_reason: "ended",
    duration_ms: 155_911,
    turn_count: 2,
    summary_excerpt: "transparency",
    position: { kind: "offset", value: 28_417 },
    job_resolves: false,
    provider_changed: false,
  };
  await openTutor(page, {
    rows: [source],
    turns: {
      [source.id]: [
        {
          turn_idx: 0,
          role: "user",
          text: "what is distribution transparency?",
          created_at: 1,
        },
        {
          turn_idx: 1,
          role: "assistant",
          text: "It hides that the system runs on many machines.",
          created_at: 2,
        },
      ],
    },
  });
  await page.getByTestId("session-history-continue").click();
  const earlier = page.getByTestId("voice-session-earlier");
  await expect(earlier).toContainText("what is distribution transparency?");
  await expect(earlier).toContainText("many machines");

  // The continued session's first events do not wipe it.
  await goLive(page);
  await expect(earlier).toContainText("many machines");
  await expect(page.getByTestId("voice-session-earlier-divider")).toHaveText(
    msg("voice.earlier.resumed"),
  );
  await emitVoice(page, { kind: "transcript", role: "user", text: "كمل" });
  await expect(page.getByTestId("voice-session-earlier-divider")).toHaveText(
    msg("voice.earlier.divider"),
  );
});
