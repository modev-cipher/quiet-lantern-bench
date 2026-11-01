import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";

import { expect, test, type Browser, type Page } from "@playwright/test";

import readingGolden from "./fixtures/reading-timeline.golden.json" with { type: "json" };
import {
  BOOK_ID,
  BOOK_LICENCE,
  BOOK_TITLE,
  CAPTURE_SEEK_S,
  CAST_EXPORT_ID,
  CITATION,
  LIVE_TURNS,
  MEDIA_URL,
  RUNNING_JOB_ID,
  RUNNING_PROGRESS,
  captureBook,
  captureCatalogModels,
  captureChapters,
  captureExports,
  captureJobs,
  captureReading,
  captureSessions,
  captureVoiceKeys,
  captureVoiceProfiles,
  longWav,
} from "./siteCaptureFixture";
import { fixtureHardware, installTauriMock, type Scenario } from "./tauriMock";
import { emitVoice } from "./voiceEnvelope";

test.setTimeout(240_000);

test.skip(
  process.env.SITE_CAPTURES !== "1",
  "site capture rig — writes docs/design/site-captures; set SITE_CAPTURES=1 to regenerate deliberately",
);

const OUT = "../../docs/design/site-captures";
const BLACKBOX =
  /pypdfium2|pdfium|pymupdf|mupdf|tauri|\brust\b|cargo|sidecar|manim|cairo|typst|gguf|safetensors|pytorch|torch|onnx|tensorrt|vulkan|cuda|rocm|metal shader|llama|whisper|deepgram|gemini|googleapis|openai|anthropic|cartesia|kokoro|xtts|coqui|ollama|piper|vllm|ggml|\bhf\b|huggingface|\bengine\b|\bmodel id\b|q4km|q4_k_m|f16\b|mmproj/i;

type Theme = "dark" | "light";
interface Size {
  width: number;
  height: number;
}
const DESKTOP: Size[] = [
  { width: 1440, height: 900 },
  { width: 1024, height: 640 },
];
const PHONE: Size = { width: 390, height: 844 };

interface LedgerRow {
  file: string;
  id: string;
  size: string;
  theme: Theme;
  asserted: string;
  book: string;
  licence: string;
  sha256: string;
}
const ledger: LedgerRow[] = [];
const WAV = longWav();

function scenario(): Scenario {
  return {
    books: [captureBook()],
    onboardingComplete: true,
    reading: captureReading() as unknown as typeof readingGolden,
    readingScenario: { mediaDataUrl: MEDIA_URL },
    jobScenario: {
      chapters: captureChapters(),
      stageDelayMs: 600_000,
      voiceProfiles: captureVoiceProfiles(),
      seededJobs: captureJobs(),
    },
    seededExports: captureExports(),
    modelScenario: {
      catalogModels: captureCatalogModels(),
      hardware: fixtureHardware(),
      installed: [
        { modelId: "studio-voice", diskBytes: 355_000_000 },
        { modelId: "teaching-model-9b", diskBytes: 5_700_000_000 },
        { modelId: "figure-reader-4b", diskBytes: 3_300_000_000 },
      ],
    },
    voiceKeysScenario: captureVoiceKeys(),
    voiceSessionsScenario: captureSessions(),
  };
}

async function open(browser: Browser, size: Size): Promise<Page> {
  const context = await browser.newContext({
    viewport: size,
    deviceScaleFactor: 2,
  });
  const page = await context.newPage();
  if (size.width >= 1024 && size.width < 1200) {
    await page.addInitScript(() => {
      localStorage.setItem("txtcaster.reading.outline-open", "0");
    });
  }
  await page.route("**/site-capture-cast.wav", (route) => {
    const range = route
      .request()
      .headers()
      .range?.match(/bytes=(\d+)-(\d*)/);
    const start = range ? Number(range[1]) : 0;
    const end = range?.[2]
      ? Math.min(Number(range[2]), WAV.length - 1)
      : WAV.length - 1;
    return route.fulfill({
      status: range ? 206 : 200,
      headers: {
        "Accept-Ranges": "bytes",
        "Content-Type": "audio/wav",
        "Content-Length": String(end - start + 1),
        ...(range
          ? { "Content-Range": `bytes ${start}-${end}/${WAV.length}` }
          : {}),
      },
      body: WAV.subarray(start, end + 1),
    });
  });
  await installTauriMock(page, scenario());
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await expect(page.getByTestId(`talk-book-${BOOK_ID}`)).toBeVisible();
  return page;
}

async function theme(page: Page, value: Theme): Promise<void> {
  await page.evaluate((next) => {
    document.documentElement.setAttribute("data-theme", next);
  }, value);
  await page.waitForTimeout(450);
}

async function blackbox(page: Page): Promise<void> {
  const visible = await page.evaluate(() => {
    const attrs = Array.from(
      document.querySelectorAll("[title], [alt], [aria-label]"),
    )
      .map((node) =>
        [
          node.getAttribute("title"),
          node.getAttribute("alt"),
          node.getAttribute("aria-label"),
        ]
          .filter(Boolean)
          .join(" "),
      )
      .join(" ");
    return `${document.body.textContent ?? ""} ${attrs}`;
  });
  const hit = visible.match(BLACKBOX);
  expect(hit, `blackbox term on screen: ${hit?.[0] ?? ""}`).toBeNull();
}

async function capture(
  page: Page,
  id: string,
  size: Size,
  value: Theme,
  asserted: string,
  assert: () => Promise<void>,
): Promise<void> {
  await theme(page, value);
  await assert();
  await blackbox(page);
  const file = `CAP-${id}-${size.width}x${size.height}-${value}.png`;
  await page.screenshot({ path: `${OUT}/${file}` });
  ledger.push({
    file,
    id: `CAP-${id}`,
    size: `${size.width}×${size.height}`,
    theme: value,
    asserted,
    book: BOOK_TITLE,
    licence: BOOK_LICENCE,
    sha256: createHash("sha256")
      .update(readFileSync(`${OUT}/${file}`))
      .digest("hex"),
  });
}

async function playCast(page: Page): Promise<void> {
  await page.getByTestId("nav-audio").click();
  await page.getByTestId(`cover-quick-toggle-${CAST_EXPORT_ID}`).click();
  await expect(page.getByTestId("mini-player")).toBeVisible();
  await page.getByTestId("mini-player-follow-along").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await page.waitForFunction(() => {
    const audio = document.querySelector("audio");
    return (
      audio != null && Number.isFinite(audio.duration) && audio.duration > 3000
    );
  });
  await page.evaluate((seconds) => {
    const audio = document.querySelector("audio");
    if (audio != null) audio.currentTime = seconds;
  }, CAPTURE_SEEK_S);
  await expect(
    page.locator("[data-word].bg-reading-highlight").first(),
  ).toBeVisible({
    timeout: 15_000,
  });
  await expect(page.getByTestId("player-time")).toContainText("55:28");
  await page.waitForTimeout(600);
}

const FOLLOW_ASSERTED =
  "follow-along open on the finished Teacher cast; audio element playing (not paused); a word carries the highlight class; transport reads 55:28 total";

async function assertPlaying(page: Page): Promise<void> {
  const paused = await page.evaluate(
    () => document.querySelector("audio")?.paused ?? true,
  );
  expect(paused, "cast must be playing at capture").toBe(false);
  await expect(
    page.locator("[data-word].bg-reading-highlight").first(),
  ).toBeVisible();
  await expect(page.getByTestId("reading-view")).toHaveAttribute(
    "data-surface",
    "script",
  );
}

test("CAP-1 follow-along mid-playback", async ({ browser }) => {
  mkdirSync(OUT, { recursive: true });
  for (const size of DESKTOP) {
    const page = await open(browser, size);
    await playCast(page);
    for (const value of ["dark", "light"] as Theme[]) {
      await capture(page, "1", size, value, FOLLOW_ASSERTED, () =>
        assertPlaying(page),
      );
    }
    await page.context().close();
  }
});

test("CAP-1m follow-along at phone viewport", async ({ browser }) => {
  const page = await open(browser, PHONE);
  await playCast(page);
  await capture(
    page,
    "1m",
    PHONE,
    "dark",
    `${FOLLOW_ASSERTED}; desktop reading view rendered at a phone viewport`,
    () => assertPlaying(page),
  );
  await page.context().close();
});

async function talkSession(page: Page, size: Size): Promise<void> {
  await page.getByTestId(`talk-book-${BOOK_ID}`).click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await page.getByTestId("reading-mode-toggle").click();
  await expect(page.getByTestId("reading-view")).toHaveAttribute(
    "data-surface",
    "page",
  );
  if (size.width < 1200) {
    await page.getByTestId("reading-fit-page").click();
    await expect(page.getByTestId("reading-fit-page")).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    await page.waitForTimeout(400);
  }
  await emitVoice(page, { kind: "lifecycle", state: "connected" }, BOOK_ID);
  await emitVoice(
    page,
    { kind: "audio_state", state: "speaking", detail: "device playback" },
    BOOK_ID,
  );
  await emitVoice(
    page,
    { kind: "mic_state", state: "muted", detail: "half-duplex hold" },
    BOOK_ID,
  );
  for (const turn of LIVE_TURNS) {
    await emitVoice(
      page,
      { kind: "transcript", role: turn.role, text: turn.text },
      BOOK_ID,
    );
    await page.waitForTimeout(120);
  }
  await page.evaluate((citation) => {
    (
      window as unknown as {
        __mockEmitEvent: (event: string, payload: unknown) => void;
      }
    ).__mockEmitEvent("voice-agent-highlight", {
      ...citation,
      gen: 0,
      ackCallId: "call-site-capture",
    });
  }, CITATION);
  await expect(page.getByTestId("reading-facsimile-image")).toBeVisible();
  await expect(page.getByTestId("agent-citation")).toBeVisible();
  await page.waitForTimeout(700);
}

const TALK_ASSERTED =
  "book-direct Talk entry; page surface shown; session status pill reads Live; transcript lists the exchange; the cited passage outline is visible on the page";

async function assertTalk(page: Page): Promise<void> {
  await expect(page.getByTestId("voice-session-status")).toContainText("Live");
  await expect(page.getByTestId("voice-session-transcript")).toContainText(
    LIVE_TURNS[3].text.slice(0, 24),
  );
  await expect(page.getByTestId("agent-citation")).toBeVisible();
  await expect(page.getByTestId("reading-view")).toHaveAttribute(
    "data-surface",
    "page",
  );
}

test("CAP-2 book-direct Talk with an active citation", async ({ browser }) => {
  for (const size of DESKTOP) {
    const page = await open(browser, size);
    await talkSession(page, size);
    for (const value of ["dark", "light"] as Theme[]) {
      await capture(page, "2", size, value, TALK_ASSERTED, () =>
        assertTalk(page),
      );
    }
    await page.context().close();
  }
});

const CAST_ASSERTED =
  "new-cast dialog open; Teacher mode selected; chapters 4 and 5 checked; voice select carries a non-empty value; time estimate label rendered";

async function assertCastDialog(page: Page): Promise<void> {
  await expect(page.getByTestId("job-modal")).toBeVisible();
  await expect(page.getByTestId("chapter-check-ch-4")).toBeChecked();
  await expect(page.getByTestId("chapter-check-ch-5")).toBeChecked();
  await expect(
    page.getByTestId("mode-teacher").getByRole("radio"),
  ).toBeChecked();
  await expect(page.getByTestId("voice-select-button")).toBeVisible();
  await expect(page.getByTestId("voice-select")).not.toHaveValue("");
  await expect(page.getByTestId("eta-label")).toContainText(/min|hour|h\b/);
}

test("CAP-3 new-cast dialog", async ({ browser }) => {
  for (const size of DESKTOP) {
    const page = await open(browser, size);
    await page.getByTestId(`create-audio-${BOOK_ID}`).click();
    await expect(page.getByTestId("job-modal")).toBeVisible();
    await page.getByTestId("select-all-chapters").click();
    await expect(page.getByTestId("chapter-check-ch-1")).not.toBeChecked();
    await page.getByTestId("chapter-check-ch-4").check();
    await page.getByTestId("chapter-check-ch-5").check();
    await page.getByTestId("mode-teacher").click();
    await page.waitForTimeout(500);
    await capture(page, "3", size, "light", CAST_ASSERTED, () =>
      assertCastDialog(page),
    );
    await page.context().close();
  }
});

const JOB_ASSERTED =
  "production view open; the running Teacher job sits in the Script stage; progress bar width above zero; current item names Figure 6.12";

async function assertJob(page: Page): Promise<void> {
  const bar = page.getByTestId(`job-progress-bar-${RUNNING_JOB_ID}`);
  await expect
    .poll(async () => (await bar.boundingBox())?.width ?? 0)
    .toBeGreaterThan(0);
  await expect(
    page.getByTestId(`job-current-item-${RUNNING_JOB_ID}`),
  ).toContainText("Figure 6.12");
  await expect(
    page.getByTestId(`job-step-${RUNNING_JOB_ID}-3`),
  ).toHaveAttribute("data-status", "active");
}

test("CAP-4 running conversion", async ({ browser }) => {
  for (const size of DESKTOP) {
    const page = await open(browser, size);
    await page.getByTestId("nav-jobs").click();
    await page.getByTestId("open-production").click();
    await expect(page.getByTestId(`job-row-${RUNNING_JOB_ID}`)).toBeVisible();
    await page.evaluate((progress) => {
      (
        window as unknown as {
          __mockEmitEvent: (event: string, payload: unknown) => void;
        }
      ).__mockEmitEvent("job-progress", progress);
    }, RUNNING_PROGRESS);
    await page.waitForTimeout(500);
    for (const value of ["dark", "light"] as Theme[]) {
      await capture(page, "4", size, value, JOB_ASSERTED, () =>
        assertJob(page),
      );
    }
    await page.context().close();
  }
});

test.afterAll(() => {
  if (ledger.length === 0) return;
  const header = [
    "# Site captures ledger",
    "",
    `Mock book: ${BOOK_TITLE} (${BOOK_LICENCE}); title and chapter names only, prose is original. Generated by apps/desktop/e2e/site-captures.spec.ts at device scale factor 2.`,
    "",
    "| File | CAP | Viewport | Theme | State asserted before the shot | Mock book | Licence | sha256 |",
    "|---|---|---|---|---|---|---|---|",
  ];
  const rows = ledger.map(
    (row) =>
      `| ${row.file} | ${row.id} | ${row.size} | ${row.theme} | ${row.asserted} | ${row.book} | ${row.licence} | ${row.sha256} |`,
  );
  writeFileSync(`${OUT}/LEDGER.md`, `${[...header, ...rows].join("\n")}\n`);
});
