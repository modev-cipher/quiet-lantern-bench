import { expect, test, type Page } from "@playwright/test";

import readingGolden from "./fixtures/reading-timeline.golden.json" with { type: "json" };
import { openReading, play, enterReading } from "./readingHarness";
import { emitVoice } from "./voiceEnvelope";

test.setTimeout(120_000);

test.skip(
  process.env.V2_READING_SHOTS !== "1",
  "reading-session capture rig — writes docs/design/v2-shots; set V2_READING_SHOTS=1 to regenerate deliberately",
);

const OUT = "../../docs/design/v2-shots";

function pageSvgDataUrl(): string {
  const lines = Array.from({ length: 26 }, (_v, i) => {
    const y = 120 + i * 22;
    const w = i % 6 === 5 ? 300 : 470;
    return `<rect x="71" y="${y}" width="${w}" height="9" rx="4" fill="#3b362d" opacity="0.55"/>`;
  }).join("");
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="612" height="792" viewBox="0 0 612 792">` +
    `<rect width="612" height="792" fill="#fbfaf8"/>` +
    `<rect x="71" y="56" width="330" height="18" rx="4" fill="#111111" opacity="0.8"/>` +
    lines +
    `<text x="306" y="756" font-family="serif" font-size="13" fill="#40403c" text-anchor="middle">7</text>` +
    `</svg>`;
  return `data:image/svg+xml;base64,${Buffer.from(svg).toString("base64")}`;
}

function goldenWithImages(): typeof readingGolden {
  const pageImage = pageSvgDataUrl();
  return {
    ...readingGolden,
    pages: readingGolden.pages.map((entry) => ({
      ...entry,
      imagePath: entry.imagePath ?? `<cache>/pa/page-${entry.pageNo}`,
      imageDataUrl: pageImage,
    })),
  } as typeof readingGolden;
}

async function dark(page: Page) {
  await page.evaluate(() => {
    document.documentElement.setAttribute("data-theme", "dark");
  });
  await page.waitForTimeout(250);
}

async function rtl(page: Page) {
  await page.evaluate(() => {
    document.documentElement.setAttribute("dir", "rtl");
  });
  await page.waitForTimeout(250);
}

async function shot(page: Page, name: string, width = 1440, height = 900) {
  await page.setViewportSize({ width, height });
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${OUT}/${name}.png` });
}

async function enterSession(page: Page) {
  await openReading(page, {}, {}, {}, { reading: goldenWithImages() });
  await play(page);
  await enterReading(page);
  await page.waitForTimeout(800);
  await dark(page);
}

async function emitSettled(page: Page, payload: Record<string, unknown>) {
  await emitVoice(page, payload);
  await page.waitForTimeout(150);
}

test("reading session surface — LTR, both modes, displacement proof", async ({
  page,
}) => {
  await enterSession(page);
  await shot(page, "reading-session-script-ltr-1440");
  await shot(page, "reading-session-script-ltr-800", 800, 720);
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.getByTestId("reading-mode-toggle").click();
  await page.waitForSelector('[data-testid="reading-facsimile-image"]');
  await page.waitForTimeout(1100);
  await shot(page, "reading-session-page-ltr-1440");
  await shot(page, "reading-session-page-ltr-800", 800, 720);
});

test("reading session surface — RTL, both modes", async ({ page }) => {
  await enterSession(page);
  await rtl(page);
  await shot(page, "reading-session-script-rtl-1440");
  await page.getByTestId("reading-mode-toggle").click();
  await page.waitForSelector('[data-testid="reading-facsimile-image"]');
  await page.waitForTimeout(1100);
  await shot(page, "reading-session-page-rtl-1440");
});

test("reading session surface — tutor live, calm identity", async ({
  page,
}) => {
  await enterSession(page);
  await emitSettled(page, { kind: "lifecycle", state: "connected" });
  await expect(page.getByTestId("voice-session-status")).toContainText("Live");
  await emitSettled(page, {
    kind: "audio_state",
    state: "speaking",
    detail: "device playback",
  });
  await emitSettled(page, {
    kind: "mic_state",
    state: "muted",
    detail: "half-duplex hold",
  });
  await emitSettled(page, {
    kind: "transcript",
    role: "user",
    text: "Why does the tide matter in this chapter?",
  });
  await emitSettled(page, {
    kind: "transcript",
    role: "assistant",
    text: "The harbour scene sets the mood before the tide turns.",
  });
  await expect(page.getByTestId("voice-session-status")).toContainText("Live");
  await shot(page, "reading-session-tutor-live-1440");
});
