import fs from "node:fs";
import path from "node:path";

import { test, type Page } from "@playwright/test";

import { enterReading, openReading, play } from "./readingHarness";
import { emitVoice } from "./voiceEnvelope";
import {
  fixtureBook,
  fixtureCatalogModels,
  fixtureChapters,
  fixtureHardware,
  installTauriMock,
  type MockCatalogModel,
  type MockSessionRow,
} from "./tauriMock";

// I18N-1 geometry rig (#53 analogue for the UI). For every core surface it
// records the bounding box of each visible `data-testid` landmark plus the
// document direction, then a screenshot. Baseline = EN at main; the gate
// diffs EN head vs baseline (≤2 px) and checks AR landmarks mirror about the
// viewport centre. Set I18N_GEOMETRY_OUT=<dir>; I18N_LOCALE=en|ar|system.
const OUT = process.env.I18N_GEOMETRY_OUT;
const LOCALE = process.env.I18N_LOCALE ?? "system";
test.skip(!OUT, "geometry rig — set I18N_GEOMETRY_OUT to record deliberately");
test.setTimeout(120_000);

const VIEWPORT = { width: 1440, height: 900 };
const MODELS = {
  catalogModels: fixtureCatalogModels(),
  hardware: fixtureHardware(),
  downloadTicks: 2,
  downloadDelayMs: 60,
};

export interface Landmark {
  testid: string;
  x: number;
  y: number;
  w: number;
  h: number;
  dir: string;
  text: string;
}

async function record(page: Page, name: string) {
  await page.setViewportSize(VIEWPORT);
  await page.waitForTimeout(400);
  // entrance animations (modal-in 0.28 s, scale 0.98→1) must finish before boxes are read
  // (infinite animations — progress rings — never finish: skip them, cap the wait)
  await page.evaluate(() =>
    Promise.race([
      Promise.all(
        document
          .getAnimations()
          .filter((a) => a.effect?.getTiming().iterations !== Infinity)
          .map((a) => a.finished.catch(() => null)),
      ),
      new Promise((resolve) => setTimeout(resolve, 1500)),
    ]),
  );
  const landmarks: Landmark[] = await page.evaluate(() =>
    Array.from(document.querySelectorAll<HTMLElement>("[data-testid]"))
      .map((el) => ({ el, r: el.getBoundingClientRect() }))
      .filter(({ r }) => r.width > 0 && r.height > 0)
      .map(({ el, r }) => ({
        testid: el.dataset.testid ?? "",
        x: Math.round(r.left),
        y: Math.round(r.top),
        w: Math.round(r.width),
        h: Math.round(r.height),
        dir: getComputedStyle(el).direction,
        text: (el.textContent ?? "").replace(/\s+/g, " ").trim().slice(0, 40),
      })),
  );
  const htmlDir = await page.locator("html").getAttribute("dir");
  fs.mkdirSync(OUT!, { recursive: true });
  fs.writeFileSync(
    path.join(OUT!, `${name}.json`),
    JSON.stringify(
      { name, locale: LOCALE, htmlDir, viewport: VIEWPORT, landmarks },
      null,
      1,
    ),
  );
  await page.screenshot({ path: path.join(OUT!, `${name}.png`) });
}

// AR/EN runs stand in for a persisted preference: the localStorage mirror
// (first paint) plus the mock's persisted answer (what the backend would return).
async function persistLocale(page: Page) {
  if (LOCALE === "system") return;
  await page.addInitScript((l) => {
    localStorage.setItem("txtcaster.locale", l);
    sessionStorage.setItem("mock-locale-preference", l);
  }, LOCALE);
}

async function boot(
  page: Page,
  scenario: Parameters<typeof installTauriMock>[1],
) {
  await installTauriMock(page, scenario);
  await persistLocale(page);
  await page.goto("/");
}

test("wizard", async ({ page }) => {
  await boot(page, {
    books: [],
    onboardingComplete: false,
    modelScenario: MODELS,
  });
  await page.waitForSelector('[data-testid="wizard-scan-card"]');
  await record(page, "wizard-step1");
});

test("library, audio shelf, send to phone", async ({ page }) => {
  await boot(page, {
    books: [
      fixtureBook(),
      fixtureBook({
        id: "book-2",
        title: "A Second, Much Longer Book Title That Wraps",
        author: "Another Author",
        pageCount: 340,
        chapterCount: 18,
        createdAt: 2,
      }),
    ],
    jobScenario: { chapters: fixtureChapters() },
  });
  await page.getByTestId("nav-library").click();
  await page.waitForSelector('[data-testid="book-card"]');
  await record(page, "library-books");
  await page.getByTestId("nav-audio").click();
  await page.waitForTimeout(300);
  await record(page, "library-audios");
});

test("job modal and jobs", async ({ page }) => {
  const catalogModels = fixtureCatalogModels();
  await boot(page, {
    books: [fixtureBook()],
    modelScenario: {
      catalogModels,
      installed: catalogModels.map((m) => ({ modelId: m.id, diskBytes: 1 })),
      hardware: fixtureHardware(),
      downloadTicks: 1,
      downloadDelayMs: 10,
    },
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 8000 /* running state must still be running when read */,
      outputPath: "/exports/fixture-book.mp3",
    },
  });
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.waitForSelector('[data-testid="job-modal"]');
  await record(page, "job-modal");
  // #31 capture: teacher mode shows the per-step names (role · tier) and outcomes
  await page.getByTestId("mode-teacher").click();
  await page.waitForTimeout(400);
  await record(page, "new-cast-dialog-teach");
  await page.getByTestId("mode-narrator").click();
  await page.waitForTimeout(300);
  await page.getByTestId("start-job").click();
  await page.waitForTimeout(600);
  await record(page, "jobs-running");
});

test("models, settings, providers", async ({ page }) => {
  await boot(page, { books: [], modelScenario: MODELS });
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-models").click();
  await page.waitForTimeout(500);
  await record(page, "models");
  await page.getByTestId("nav-settings").click();
  await page.waitForSelector('[data-testid="storage-settings"]');
  await record(page, "settings");
  await page.getByTestId("nav-providers").click();
  await page.waitForTimeout(500);
  await record(page, "providers");
});

test("shelf home", async ({ page }) => {
  await boot(page, { books: [fixtureBook()] });
  await page.waitForSelector('[data-testid="library"]');
  await record(page, "shelf");
});

test("mini player, now playing, reading (page + script), voice panel", async ({
  page,
}) => {
  await persistLocale(page);
  await openReading(page);
  await play(page);
  // Freeze playback at a fixed position: which segment/page is shown, the
  // listened marker and the progress fill all follow the clock otherwise.
  await page.evaluate(() => {
    const audio = document.querySelector("audio");
    if (audio) {
      audio.pause();
      audio.currentTime = 3;
    }
  });
  await page.waitForTimeout(500);
  await record(page, "library-mini-player");
  await enterReading(page);
  await page.waitForTimeout(600);
  await record(page, "reading-view");
  const script = page.getByTestId("reading-mode-script");
  if (await script.count()) {
    await script.click();
    await page.waitForTimeout(400);
    await record(page, "reading-script");
  }
  const outline = page.getByTestId("outline-toggle");
  if (await outline.count()) {
    await outline.click();
    await page.waitForTimeout(400);
    await record(page, "reading-outline-open");
  }
});

// I18N-2: the Voice and Tutor surfaces. Deterministic: the session is adopted
// through a lifecycle event (no timers), transcript turns are emitted once,
// the probe answer is the mock's fixed consent_required, playback is frozen.
const CLONING_VOICE: MockCatalogModel = {
  id: "chatterbox-0.5b",
  task: "tts",
  display_name: "Chatterbox",
  family: "chatterbox",
  quant: null,
  format: "pytorch",
  disk_bytes: 3_191_966_992,
  full_gpu_vram_mb: 5120,
  min_free_ram_mb: 6144,
  hybrid_capable: false,
  quality_score: 90,
  speed_score: 70,
  capabilities: ["voice_cloning"],
  license: { spdx: "MIT", commercial_ok: true },
  perf: { rtf_gpu: 0.2, rtf_cpu: 4.0 },
};
const SESSION_ROW: MockSessionRow = {
  id: "session-0",
  book_id: null, // standalone: listed by the Study desk History
  job_id: null,
  started_at: 1_757_000_000_000 - 38 * 60_000,
  ended_at: 1_757_000_000_000,
  end_reason: "ended",
  duration_ms: 38 * 60_000,
  turn_count: 2,
  summary_excerpt: "We proved Lemma 3.2 and started on measure spaces.",
  position: { kind: "offset", value: 120 },
  job_resolves: false,
  provider_changed: false,
};
const SESSION_TURNS = {
  "session-0": [
    {
      turn_idx: 0,
      role: "user" as const,
      text: "Why does σ-additivity matter?",
      created_at: 1,
    },
    {
      turn_idx: 1,
      role: "assistant" as const,
      text: "Because a countable union must stay measurable. See [p.12].",
      created_at: 2,
    },
  ],
};
test("voice tutor, book agent, clone voice, session view", async ({ page }) => {
  await persistLocale(page);
  await openReading(
    page,
    {},
    {},
    {},
    {
      voiceSessionsScenario: { rows: [SESSION_ROW], turns: SESSION_TURNS },
    },
  );
  await play(page);
  await enterReading(page);
  await page.evaluate(() => {
    const audio = document.querySelector("audio");
    if (audio) {
      audio.pause();
      audio.currentTime = 3;
    }
  });
  await emitVoice(page, { kind: "lifecycle", state: "connected" });
  await page.waitForSelector('[data-testid="voice-session-stop"]');
  await emitVoice(page, {
    kind: "transcript",
    role: "user",
    text: "What is a σ-algebra? See /notes/ch3.md",
  });
  await emitVoice(page, {
    kind: "transcript",
    role: "assistant",
    text: "A family of sets closed under complement and countable union [p.12].",
  });
  await page.waitForSelector('[data-testid="voice-session-transcript"]');
  await record(page, "reading-companion-tutor-live");
  await page.getByTestId("companion-tab-book").click();
  await page.waitForSelector('[data-testid="book-agent-consent"]');
  await record(page, "reading-companion-book");
  await page.getByTestId("reading-close").click();
  await page.getByTestId("tutor-launcher").click();
  await page.waitForSelector('[data-testid="study-standalone-tutor"]');
  await page.getByTestId("session-history-row-session-0").click();
  await page.waitForSelector('[data-testid="voice-session-transcript"]');
  await record(page, "session-view");
  await boot(page, {
    books: [fixtureBook()],
    jobScenario: { chapters: fixtureChapters() },
    modelScenario: {
      catalogModels: [...fixtureCatalogModels(), CLONING_VOICE],
      installed: [
        { modelId: "kokoro-82m", diskBytes: 355_000_000 },
        { modelId: "chatterbox-0.5b", diskBytes: 3_191_966_992 },
      ],
    },
  });
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.waitForSelector('[data-testid="job-modal"]');
  await page.getByTestId("clone-voice-open").click();
  await page.waitForSelector('[data-testid="clone-voice-modal"]');
  await record(page, "clone-voice-modal");
});
