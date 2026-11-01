import { expect, test } from "@playwright/test";

import { msg } from "./i18nText";
import { fixtureBook, installTauriMock } from "./tauriMock";

test("Settings lists what the tutor remembers and forgets one on request", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    learningPreferences: [
      { key: "pace", value: "slow down, one idea at a time", updatedAt: 2 },
      { key: "language", value: "اشرح بالعربي", updatedAt: 1 },
    ],
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();

  const memory = page.getByTestId("learner-memory");
  await expect(memory).toContainText(msg("settings.memoryLabel"));
  const entries = memory.getByTestId("learner-memory-entry");
  await expect(entries).toHaveCount(2);
  await expect(entries.nth(0)).toContainText(msg("settings.memoryTopicPace"));
  await expect(entries.nth(0)).toContainText("slow down, one idea at a time");
  await expect(entries.nth(1)).toContainText(
    msg("settings.memoryTopicLanguage"),
  );
  await expect(entries.nth(1)).toContainText("اشرح بالعربي");

  await entries.nth(0).getByTestId("learner-memory-forget").click();
  await expect(entries).toHaveCount(1);
  await expect(memory).not.toContainText("slow down, one idea at a time");

  await memory.getByTestId("learner-memory-forget").click();
  await expect(memory.getByTestId("learner-memory-empty")).toHaveText(
    msg("settings.memoryEmpty"),
  );
});

test("a preference saved under a built-in object name can be viewed and forgotten", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    learningPreferences: [
      {
        key: "constructor",
        value: "explain with examples first",
        updatedAt: 1,
      },
    ],
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  const entry = page.getByTestId("learner-memory-entry");
  await expect(entry).toContainText("constructor");
  await expect(entry).toContainText("explain with examples first");
  await entry.getByTestId("learner-memory-forget").click();
  await expect(entry).toHaveCount(0);
});

test("a late Forget answer never brings back a preference already forgotten", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    learningPreferences: [
      { key: "pace", value: "slow down", updatedAt: 2 },
      { key: "style", value: "examples first", updatedAt: 1 },
    ],
  });
  await page.addInitScript(() => {
    const w = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (
          command: string,
          args?: Record<string, unknown>,
        ) => Promise<unknown>;
      };
      __releaseForget: (() => void) | null;
    };
    const original = w.__TAURI_INTERNALS__.invoke;
    w.__releaseForget = null;
    w.__TAURI_INTERNALS__.invoke = async (command, args) => {
      const answer = await original(command, args);
      if (command === "forget_learning_preference" && args?.key === "pace") {
        return new Promise((resolve) => {
          w.__releaseForget = () => resolve(answer);
        });
      }
      return answer;
    };
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  const entries = page.getByTestId("learner-memory-entry");
  await expect(entries).toHaveCount(2);
  await entries
    .filter({ hasText: "slow down" })
    .getByTestId("learner-memory-forget")
    .click();
  await page.waitForFunction(
    () =>
      (window as unknown as { __releaseForget: unknown }).__releaseForget !=
      null,
  );
  await entries
    .filter({ hasText: "examples first" })
    .getByTestId("learner-memory-forget")
    .click();
  await expect(entries.filter({ hasText: "examples first" })).toHaveCount(0);
  const stored = await page.evaluate(async () => {
    const w = window as unknown as {
      __TAURI_INTERNALS__: { invoke: (command: string) => Promise<unknown> };
      __releaseForget: () => void;
    };
    w.__releaseForget();
    await new Promise<void>((resolve) =>
      requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
    );
    return w.__TAURI_INTERNALS__.invoke("list_learning_preferences");
  });
  expect(stored).toEqual([]);
  await expect(entries).toHaveCount(0);
});
