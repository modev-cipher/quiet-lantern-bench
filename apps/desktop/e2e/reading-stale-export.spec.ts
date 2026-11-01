import { expect, test } from "@playwright/test";

import { EXPORT_ID, enterReading, openReading, play } from "./readingHarness";
import { markExportStale } from "./tauriMock";

test("a stale export offers neither playback nor follow-along", async ({
  page,
}) => {
  await openReading(page, {}, { staleExport: true });

  await expect(page.getByTestId(`audio-stale-${EXPORT_ID}`)).toBeVisible();
  await expect(
    page.getByTestId(`cover-quick-toggle-${EXPORT_ID}`),
    "canPlay refuses a stale export before anything asks the backend for a URL",
  ).toHaveCount(0);
  await expect(page.getByTestId("mini-player-follow-along")).toHaveCount(0);
  await expect(page.getByTestId("reading-view")).toHaveCount(0);

  test.info().annotations.push({
    type: "residual",
    description:
      "the backend's refusal message (STALE_EXPORT_MESSAGE) is not reachable from the shelf, because canPlay gates a stale row earlier. The mock's refusal and the Rust literal guard therefore pin a regression boundary rather than a live user path: if that earlier gate is ever relaxed, one message is already shared by both ends instead of a second one being invented.",
  });
});

test("an export that goes stale mid-session loses its controls on the next read", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await expect(page.getByTestId("reading-script")).toBeVisible();

  await markExportStale(page);
  await page.getByTestId("reading-close").click();
  await page.getByTestId("nav-jobs").click();
  await page.getByTestId("open-production").click();
  await page.getByTestId("nav-library").click();
  await page.getByTestId("nav-audio").click();

  await expect(page.getByTestId(`audio-stale-${EXPORT_ID}`)).toBeVisible({
    timeout: 5_000,
  });
  await expect(page.getByTestId(`cover-quick-toggle-${EXPORT_ID}`)).toHaveCount(
    0,
  );

  test.info().annotations.push({
    type: "residual",
    description:
      "two things are deliberately not asserted because they are not true today and fixing them is not this PR's scope: the already-loaded item in the playback store keeps its own stale flag, so its follow-along entry survives until playback is stopped; and getMediaUrl memoises per path, so an audio stream already registered is never re-requested and cannot be refused mid-stream. Both are stated rather than implied.",
  });
});
