import fs from "node:fs";
import path from "node:path";

import { expect, test, type Page } from "@playwright/test";

import { e2eLocale, msg } from "./i18nText";
import { wavDataUrl } from "./pcmFixture";
import { fixtureBook, installTauriMock } from "./tauriMock";

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

async function boot(page: Page, withProgress: boolean): Promise<void> {
  await page.setViewportSize({ width: 1440, height: 900 });
  await installTauriMock(page, {
    books: [fixtureBook()],
    seededExports: withProgress ? [seeded] : [],
    readingScenario: { mediaDataUrl: wavDataUrl() },
  });
  if (withProgress) {
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
  }
  await page.goto("/");
  await expect(page.getByTestId("library")).toBeVisible();
}

test("setup floats as one next-step pill that opens the checklist and can be hidden", async ({
  page,
}) => {
  await boot(page, false);
  const pill = page.getByTestId("setup-pill");
  await expect(pill).toBeVisible();
  await expect(pill).toContainText(msg("setup.next"));
  await page.getByTestId("setup-toggle").click();
  const list = page.getByTestId("setup-checklist");
  await expect(list).toBeVisible();
  await expect(list.getByTestId("checklist-item-import")).toContainText(
    msg("setup.import"),
  );
  await page.getByTestId("setup-hide").click();
  await expect(pill).toHaveCount(0);
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-library").click();
  await expect(page.getByTestId("setup-pill")).toHaveCount(0);
});

test("the floating elements never move the shelf", async ({ page }) => {
  await boot(page, true);
  await expect(page.getByTestId("study-hero")).toBeVisible();
  const heading = page.locator("#main-content h1").first();
  await page.evaluate(() =>
    Promise.all(
      document
        .getAnimations()
        .filter(
          (animation) => animation.effect?.getTiming().iterations !== Infinity,
        )
        .map((animation) => animation.finished),
    ),
  );
  const withFloat = (await heading.boundingBox())!.y;
  await page.evaluate(() =>
    document.getElementById("main-overlay")?.replaceChildren(),
  );
  expect(
    Math.abs((await heading.boundingBox())!.y - withFloat),
  ).toBeLessThanOrEqual(0.5);
});

test("Resume floats in the corner and steps aside once something plays", async ({
  page,
}) => {
  await boot(page, true);
  const card = page.getByTestId("shelf-continue");
  await expect(card).toBeVisible();
  const box = (await card.boundingBox())!;
  const main = (await page.locator("#main-content").boundingBox())!;
  expect(main.y + main.height - (box.y + box.height)).toBeLessThan(40);
  await page.getByTestId("study-resume").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("mini-player")).toBeVisible();
  await expect(page.getByTestId("shelf-continue")).toHaveCount(0);
});

test("with something playing, the setup pill sits above the mini player and stays usable", async ({
  page,
}) => {
  await boot(page, true);
  await page.setViewportSize({ width: 900, height: 700 });
  await page.getByTestId("study-resume").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await page.keyboard.press("Escape");
  const player = page.getByTestId("mini-player");
  await expect(player).toBeVisible();
  const pill = page.getByTestId("setup-pill");
  await expect(pill).toBeVisible();
  await expect
    .poll(async () => {
      const pillBox = (await pill.boundingBox())!;
      const playerBox = (await player.boundingBox())!;
      return playerBox.y - (pillBox.y + pillBox.height);
    })
    .toBeGreaterThanOrEqual(0);
  await page.getByTestId("setup-toggle").click();
  await expect(page.getByTestId("setup-checklist")).toBeVisible();
});

test("the pill follows a language change without a setup change", async ({
  page,
}) => {
  const other = e2eLocale === "ar" ? "en" : "ar";
  const otherCatalog = Object.assign(
    {},
    ...fs
      .readdirSync(path.join("src", "i18n", "messages", other))
      .filter((file) => file.endsWith(".json"))
      .map((file) =>
        JSON.parse(
          fs.readFileSync(
            path.join("src", "i18n", "messages", other, file),
            "utf8",
          ),
        ),
      ),
  ) as Record<string, string>;
  await boot(page, false);
  await page.getByTestId("setup-toggle").click();
  await expect(page.getByTestId("checklist-item-import")).toContainText(
    msg("setup.import"),
  );
  await page.getByTestId("nav-settings").click();
  await page.getByTestId(`language-choice-${other}`).click();
  await page.getByTestId("nav-library").click();
  await expect(page.getByTestId("setup-show-me")).toContainText(
    otherCatalog["setup.showMe"],
  );
  await page.getByTestId("setup-toggle").click();
  await expect(page.getByTestId("checklist-item-import")).toContainText(
    otherCatalog["setup.import"],
  );
});
