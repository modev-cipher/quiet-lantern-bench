import { expect, test } from "@playwright/test";

import { msg } from "./i18nText";
import {
  fixtureBook,
  fixtureCatalogModels,
  fixtureChapters,
  fixtureHardware,
  installTauriMock,
} from "./tauriMock";

const reportJson = JSON.stringify({
  segment_count: 3,
  verify: {
    enabled: true,
    threshold: 0.15,
    counts: { ok: 2, failed: 1, missing: 0, unscoreable_ref: 0 },
    worst: [
      {
        segment_id: "seg-accessible",
        chapter_id: "ch-1",
        idx: 1,
        band: "failed",
        wer: 0.42,
        reference_words: 12,
      },
    ],
  },
});

test.beforeEach(async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 600 });
  await installTauriMock(page, {
    books: [fixtureBook()],
    castProviders: {
      scriptConnections: [],
      ttsVoices: Array.from({ length: 50 }, (_, index) => ({
        voiceProfileId: `cloud-voice-${index}`,
        name: `Voice ${String(index).padStart(2, "0")}`,
        voiceId: `voice-${index}`,
        connectionId: "cartesia-roster",
        connectionName: "Cartesia",
        provider: "cartesia_tts",
        model: "sonic-3.6",
        loopback: false,
      })),
    },
    modelScenario: {
      catalogModels: fixtureCatalogModels(),
      hardware: fixtureHardware(),
      installed: [{ modelId: "kokoro-82m", diskBytes: 355_000_000 }],
    },
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 10,
      reportJson,
      voiceProfiles: [
        {
          id: "voice-heart",
          name: "Heart",
          engine: "kokoro-82m",
          kind: "preset",
          presetVoiceId: "af_heart",
        },
        {
          id: "voice-bella",
          name: "Bella",
          engine: "kokoro-82m",
          kind: "preset",
          presetVoiceId: "af_bella",
        },
        {
          id: "voice-adam",
          name: "Adam",
          engine: "kokoro-82m",
          kind: "preset",
          presetVoiceId: "am_adam",
        },
      ],
      segments: [
        {
          id: "seg-earlier",
          chapterId: "ch-1",
          idx: 0,
          pageNo: 1,
          kind: "text",
          text: "An earlier correction.",
          flags: [],
          hasAudio: true,
          durationMs: 900,
          edited: true,
          wer: null,
          alignSource: "whisperx",
        },
        {
          id: "seg-accessible",
          chapterId: "ch-1",
          idx: 1,
          pageNo: 1,
          kind: "text",
          text: "The lyin sleps tonite.",
          flags: ["verify_failed"],
          hasAudio: true,
          durationMs: 900,
          edited: false,
          wer: null,
          alignSource: "whisperx",
        },
      ],
    },
  });
  await page.goto("/");
  await page.getByTestId("create-audio-book-fixture").click();
  await page.getByTestId("cast-step-sound").click();
});

test("the visible voice selector supports navigation, cancellation, type-ahead and Tab", async ({
  page,
}) => {
  const field = page.getByRole("combobox", {
    name: msg("cast.voice"),
    exact: true,
  });
  await field.focus();
  await field.press("ArrowDown");
  await expect(field).toHaveAttribute("aria-expanded", "true");
  await field.press("Home");
  await field.press("ArrowDown");
  const activeId = await field.getAttribute("aria-activedescendant");
  await expect(page.locator(`[id="${activeId}"]`)).toContainText("Bella");
  await field.press("Enter");
  await expect(field).toContainText("Bella");
  await field.press("Space");
  await expect(field).toHaveAttribute("aria-expanded", "true");
  await field.press("End");
  await field.press("Escape");
  await expect(field).toContainText("Bella");
  await expect(page.getByTestId("job-modal")).toBeVisible();
  await field.press("h");
  await field.press("Tab");
  await expect(field).toContainText("Heart");
  await expect(field).toHaveAttribute("aria-expanded", "false");
  await expect(page.getByTestId("voice-preview")).toBeFocused();
});

test("sentence editing has a translated name, receives focus and returns focus on cancel", async ({
  page,
}) => {
  await page.getByTestId("verify-toggle").check();
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-output-job-1")).toBeVisible({
    timeout: 15000,
  });
  await page.getByTestId("review-toggle-job-1").click();
  const edit = page.getByTestId("review-edit-seg-accessible");
  await edit.click();
  const field = page.getByRole("textbox", {
    name: msg("review.editSentence", { index: 2 }),
    exact: true,
  });
  await expect(field).toBeFocused();
  await expect(field).toHaveValue("The lyin sleps tonite.");
  await field.fill("The lion sleeps tonight.");
  await field.press("Escape");
  await expect(field).toHaveCount(0);
  await expect(edit).toBeFocused();
  await expect(page.getByTestId("review-text-seg-accessible")).toHaveText(
    "The lyin sleps tonite.",
  );
});

test("a long voice roster keeps the active option visible and commits its identity", async ({
  page,
}) => {
  await page.getByTestId("voice-engine-cloud").click();
  const field = page.getByRole("combobox", {
    name: msg("cast.voice"),
    exact: true,
  });
  await field.press("End");
  const activeId = await field.getAttribute("aria-activedescendant");
  const active = page.locator(`[id="${activeId}"]`);
  await expect(active).toContainText("Voice 49");
  await expect(active).toBeInViewport({ ratio: 1 });
  const menu = page.getByTestId("cast-cascade-model-tts-menu");
  const optionBox = (await active.boundingBox())!;
  const menuBox = (await menu.boundingBox())!;
  expect(optionBox.y).toBeGreaterThanOrEqual(menuBox.y - 1);
  expect(optionBox.y + optionBox.height).toBeLessThanOrEqual(
    menuBox.y + menuBox.height + 1,
  );
  await field.press("Enter");
  await expect(field).toContainText("Voice 49");
  await expect(page.getByTestId("cast-cascade-model-tts")).toHaveValue(
    "cloud-voice-49",
  );
});

test("submitting a sentence protects it and other drafts until the cast is requeued", async ({
  page,
}) => {
  await page.getByTestId("verify-toggle").check();
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-output-job-1")).toBeVisible({
    timeout: 15000,
  });
  await page.getByTestId("review-toggle-job-1").click();
  await page.evaluate(() => {
    const runtime = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (
          command: string,
          args?: unknown,
          options?: unknown,
        ) => Promise<unknown>;
      };
      __releaseReview?: () => void;
    };
    const original = runtime.__TAURI_INTERNALS__.invoke;
    runtime.__TAURI_INTERNALS__.invoke = async (command, args, options) => {
      if (command === "resynthesize_segment")
        await new Promise<void>((resolve) => {
          runtime.__releaseReview = resolve;
        });
      return original(command, args, options);
    };
  });
  await page.getByTestId("review-edit-seg-accessible").click();
  const input = page.getByTestId("review-input-seg-accessible");
  const submitted = "The lion sleeps tonight.";
  await input.fill(submitted);
  await expect(input).toHaveAccessibleDescription(msg("review.singleEditHint"));
  await expect(page.getByTestId("review-revert-seg-earlier")).toBeDisabled();
  await page.getByTestId("review-save-seg-accessible").click();
  await expect(input).toHaveJSProperty("readOnly", true);
  await expect(page.getByTestId("review-cancel-seg-accessible")).toBeDisabled();
  await expect(page.getByTestId("review-edit-seg-earlier")).toBeDisabled();
  await input.focus();
  await page.keyboard.press("Escape");
  await expect(input).toBeVisible();
  await page.keyboard.type("Unsubmitted text");
  await expect(input).toHaveValue(submitted);
  await page.evaluate(() => {
    const release = (window as unknown as { __releaseReview?: () => void })
      .__releaseReview;
    if (!release) throw new Error("review acknowledgment was not pending");
    release();
  });
  await expect(input).toHaveCount(0);
  const toggle = page.getByTestId("review-toggle-job-1");
  if ((await toggle.getAttribute("aria-expanded")) !== "true")
    await toggle.click();
  await expect(page.getByTestId("review-text-seg-accessible")).toHaveText(
    submitted,
  );
});
