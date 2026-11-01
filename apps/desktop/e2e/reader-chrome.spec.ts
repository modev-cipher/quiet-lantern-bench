import { expect, test, type Page } from "@playwright/test";

import { msg } from "./i18nText";
import { wavDataUrl } from "./pcmFixture";
import { fixtureBook, installTauriMock } from "./tauriMock";
import { emitVoice } from "./voiceEnvelope";

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

async function openBook(page: Page) {
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
  await page.goto("/");
  await page.getByTestId("study-resume").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
}

async function contrastAgainstGround(
  page: Page,
  selector: string,
): Promise<number> {
  return page.evaluate((target) => {
    const channels = (color: string): [number, number, number, number] => {
      const match = /rgba?\(([^)]+)\)/.exec(color);
      if (match == null) return [0, 0, 0, 0];
      const parts = match[1].split(",").map((part) => Number(part.trim()));
      return [parts[0], parts[1], parts[2], parts[3] ?? 1];
    };
    const luminance = ([red, green, blue]: number[]) => {
      const linear = [red, green, blue].map((value) => {
        const unit = value / 255;
        return unit <= 0.03928
          ? unit / 12.92
          : Math.pow((unit + 0.055) / 1.055, 2.4);
      });
      return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
    };
    const element = document.querySelector<HTMLElement>(target);
    if (element == null) return 0;
    const foreground = channels(getComputedStyle(element).color);
    let ground: [number, number, number, number] = [255, 255, 255, 1];
    for (
      let node: HTMLElement | null = element;
      node != null;
      node = node.parentElement
    ) {
      const candidate = channels(getComputedStyle(node).backgroundColor);
      if (candidate[3] >= 0.99) {
        ground = candidate;
        break;
      }
    }
    const [lighter, darker] = [luminance(foreground), luminance(ground)].sort(
      (a, b) => b - a,
    );
    return (lighter + 0.05) / (darker + 0.05);
  }, selector);
}

for (const scheme of ["light", "dark"] as const) {
  test.describe(`${scheme} theme`, () => {
    test.use({ colorScheme: scheme });

    test("the Book's player controls are readable against the reading page", async ({
      page,
    }) => {
      await openBook(page);
      await expect(page.locator("html")).toHaveAttribute("data-theme", scheme);
      for (const control of [
        '[data-testid="player-time"]',
        '[data-testid="player-skip-back"]',
        '[data-testid="player-rate"]',
      ]) {
        expect(
          await contrastAgainstGround(page, control),
          `${control} against the ground it is drawn on`,
        ).toBeGreaterThanOrEqual(4.5);
      }
    });
  });
}

test("the sentence bar says what it measures, so it cannot be read as a second seek bar", async ({
  page,
}) => {
  await openBook(page);
  const label = page.getByTestId("reading-underline-label");
  await expect(label).toBeVisible();
  await expect(label).not.toBeEmpty();
  await expect(page.getByTestId("reading-underline")).toBeVisible();
});

test("the tutor header keeps each control on one line at the companion's width", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    bookAgentProbe: "notInBuild",
    voiceKeysScenario: {
      entries: [
        {
          baseUrl: "https://generativelanguage.googleapis.com",
          provider: "gemini",
          last4: "abcd",
          state: "active" as const,
          keyPresent: true,
          custom: false,
        },
      ],
    },
  });
  await page.setViewportSize({ width: 1100, height: 720 });
  await page.goto("/");
  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await emitVoice(page, { kind: "lifecycle", state: "connected" });
  await emitVoice(page, { kind: "mode", mode: "text", model: "text-model" });
  await expect(page.getByTestId("voice-session-stop")).toBeVisible();

  const toggle = page.getByTestId("voice-session-toggle");
  await expect(toggle).toHaveText("");
  await expect(toggle).toHaveAccessibleName(msg("voice.collapse"));
  const box = await toggle.boundingBox();
  expect(box?.height).toBe(box?.width);

  for (const control of ["voice-session-status", "voice-session-stop"]) {
    const lines = await page.getByTestId(control).evaluate((element) => {
      const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
      const tops: number[] = [];
      for (let node = walker.nextNode(); node != null; node = walker.nextNode()) {
        if ((node.textContent ?? "").trim() === "") continue;
        const range = document.createRange();
        range.selectNodeContents(node);
        for (const rect of range.getClientRects()) {
          if (rect.width > 0) tops.push(rect.top + rect.height / 2);
        }
      }
      tops.sort((a, b) => a - b);
      let count = tops.length > 0 ? 1 : 0;
      for (let at = 1; at < tops.length; at += 1) {
        if (tops[at] - tops[at - 1] > 6) count += 1;
      }
      return count;
    });
    expect(lines, `${control} wrapped onto more than one line`).toBe(1);
  }
});
