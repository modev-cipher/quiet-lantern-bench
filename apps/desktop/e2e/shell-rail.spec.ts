import { expect, test, type Page } from "@playwright/test";

import { e2eLocale, msg } from "./i18nText";
import { audioState, enterReading, openReading, play } from "./readingHarness";
import { wavDataUrl } from "./pcmFixture";
import { fixtureBook, fixtureChapters, installTauriMock } from "./tauriMock";

const rtl = e2eLocale === "ar";

async function boot(page: Page): Promise<void> {
  await page.setViewportSize({ width: 1440, height: 900 });
  await installTauriMock(page, {
    books: [
      fixtureBook(),
      fixtureBook({
        id: "book-jane",
        title: "Jane Eyre",
        author: "Charlotte Brontë",
      }),
    ],
  });
  await page.goto("/");
  await expect(page.getByTestId("launch-intro")).toHaveCount(0);
}

test("the rail replaces the top bar and holds the reading-start edge", async ({
  page,
}) => {
  await boot(page);
  await expect(page.getByTestId("topbar")).toHaveCount(0);
  const rail = (await page.getByTestId("shell-rail").boundingBox())!;
  expect(rail.width).toBeLessThanOrEqual(64);
  expect(rail.height).toBeGreaterThanOrEqual(900 - 1);
  if (rtl) expect(rail.x + rail.width).toBeGreaterThanOrEqual(1440 - 1);
  else expect(rail.x).toBeLessThanOrEqual(1);
  const main = (await page.locator("#main-content").boundingBox())!;
  if (rtl) expect(main.x + main.width).toBeLessThanOrEqual(rail.x + 1);
  else expect(main.x).toBeGreaterThanOrEqual(rail.x + rail.width - 1);
});

test("the brand mark returns home from Settings and Listening with mouse and keyboard", async ({
  page,
}) => {
  await boot(page);
  await page.getByTestId("nav-settings").click();
  await expect(page.getByTestId("library")).toHaveCount(0);
  await page.getByTestId("shell-rail").getByTestId("brand-mark").click();
  await expect(page.getByTestId("library")).toBeVisible();
  await expect(page.getByTestId("book-grid")).toBeVisible();

  const home = page.getByTestId("nav-home");
  await expect(home).toHaveRole("button");
  await expect(home).toHaveAccessibleName(msg("nav.home"));
  for (const key of ["Enter", "Space"]) {
    await page.getByTestId("nav-listening").click();
    await expect(page.getByTestId("nav-audio")).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await home.focus();
    await page.keyboard.press(key);
    await expect(page.getByTestId("zone-books")).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await expect(page.getByTestId("book-grid")).toBeVisible();
  }
});

test("going home clears the Casting filter", async ({ page }) => {
  await boot(page);
  await page.getByTestId("filter-casting").click();
  await expect(page.getByTestId("book-card")).toHaveCount(0);
  await page.getByTestId("shell-rail").getByTestId("brand-mark").click();
  await expect(page.getByTestId("filter-all-books")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(page.getByTestId("book-card")).toHaveCount(2);
});

test("every rail item has a name, and the label shows on hover and on keyboard focus", async ({
  page,
}) => {
  await boot(page);
  for (const id of [
    "nav-home",
    "nav-search",
    "nav-library",
    "nav-listening",
    "nav-jobs",
    "nav-report-issue",
    "nav-settings",
  ]) {
    await expect(page.getByTestId(id)).toHaveAttribute("aria-label", /\S/);
  }
  const shelf = page.getByTestId("nav-library");
  const tip = shelf.locator(".rail-tip");
  await expect(tip).toHaveCSS("opacity", "0");
  await shelf.hover();
  await expect(tip).toHaveCSS("opacity", "1");
  await page.mouse.move(700, 450);
  await expect(tip).toHaveCSS("opacity", "0");
  await page.getByTestId("nav-search").focus();
  await page.keyboard.press("Tab");
  await expect(shelf).toBeFocused();
  await expect(tip).toHaveCSS("opacity", "1");
});

test("the marker follows the current destination and Listening opens the audio shelf", async ({
  page,
}) => {
  await boot(page);
  await page.getByTestId("nav-library").click();
  await expect(page.getByTestId("nav-library")).toHaveAttribute(
    "aria-current",
    "page",
  );
  const marker = page.getByTestId("rail-marker");
  const onShelf = (await marker.boundingBox())!;
  await page.getByTestId("nav-listening").click();
  await expect(page.getByTestId("nav-listening")).toHaveAttribute(
    "aria-current",
    "page",
  );
  await expect(page.getByTestId("nav-library")).not.toHaveAttribute(
    "aria-current",
    "page",
  );
  await expect
    .poll(async () => (await marker.boundingBox())!.y)
    .toBeGreaterThan(onShelf.y + 20);
  const item = (await page.getByTestId("nav-listening").boundingBox())!;
  await expect
    .poll(async () => Math.abs((await marker.boundingBox())!.y - item.y))
    .toBeLessThanOrEqual(1);
});

test("the activity tray opens beside the rail", async ({ page }) => {
  await boot(page);
  await page.getByTestId("nav-jobs").click();
  const tray = (await page.getByTestId("activity-tray").boundingBox())!;
  const rail = (await page.getByTestId("shell-rail").boundingBox())!;
  if (rtl) expect(tray.x + tray.width).toBeLessThanOrEqual(rail.x);
  else expect(tray.x).toBeGreaterThanOrEqual(rail.x + rail.width);
  if (rtl) expect(rail.x - (tray.x + tray.width)).toBeLessThan(24);
  else expect(tray.x - (rail.x + rail.width)).toBeLessThan(24);
  expect(tray.y + tray.height).toBeLessThanOrEqual(900);
  const activity = (await page.getByTestId("nav-jobs").boundingBox())!;
  expect(
    Math.abs(tray.y - activity.y),
    "the tray opens level with the item that opened it",
  ).toBeLessThanOrEqual(12);
});

test("Ctrl K opens the palette, Esc closes it and returns focus, and the search item opens it too", async ({
  page,
}) => {
  await boot(page);
  await page.getByTestId("nav-library").focus();
  await page.keyboard.press("Control+k");
  const input = page.getByTestId("command-palette-input");
  await expect(input).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(input).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("command-palette")).toHaveCount(0);
  await expect(page.getByTestId("nav-library")).toBeFocused();

  await page.getByTestId("nav-search").click();
  await expect(input).toBeFocused();
  await page.keyboard.press("Control+k");
  await expect(page.getByTestId("command-palette")).toHaveCount(0);
});

test("Ctrl K stays out of text fields and editable regions", async ({
  page,
}) => {
  await boot(page);
  for (const tag of ["input", "textarea", "div"]) {
    await page.evaluate((name) => {
      const el = document.createElement(name);
      el.setAttribute("data-testid", "probe-field");
      if (name === "div") el.contentEditable = "true";
      document.querySelector("#main-content")?.appendChild(el);
    }, tag);
    const probe = page.getByTestId("probe-field");
    await probe.focus();
    await page.keyboard.press("Control+k");
    await expect(page.getByTestId("command-palette")).toHaveCount(0);
    await probe.evaluate((el) => el.remove());
  }
});

test("a typed query opens a book; arrows move the choice and wrap", async ({
  page,
}) => {
  await boot(page);
  await page.keyboard.press("Control+k");
  await page.keyboard.type("bronte");
  const jane = page.getByTestId("palette-book:book-jane");
  await expect(jane).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("ArrowUp");
  await expect(jane).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("command-palette")).toHaveCount(0);
  await expect(page.getByTestId("reading-view")).toBeVisible();
});

test("an unmatched query says so", async ({ page }) => {
  await boot(page);
  await page.keyboard.press("Control+k");
  await page.keyboard.type("zzqx");
  await expect(page.getByTestId("command-palette-empty")).toContainText(
    msg("palette.emptyHint"),
  );
});

test("Go to Settings and the tutor action reach their destinations", async ({
  page,
}) => {
  await boot(page);
  await page.keyboard.press("Control+k");
  await page.getByTestId("palette-go:settings").click();
  await expect(page.getByTestId("nav-settings")).toHaveAttribute(
    "aria-current",
    "page",
  );
  await page.keyboard.press("Control+k");
  await page.getByTestId("palette-action:tutor").click();
  await expect(page.getByTestId("tutor-panel")).toBeVisible();
});

test("the playing cast's chapters appear and one plays from its start", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await expect
    .poll(async () => (await audioState(page)).duration)
    .toBeGreaterThan(8);
  await page.keyboard.press("Control+k");
  const chapters = page.locator('[data-testid^="palette-chapter:"]');
  await expect(chapters).toHaveCount(2);
  await chapters.nth(1).click();
  await expect
    .poll(async () => (await audioState(page)).currentTime)
    .toBeGreaterThanOrEqual(3.4);
  await enterReading(page);
});

const otherCast = {
  id: "export-other",
  bookId: "book-other",
  jobId: null,
  path: "/exports/Other Book - Ch 1.mp3",
  bytes: 4_000_000,
  format: "mp3",
  mode: "narrate",
  stale: false,
  createdAt: 1_755_000_000_000,
};

test("playing a cast from the palette while a page-only Book is open leaves a visible player", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await installTauriMock(page, {
    books: [
      fixtureBook(),
      fixtureBook({ id: "book-other", title: "Other Book" }),
    ],
    seededExports: [otherCast],
    readingScenario: { mediaDataUrl: wavDataUrl() },
  });
  await page.goto("/");
  await expect(page.getByTestId("launch-intro")).toHaveCount(0);
  await page.keyboard.press("Control+k");
  await page.getByTestId("palette-book:book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await page.keyboard.press("Control+k");
  await page.getByTestId("palette-cast:export-other").click();
  await expect.poll(async () => (await audioState(page)).paused).toBe(false);
  await expect(page.getByTestId("player-toggle").first()).toBeVisible();
});

test("the tutor action inside a Book in Focus mode reveals the Book's tutor", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await openReading(page);
  await play(page);
  await enterReading(page);
  await page.getByTestId("focus-toggle").click();
  await expect(page.getByTestId("study-companion")).toHaveCount(0);
  await page.keyboard.press("Control+k");
  await page.getByTestId("palette-action:tutor").click();
  await expect(page.getByTestId("study-companion")).toBeVisible();
  await expect(page.getByTestId("companion-tab-tutor")).toHaveAttribute(
    "aria-selected",
    "true",
  );
});

test("Ctrl K stays shut over a modal dialog, and opening it closes the Activity tray first", async ({
  page,
}) => {
  await boot(page);
  await page.getByTestId("nav-jobs").click();
  await expect(page.getByTestId("activity-tray")).toBeVisible();
  await page.keyboard.press("Control+k");
  await expect(page.getByTestId("activity-tray")).toHaveCount(0);
  await expect(page.getByTestId("command-palette-input")).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("command-palette")).toHaveCount(0);

  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await expect(page.getByTestId("job-modal")).toBeVisible();
  await page.keyboard.press("Control+k");
  await expect(page.getByTestId("command-palette")).toHaveCount(0);
});

test("a Book with no pages but a cast to follow opens that cast from the palette", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await installTauriMock(page, {
    books: [fixtureBook({ pageCount: 0 })],
    seededExports: [
      {
        ...otherCast,
        id: "export-follow",
        bookId: "book-fixture",
        jobId: "job-1",
      },
    ],
    readingScenario: { mediaDataUrl: wavDataUrl() },
    jobScenario: { chapters: fixtureChapters(), stageDelayMs: 10 },
  });
  await page.goto("/");
  await expect(page.getByTestId("launch-intro")).toHaveCount(0);
  await page.keyboard.press("Control+k");
  await page.getByTestId("palette-book:book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
});

test("the rail marker follows Settings when the window height changes", async ({
  page,
}) => {
  await boot(page);
  await page.getByTestId("nav-settings").click();
  const marker = page.getByTestId("rail-marker");
  await page.setViewportSize({ width: 1440, height: 600 });
  await expect
    .poll(async () => {
      const m = (await marker.boundingBox())!;
      const s = (await page.getByTestId("nav-settings").boundingBox())!;
      return Math.abs(m.y - s.y);
    })
    .toBeLessThanOrEqual(1);
});

test("a near rail move glides the same marker, a far jump re-creates it in place", async ({
  page,
}) => {
  await boot(page);
  await page.getByTestId("nav-library").click();
  await page.evaluate(() => {
    (window as unknown as { __marker?: Element | null }).__marker =
      document.querySelector('[data-testid="rail-marker"]');
  });
  await page.getByTestId("nav-listening").click();
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __marker?: Element | null }).__marker ===
        document.querySelector('[data-testid="rail-marker"]'),
    ),
    "a neighbouring item glides the same marker",
  ).toBe(true);
  await page.getByTestId("nav-settings").click();
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __marker?: Element | null }).__marker ===
        document.querySelector('[data-testid="rail-marker"]'),
    ),
    "a far jump does not sweep the old marker across the rail",
  ).toBe(false);
  await expect(page.getByTestId("rail-marker")).toHaveAttribute(
    "data-jumps",
    "1",
  );
});

test("switching Settings sections keeps the frame still and changes only the content", async ({
  page,
}) => {
  await boot(page);
  await page.getByTestId("nav-settings").click();
  await expect(page.getByTestId("settings-frame")).toBeVisible();
  await page.evaluate(() => {
    (window as unknown as { __frame?: Element | null }).__frame =
      document.querySelector('[data-testid="settings-frame"]');
  });
  await page.getByTestId("nav-models").click();
  await expect(page.getByTestId("models-panel")).toBeVisible();
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __frame?: Element | null }).__frame ===
        document.querySelector('[data-testid="settings-frame"]'),
    ),
  ).toBe(true);
});
