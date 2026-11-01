import { expect, test, type Page } from "@playwright/test";
import { castStep } from "./castSteps";
import { msg } from "./i18nText";

import {
  fixtureBook,
  fixtureCatalogModels,
  fixtureChapters,
  fixtureHardware,
  installTauriMock,
  type CastProviderScenario,
} from "./tauriMock";

// F6 redesign proof rows (owner feedback 2026-08-29): the chapter picker is
// VISIBLE without instruction at every real window shape, the dialog never
// overflows sideways, each phase says what runs it, effort is an honest
// per-phase override, and the voice tutor is discoverable from the cast
// flow.

const MODELS = {
  catalogModels: fixtureCatalogModels(),
  hardware: fixtureHardware(),
  installed: [{ modelId: "kokoro-82m", diskBytes: 355_000_000 }],
};

const CLOUD: CastProviderScenario = {
  scriptConnections: [
    {
      connectionId: "c-ollama",
      name: "Local Ollama",
      provider: "openai_compatible",
      defaultModel: "llama3",
      loopback: true,
    },
  ],
  ttsVoices: [],
};

async function openModal(page: Page, cast?: CastProviderScenario) {
  await installTauriMock(page, {
    books: [fixtureBook()],
    onboardingComplete: true,
    modelScenario: MODELS,
    castProviders: cast,
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 250,
      outputPath: "/exports/fixture-book.mp3",
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await expect(page.getByTestId("job-modal")).toBeVisible();
}

// The owner's own shapes: full desktop, laptop, a small maximized laptop,
// and a genuinely cramped window. Heights matter as much as widths — the
// original failure was a tall dialog swallowing the chapter list.
const SHAPES = [
  { width: 1920, height: 1080 },
  { width: 1440, height: 900 },
  { width: 1100, height: 768 },
  { width: 900, height: 700 },
];

test("the chapter picker and the Cast action are visible at every window shape without any scrolling", async ({
  page,
}) => {
  await openModal(page);
  for (const shape of SHAPES) {
    await page.setViewportSize(shape);
    await page.waitForTimeout(250);
    const firstChapter = page.getByTestId("chapter-check-ch-1");
    await expect(
      firstChapter,
      `chapter checkbox hidden at ${shape.width}x${shape.height}`,
    ).toBeVisible();
    const box = await firstChapter.boundingBox();
    expect(box, `no box at ${shape.width}x${shape.height}`).not.toBeNull();
    expect(
      box!.y >= 0 && box!.y + box!.height <= shape.height,
      `first chapter row is outside the ${shape.width}x${shape.height} viewport (y=${box!.y})`,
    ).toBe(true);
    const cast = await page.getByTestId("start-job").boundingBox();
    expect(
      cast != null && cast.y + cast.height <= shape.height && cast.y >= 0,
      `Cast button outside the ${shape.width}x${shape.height} viewport`,
    ).toBe(true);
    const overflow = await page
      .getByTestId("job-modal-panes")
      .evaluate((el) => el.scrollWidth - el.clientWidth);
    expect(
      overflow,
      `horizontal overflow at ${shape.width}x${shape.height}`,
    ).toBeLessThanOrEqual(1);
  }
});

test("chapters list titles, page ranges, and per-chapter checkboxes; select-all round-trips", async ({
  page,
}) => {
  await openModal(page);
  await expect(page.getByTestId("chapter-list")).toBeVisible();
  await expect(page.getByTestId("job-modal")).toContainText("Getting Started");
  await expect(page.getByTestId("job-modal")).toContainText(msg("cast.selectedCount", { selected: 3, total: 3 }));
  await page.getByTestId("chapter-check-ch-2").uncheck();
  await expect(page.getByTestId("job-modal")).toContainText(msg("cast.selectedCount", { selected: 2, total: 3 }));
  await page.getByTestId("select-all-chapters").click();
  await expect(page.getByTestId("job-modal")).toContainText(msg("cast.selectedCount", { selected: 3, total: 3 }));
  await page.getByTestId("select-all-chapters").click();
  await expect(page.getByTestId("job-modal")).toContainText(msg("cast.selectedCount", { selected: 0, total: 3 }));
  await expect(page.getByTestId("start-job")).toBeDisabled();
});

test("the phase summary names what runs scripting, figures, and the voice — plain words with the ruled defaults", async ({
  page,
}) => {
  await openModal(page);
  await castStep(page, "review");
  const summary = page.getByTestId("phase-summary");
  await expect(summary).toBeVisible();
  await expect(page.getByTestId("phase-summary-script")).toContainText(
    msg("phase.script.narrator"),
  );
  await expect(page.getByTestId("phase-summary-figures")).toBeVisible();
  await expect(page.getByTestId("phase-summary-tts")).toContainText(
    msg("phase.voice.local", { voice: "" }).trim(),
  );
});

test("a cloud scripting choice submits the ruled LOW effort by default and an explicit raise verbatim", async ({
  page,
}) => {
  await openModal(page, CLOUD);
  await castStep(page, "sound");
  await page.getByTestId("mode-teacher").click();
  await expect(page.getByTestId("cast-cascade-model-llm")).toHaveValue(
    "cloud:c-ollama:llama3",
  );
  const effort = page.getByTestId("cast-script-effort");
  await expect(effort).toHaveValue("low");
  await expect(page.getByTestId("cast-script-effort-hint")).toContainText(
    msg("cast.effort.low.hint"),
  );
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-modal")).not.toBeVisible();
  const first = await page.evaluate(
    () =>
      (
        window as unknown as {
          __createJobCalls?: Array<Record<string, unknown>>;
        }
      ).__createJobCalls?.[0],
  );
  expect(first?.scriptCloud).toMatchObject({
    connectionId: "c-ollama",
    effort: "low",
  });
});

test("raising effort per phase rides the submission and the phase summary says so honestly", async ({
  page,
}) => {
  await openModal(page, CLOUD);
  await castStep(page, "sound");
  await page.getByTestId("mode-teacher").click();
  await expect(page.getByTestId("cast-cascade-model-llm")).toHaveValue(
    "cloud:c-ollama:llama3",
  );
  await page.getByTestId("cast-script-effort").selectOption("high");
  await castStep(page, "review");
  await expect(page.getByTestId("phase-summary-script")).toContainText(
    msg("cast.effort.high.label"),
  );
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-modal")).not.toBeVisible();
  const first = await page.evaluate(
    () =>
      (
        window as unknown as {
          __createJobCalls?: Array<Record<string, unknown>>;
        }
      ).__createJobCalls?.[0],
  );
  expect(first?.scriptCloud).toMatchObject({ effort: "high" });
});

test("gallery capture: the cast dialog at the viewport matrix, both themes", async ({
  page,
}) => {
  test.skip(
    process.env.GALLERY_SHOTS !== "1",
    "set GALLERY_SHOTS=1 to regenerate",
  );
  await openModal(page, CLOUD);
  for (const shape of SHAPES) {
    await page.setViewportSize(shape);
    await page.waitForTimeout(300);
    for (const theme of ["light", "dark"]) {
      await page.evaluate((value) => {
        document.documentElement.setAttribute("data-theme", value);
      }, theme);
      await page.waitForTimeout(250);
      await page.screenshot({
        path: `../../docs/design/gallery/cast-dialog-${shape.width}x${shape.height}-${theme}.png`,
      });
    }
  }
});

test("the voice tutor is discoverable from the cast flow and opens the reader beside the book", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1100, height: 720 });
  await openModal(page);
  const entry = page.getByTestId("open-voice-tutor");
  await expect(entry).toBeInViewport({ ratio: 1 });
  await expect(entry).toHaveAccessibleName(msg("cast.talkAbout"));
  await expect(entry).toHaveAccessibleDescription(msg("cast.talkHint"));
  await entry.click();
  await expect(page.getByTestId("job-modal")).not.toBeVisible();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(page.getByTestId("reading-surface")).toBeVisible();
  const facsimile = page.getByTestId("reading-facsimile-image").first();
  await expect(facsimile).toBeVisible();
  await expect
    .poll(() =>
      facsimile.evaluate((img) => (img as HTMLImageElement).naturalWidth),
    )
    .toBeGreaterThan(0);
  await expect(page.locator("[data-word].bg-reading-highlight")).toHaveCount(0);
  await expect(page.getByTestId("voice-session-panel")).toBeVisible();
  await expect(page.getByTestId("tutor-panel")).toHaveCount(0);
});

for (const viewport of [
  { width: 1100, height: 720 },
  { width: 1440, height: 900 },
]) {
  test(`at ${viewport.width}×${viewport.height} every cast mode is whole above the footer`, async ({
    page,
  }) => {
    await page.setViewportSize(viewport);
    await openModal(page);
    await castStep(page, "sound");
    const footer = await page
      .getByTestId("job-modal")
      .locator("footer")
      .boundingBox();
    for (const mode of ["narrator", "teacher"]) {
      const card = await page.getByTestId(`mode-${mode}`).boundingBox();
      expect(
        card!.y + card!.height,
        `${mode} ends under the footer, so the choice is clipped at the default window`,
      ).toBeLessThanOrEqual(footer!.y);
    }
    await expect(page.getByTestId("mode-voice-tutor-card")).toHaveCount(0);
  });
}
