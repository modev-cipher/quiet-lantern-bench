import { test, type Page } from "@playwright/test";

import readingGolden from "./fixtures/reading-timeline.golden.json" with { type: "json" };
import { wavDataUrl } from "./pcmFixture";
import {
  advanceLanSend,
  fixtureBook,
  fixtureCatalogModels,
  fixtureChapters,
  fixtureHardware,
  installTauriMock,
} from "./tauriMock";
import { emitVoice as emitVoiceEnvelope } from "./voiceEnvelope";

const MODELS = {
  catalogModels: fixtureCatalogModels(),
  hardware: fixtureHardware(),
  downloadTicks: 2,
  downloadDelayMs: 60,
};

test.setTimeout(120_000);

test.skip(
  process.env.GALLERY_SHOTS !== "1",
  "gallery capture rig — writes docs/design/gallery; set GALLERY_SHOTS=1 to regenerate deliberately",
);

const OUT = "../../docs/design/gallery";
const WIDTHS = [
  { name: "1440", width: 1440, height: 900 },
  { name: "1100", width: 1100, height: 720 },
  { name: "900", width: 900, height: 600 },
];

async function settle(page: Page) {
  await page.waitForTimeout(900);
}

async function shot(page: Page, name: string, widths = WIDTHS) {
  for (const w of widths) {
    await page.setViewportSize({ width: w.width, height: w.height });
    await page.waitForTimeout(350);
    await page.screenshot({ path: `${OUT}/${name}-${w.name}.png` });
  }
  await page.setViewportSize({ width: 1440, height: 900 });
}

async function dark(page: Page) {
  await page.evaluate(() => {
    document.documentElement.setAttribute("data-theme", "dark");
  });
  await page.waitForTimeout(250);
}

async function light(page: Page) {
  await page.evaluate(() => {
    document.documentElement.setAttribute("data-theme", "light");
  });
  await page.waitForTimeout(250);
}

const BOOKS = [
  fixtureBook(),
  fixtureBook({
    id: "b2",
    title: "A Second, Much Longer Book Title That Wraps",
    author: "Another Author",
    pageCount: 340,
    chapterCount: 18,
    createdAt: 2,
  }),
  fixtureBook({
    id: "b3",
    title: "Arabic Grammar Primer",
    author: "M. Haroun",
    pageCount: 96,
    chapterCount: 7,
    createdAt: 3,
    isScanned: true,
  }),
];

const EXPORTS = [
  {
    id: "e1",
    bookId: "book-fixture",
    jobId: "j1",
    path: "/exports/Fixture Book - full - narrate.mp3",
    bytes: 5_000_000,
    format: "mp3",
    mode: "narrate",
    stale: false,
    createdAt: 1_755_000_000_000,
  },
  {
    id: "e2",
    bookId: "b2",
    jobId: "j2",
    path: "/exports/Second Book - ch1-4 - narrate.mp3",
    bytes: 22_000_000,
    format: "mp3",
    mode: "narrate",
    stale: true,
    createdAt: 1_755_100_000_000,
  },
];

test("shell and library gallery", async ({ page }) => {
  await installTauriMock(page, { books: BOOKS, seededExports: EXPORTS });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.waitForSelector('[data-testid="book-card"]');
  await settle(page);
  await shot(page, "library-books");
  await dark(page);
  await shot(page, "library-books-dark", [WIDTHS[0]]);
  await light(page);
  await page.getByTestId("nav-audio").click();
  await settle(page);
  await shot(page, "library-audio");
  await dark(page);
  await shot(page, "library-audio-dark", [WIDTHS[0]]);
});

test("shelf home gallery", async ({ page }) => {
  await installTauriMock(page, { books: BOOKS, seededExports: EXPORTS });
  await page.addInitScript(() => {
    localStorage.setItem("txtcaster.listened", JSON.stringify({ e1: 0.34 }));
    localStorage.setItem("txtcaster.listened.recent", JSON.stringify(["e1"]));
  });
  await page.goto("/");
  await page.waitForSelector('[data-testid="shelf-continue"]');
  await settle(page);
  await shot(page, "shelf-home");
  await dark(page);
  await shot(page, "shelf-home-dark", [WIDTHS[0]]);
  await light(page);
});

test("empty states gallery", async ({ page }) => {
  await installTauriMock(page, { books: [] });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.waitForSelector("text=Your library is empty.");
  await settle(page);
  await shot(page, "library-empty");
  await dark(page);
  await shot(page, "library-empty-dark", [WIDTHS[0]]);
  await light(page);
  await page.getByTestId("nav-audio").click();
  await settle(page);
  await shot(page, "library-audio-empty", [WIDTHS[0]]);
});

test("wizard gallery", async ({ page }) => {
  await installTauriMock(page, {
    books: [],
    onboardingComplete: false,
    modelScenario: MODELS,
  });
  await page.goto("/");
  await page.waitForSelector('[data-testid="wizard-scan-card"]');
  await settle(page);
  await shot(page, "wizard-machine");
  await dark(page);
  await shot(page, "wizard-machine-dark", [WIDTHS[0]]);
  await light(page);
  await page.getByTestId("wizard-next").click();
  await page.waitForTimeout(400);
  await shot(page, "wizard-voice", [WIDTHS[0], WIDTHS[2]]);
  await page.getByTestId("wizard-teacher").locator("summary").click();
  await page.waitForTimeout(400);
  await shot(page, "wizard-brain", [WIDTHS[0]]);
});

test("cast modal and jobs gallery", async ({ page }) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 1200,
      outputPath: "/exports/fixture-book.mp3",
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.waitForSelector('[data-testid="job-modal"]');
  await settle(page);
  await shot(page, "cast-modal");
  await dark(page);
  await shot(page, "cast-modal-dark", [WIDTHS[0]]);
  await light(page);
  await page.getByTestId("start-job").click();
  await page.waitForTimeout(900);
  await shot(page, "jobs-running", [WIDTHS[0], WIDTHS[2]]);
  await page.waitForSelector('[data-testid="job-output-job-1"]', {
    timeout: 40_000,
  });
  await settle(page);
  await shot(page, "jobs-done", [WIDTHS[0]]);
  await dark(page);
  await shot(page, "jobs-done-dark", [WIDTHS[0]]);
});

test("jobs empty gallery", async ({ page }) => {
  await installTauriMock(page, { books: [] });
  await page.goto("/");
  await page.getByTestId("nav-jobs").click();
  await page.waitForTimeout(400);
  await shot(page, "activity-tray", [WIDTHS[0]]);
  await dark(page);
  await shot(page, "activity-tray-dark", [WIDTHS[0]]);
  await light(page);
  await page.getByTestId("open-production").click();
  await page.waitForTimeout(700);
  await shot(page, "jobs-empty", [WIDTHS[0]]);
});

test("models and settings gallery", async ({ page }) => {
  await installTauriMock(page, { books: [], modelScenario: MODELS });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-models").click();
  await settle(page);
  await shot(page, "models");
  await dark(page);
  await shot(page, "models-dark", [WIDTHS[0]]);
  await light(page);
  await page.getByTestId("nav-settings").click();
  await settle(page);
  await shot(page, "settings");
  await dark(page);
  await shot(page, "settings-dark", [WIDTHS[0]]);
});

test("send to phone gallery", async ({ page }) => {
  await installTauriMock(page, {
    books: BOOKS,
    seededExports: EXPORTS,
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("nav-audio").click();
  await settle(page);
  await page.waitForTimeout(600);
  await page.getByTestId("send-to-phone").click();
  await page.waitForSelector('[data-testid="send-to-phone-modal"]');
  await page.waitForTimeout(500);
  await shot(page, "send-to-phone", [WIDTHS[0], WIDTHS[2]]);
  await dark(page);
  await shot(page, "send-to-phone-dark", [WIDTHS[0]]);
  await light(page);
  await page.getByRole("button", { name: "Show QR code" }).click();
  await page.waitForSelector('[data-testid="lan-send-qr"]');
  await page.waitForTimeout(400);
  await shot(page, "send-to-phone-qr", [WIDTHS[0], WIDTHS[2]]);
  await dark(page);
  await shot(page, "send-to-phone-qr-dark", [WIDTHS[0]]);
  await light(page);
  await advanceLanSend(page, "sending");
  await page.waitForTimeout(500);
  await shot(page, "send-to-phone-sending", [WIDTHS[0]]);
  await advanceLanSend(page, "done");
  await page.waitForTimeout(500);
  await shot(page, "send-to-phone-done", [WIDTHS[0]]);
});

test("reduced motion evidence", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await installTauriMock(page, { books: BOOKS, seededExports: EXPORTS });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.waitForSelector('[data-testid="book-card"]');
  await page.waitForTimeout(400);
  await shot(page, "library-reduced-motion", [WIDTHS[0]]);
});

async function emitVoice(page: Page, payload: Record<string, unknown>) {
  await emitVoiceEnvelope(page, payload);
  await page.waitForTimeout(150);
}

test("playback and reading gallery", async ({ page }) => {
  const { openReading, play, enterReading } = await import("./readingHarness");
  await openReading(page);
  await play(page);
  await page.waitForTimeout(700);
  await shot(page, "mini-player");
  await dark(page);
  await shot(page, "mini-player-dark", [WIDTHS[0]]);
  await light(page);
  await enterReading(page);
  await page.waitForTimeout(800);
  await shot(page, "reading-script");
  await dark(page);
  await shot(page, "reading-script-dark", [WIDTHS[0]]);
  await light(page);
});

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
    `<rect x="71" y="472" width="470" height="150" rx="6" fill="none" stroke="#c9661f" stroke-width="2" opacity="0.7"/>` +
    `<rect x="96" y="500" width="180" height="94" rx="4" fill="#e9b184" opacity="0.5"/>` +
    `<rect x="300" y="500" width="215" height="10" rx="4" fill="#3b362d" opacity="0.5"/>` +
    `<rect x="300" y="522" width="190" height="10" rx="4" fill="#3b362d" opacity="0.45"/>` +
    `<text x="306" y="756" font-family="serif" font-size="13" fill="#40403c" text-anchor="middle">7</text>` +
    `</svg>`;
  return `data:image/svg+xml;base64,${Buffer.from(svg).toString("base64")}`;
}

test("reading page mode gallery", async ({ page }) => {
  const pageImage = pageSvgDataUrl();
  await installTauriMock(page, {
    books: [fixtureBook()],
    reading: {
      ...readingGolden,
      pages: readingGolden.pages.map((entry) => ({
        ...entry,
        imagePath: entry.imagePath ?? `<cache>/pa/page-${entry.pageNo}`,
        imageDataUrl: pageImage,
      })),
    } as typeof readingGolden,
    readingScenario: { mediaDataUrl: wavDataUrl() },
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 10,
      outputPath: "/exports/fixture-book.mp3",
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.getByTestId("start-job").click();
  await page.waitForSelector('[data-testid="job-output-job-1"]', {
    timeout: 15_000,
  });
  await page.getByTestId("nav-library").click();
  await page.getByTestId("nav-audio").click();
  await page.getByTestId("cover-quick-toggle-export-job-1").click();
  await page.waitForSelector('[data-testid="mini-player"]');
  await page.getByTestId("mini-player-follow-along").click();
  await page.waitForSelector('[data-testid="reading-view"]');
  await page.waitForTimeout(800);
  await page.getByTestId("reading-mode-toggle").click();
  await page.waitForTimeout(1100);
  await shot(page, "reading-page-mode", [WIDTHS[0], WIDTHS[2]]);
  await dark(page);
  await shot(page, "reading-page-mode-dark", [WIDTHS[0]]);
});

test("voice tutor states gallery", async ({ page }) => {
  const { openReading, play } = await import("./readingHarness");
  await openReading(page);
  await play(page);
  await page.getByTestId("mini-player-follow-along").click();
  await page.waitForSelector('[data-testid="reading-view"]');
  await page.waitForTimeout(700);
  await page.getByTestId("voice-session-toggle").click();
  await page.waitForTimeout(300);
  await shot(page, "voice-tutor-off", [WIDTHS[0]]);
  await emitVoice(page, { kind: "lifecycle", state: "connected" });
  await emitVoice(page, {
    kind: "audio_state",
    state: "speaking",
    detail: "device playback",
  });
  await emitVoice(page, {
    kind: "mic_state",
    state: "muted",
    detail: "half-duplex hold",
  });
  await emitVoice(page, {
    kind: "transcript",
    role: "assistant",
    text: "The harbour scene sets the mood before the tide turns.",
  });
  await shot(page, "voice-tutor-live-muted", [WIDTHS[0]]);
  await dark(page);
  await shot(page, "voice-tutor-live-muted-dark", [WIDTHS[0]]);
  await light(page);
  await emitVoice(page, {
    kind: "audio_state",
    state: "active",
    detail: "idle between replies",
  });
  await emitVoice(page, {
    kind: "mic_state",
    state: "listening",
    detail: "push to talk",
  });
  await shot(page, "voice-tutor-listening", [WIDTHS[0]]);
  await emitVoice(page, { kind: "ended", reason: "provider closed" });
  await shot(page, "voice-tutor-ended", [WIDTHS[0]]);
});

test("tutor suppression sequence gallery", async ({ page }) => {
  const { openReading, play } = await import("./readingHarness");
  await openReading(page);
  await play(page);
  await page.getByTestId("mini-player-follow-along").click();
  await page.waitForSelector('[data-testid="reading-view"]');
  await page.waitForSelector("[data-word].bg-reading-highlight", {
    timeout: 10_000,
  });
  await page.evaluate(async () => {
    const specifier = ["", "src", "playbackStore.ts"].join("/");
    const store = (await import(specifier)) as unknown as {
      dispatch: (action: unknown) => void;
    };
    store.dispatch({ type: "pause" });
  });
  await dark(page);
  await shot(page, "tutor-suppression-before", [WIDTHS[0]]);
  await emitVoice(page, { kind: "lifecycle", state: "connected" });
  await page.waitForTimeout(400);
  await shot(page, "tutor-suppression-live", [WIDTHS[0]]);
  await emitVoice(page, { kind: "ended", reason: "the user ended it" });
  await page.waitForSelector("[data-word].bg-reading-highlight", {
    timeout: 10_000,
  });
  await shot(page, "tutor-suppression-restored", [WIDTHS[0]]);
});

test("tutor live in page mode gallery", async ({ page }) => {
  const { openReading, play } = await import("./readingHarness");
  await openReading(page);
  await play(page);
  await page.getByTestId("mini-player-follow-along").click();
  await page.waitForSelector('[data-testid="reading-view"]');
  await page.waitForTimeout(700);
  await page.getByTestId("reading-mode-toggle").click();
  await page.waitForTimeout(600);
  await emitVoice(page, { kind: "lifecycle", state: "connected" });
  await page.waitForTimeout(400);
  await dark(page);
  await shot(page, "voice-tutor-live-page-mode", [WIDTHS[0]]);
});

test("production rtl gallery", async ({ page }) => {
  const { openReading, play } = await import("./readingHarness");
  await page.addInitScript(() => {
    localStorage.setItem("txtcaster.direction", "rtl");
  });
  await openReading(page);
  await play(page);
  await page.getByTestId("mini-player-follow-along").click();
  await page.waitForSelector('[data-testid="reading-view"]');
  await page.waitForTimeout(700);
  await dark(page);
  await shot(page, "study-session-rtl-dark", [WIDTHS[0]]);
  await light(page);
  await shot(page, "study-session-rtl", [WIDTHS[0]]);
});

// Book Agent gallery (charter B-4): the companion's Book composer states in
// both themes plus RTL, and the switch confirmation. The wide shell shows the
// companion; consent flips to ready through the mocked grant flow.
test("book agent gallery", async ({ page }) => {
  const { openReading, play, enterReading } = await import("./readingHarness");
  await openReading(page);
  await play(page);
  await page.waitForTimeout(400);
  await enterReading(page);
  await page.waitForTimeout(800);
  await page.getByTestId("companion-tab-book").click();
  await page.waitForSelector('[data-testid="book-agent-consent"]');
  await shot(page, "book-agent-consent", [WIDTHS[0]]);
  await dark(page);
  await shot(page, "book-agent-consent-dark", [WIDTHS[0]]);
  await light(page);
  // RTL: the companion sits at inline-start and mirrors.
  await page.evaluate(() => {
    document.documentElement.setAttribute("dir", "rtl");
  });
  await page.waitForTimeout(200);
  await shot(page, "book-agent-consent-rtl", [WIDTHS[0]]);
  await page.evaluate(() => {
    document.documentElement.removeAttribute("dir");
  });
  await page.getByTestId("book-agent-consent-grant").click();
  await page.waitForSelector('[data-testid="book-agent-start"]');
  await shot(page, "book-agent-ready", [WIDTHS[0]]);
  await dark(page);
  await shot(page, "book-agent-ready-dark", [WIDTHS[0]]);
  await light(page);
});
