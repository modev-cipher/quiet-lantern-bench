import { expect, test, type Locator } from "@playwright/test";
import { castStep } from "./castSteps";
import { providerDisplayName } from "../src/i18n/providerNames";

import { enterReading, openReading, play } from "./readingHarness";
import {
  fixtureBook,
  fixtureCatalogModels,
  fixtureChapters,
  installTauriMock,
  type MockCatalogModel,
} from "./tauriMock";

const VENDOR = /gemini|google|openai|deepgram|cartesia|chatterbox|kokoro/i;

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

async function textOf(locator: Locator): Promise<string> {
  return (await locator.textContent()) ?? "";
}

test("the tutor setup hint names its required service through provider display data", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    voiceKeysScenario: { entries: [] },
  });
  await page.goto("/");
  await page.getByTestId("tutor-launcher").click();
  const hint = page.getByTestId("voice-session-key-hint");
  await expect(hint).toBeVisible();
  const provider = providerDisplayName("gemini");
  expect(await textOf(hint)).toContain(provider);
  expect(
    (await textOf(page.getByTestId("voice-session-panel")))
      .split(provider)
      .join(""),
  ).not.toMatch(VENDOR);
  await page.getByTestId("voice-session-key-action").click();
  await expect(page).toHaveURL(/#providers\/gemini$/);
});

test("the Book agent's unsupported state names the outcome, not a provider", async ({
  page,
}) => {
  await openReading(page, {}, {}, {}, { agentProbeState: "unsupported" });
  await play(page);
  await enterReading(page);
  await page.getByTestId("companion-tab-book").click();
  const unsupported = page.getByTestId("book-agent-unsupported");
  await expect(unsupported).toBeVisible();
  expect(await textOf(unsupported)).not.toMatch(VENDOR);
});

// One mock per page (two installTauriMock calls on one page collide, I18N-1).
test("the consent card carries no vendor copy while the egress host stays verbatim data", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await page.getByTestId("companion-tab-book").click();
  const consent = page.getByTestId("book-agent-consent");
  await expect(consent).toBeVisible();
  const hosts = page.getByTestId("book-agent-consent-host");
  await expect(hosts).toHaveCount(1);
  await expect(hosts.first()).toHaveText("generativelanguage.googleapis.com");
  await expect(hosts.first()).toHaveAttribute("dir", "ltr");
  const copy = (await textOf(consent)).replace(await textOf(hosts.first()), "");
  expect(copy).not.toMatch(VENDOR);
});

test("the clone-voice download card describes the outcome, not the cloning model", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: { chapters: fixtureChapters() },
    modelScenario: {
      catalogModels: [...fixtureCatalogModels(), CLONING_VOICE],
      installed: [{ modelId: "kokoro-82m", diskBytes: 355_000_000 }],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await expect(page.getByTestId("job-modal")).toBeVisible();
  await castStep(page, "sound");
  await page.getByTestId("clone-voice-open").click();
  const card = page.getByTestId("clone-needs-model");
  await expect(card).toBeVisible();
  await expect(card).toContainText("3.2 GB");
  expect(await textOf(page.getByTestId("clone-voice-modal"))).not.toMatch(
    VENDOR,
  );
});
