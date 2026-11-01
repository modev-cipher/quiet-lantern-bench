import { expect, type ConsoleMessage, type Page } from "@playwright/test";

import type { BookSummary } from "../src/libraryModel";
import { wavDataUrl } from "./pcmFixture";
import {
  fixtureBook,
  fixtureChapters,
  installTauriMock,
  type ReadingScenario,
  type Scenario,
} from "./tauriMock";

export const EXPORT_ID = "export-job-1";

// The fixture card vanishing at boot is a rare flake (#181). When it happens,
// name the state of every resource the boot chain awaits — did the init
// script land, did React mount, which commands the app got to — so one
// failure log localises the break instead of "timed out waiting for testid".
async function bootPostmortem(
  page: Page,
  consoleLines: string[],
): Promise<string> {
  let state = "page unreachable for evaluation";
  try {
    state = await page.evaluate(() => {
      const w = window as unknown as Record<string, unknown>;
      return JSON.stringify({
        readyState: document.readyState,
        tauriInternals: "__TAURI_INTERNALS__" in window,
        rootChildren: document.getElementById("root")?.childElementCount ?? -1,
        bookCards: document.querySelectorAll('[data-testid="book-card"]')
          .length,
        invoked: (w.__invokedCommands as string[] | undefined) ?? "missing",
      });
    });
  } catch (cause) {
    state = `page unreachable for evaluation: ${String(cause)}`;
  }
  return [
    "create-audio-book-fixture never appeared after page.goto (#181 boot flake).",
    `boot state: ${state}`,
    `page said: ${consoleLines.length > 0 ? consoleLines.join(" | ") : "(nothing)"}`,
    "reading: tauriInternals=false means the init script never applied;",
    "rootChildren=0 means React never mounted (module load/vite);",
    "invoked missing get_onboarding_complete/list_books means App never asked;",
    "bookCards=0 with list_books invoked means the card render itself broke.",
  ].join("\n");
}

export async function openReading(
  page: Page,
  reading: ReadingScenario = {},
  extraJob: Record<string, unknown> = {},
  book: Partial<BookSummary> = {},
  extraScenario: Partial<Scenario> = {},
): Promise<void> {
  await installTauriMock(page, {
    books: [fixtureBook(book)],
    readingScenario: { mediaDataUrl: wavDataUrl(), ...reading },
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 10,
      outputPath: "/exports/fixture-book.mp3",
      ...extraJob,
    },
    ...extraScenario,
  });
  const consoleLines: string[] = [];
  const onConsole = (message: ConsoleMessage) => {
    if (message.type() === "error" || message.type() === "warning") {
      consoleLines.push(`console.${message.type()}: ${message.text()}`);
    }
  };
  const onPageError = (error: Error) => {
    consoleLines.push(`pageerror: ${error.message}`);
  };
  page.on("console", onConsole);
  page.on("pageerror", onPageError);
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  try {
    // The click carries its own budget: left to the 30s test timeout, the
    // postmortem would run against an already-aborted page and lose the
    // boot-state snapshot it exists to capture.
    await page
      .getByTestId("create-audio-book-fixture")
      .click({ timeout: 15_000 });
  } catch (cause) {
    const original = cause instanceof Error ? cause.message : String(cause);
    throw new Error(
      `${await bootPostmortem(page, consoleLines)}\noriginal: ${original}`,
    );
  } finally {
    page.off("console", onConsole);
    page.off("pageerror", onPageError);
  }
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-output-job-1")).toBeVisible({
    timeout: 15_000,
  });
  await page.getByTestId("nav-library").click();
  await page.getByTestId("nav-audio").click();
  await expect(page.getByTestId(`audio-card-${EXPORT_ID}`)).toBeVisible();
}

export async function play(page: Page): Promise<void> {
  await page.getByTestId(`cover-quick-toggle-${EXPORT_ID}`).click();
  await expect(page.getByTestId("mini-player")).toBeVisible();
}

export async function enterReading(page: Page): Promise<void> {
  await page.getByTestId("mini-player-follow-along").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
}

export async function audioState(page: Page): Promise<{
  currentTime: number;
  paused: boolean;
  duration: number;
}> {
  return page.evaluate(() => {
    const audio = document.querySelector("audio");
    return {
      currentTime: audio?.currentTime ?? Number.NaN,
      paused: audio?.paused ?? true,
      duration: audio?.duration ?? Number.NaN,
    };
  });
}

export async function waitForTime(page: Page, seconds: number): Promise<void> {
  await page.waitForFunction(
    (target) => {
      const audio = document.querySelector("audio");
      return (audio?.currentTime ?? 0) >= target;
    },
    seconds,
    { timeout: 20_000 },
  );
}
