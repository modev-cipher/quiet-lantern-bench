import { expect, test } from "@playwright/test";

import { fixtureBook, installTauriMock } from "./tauriMock";

for (const colorScheme of ["light", "dark"] as const) {
  test.describe(`${colorScheme} navigation hints`, () => {
    test.use({ colorScheme });

    for (const width of [480, 1280]) {
      test(`Settings stays readable above the tutor launcher at ${width}px`, async ({
        page,
      }) => {
        await page.setViewportSize({ width, height: 800 });
        await installTauriMock(page, { books: [] });
        await page.goto("/");
        await expect(page.getByTestId("launch-intro")).toHaveCount(0);
        await expect(page.locator("html")).toHaveAttribute(
          "data-theme",
          colorScheme,
        );

        const settings = page.getByTestId("nav-settings");
        const tip = settings.locator(".rail-tip");
        const launcher = page.getByTestId("tutor-launcher");
        const expectHintAboveLauncher = async () => {
          await expect(tip).toHaveCSS("opacity", "1");
          await expect
            .poll(() =>
              tip.evaluate((element) => {
                const tutor = document.querySelector(
                  '[data-testid="tutor-launcher"]',
                );
                if (tutor == null) throw new Error("tutor launcher missing");
                const hintBox = element.getBoundingClientRect();
                const tutorBox = tutor.getBoundingClientRect();
                const left = Math.max(hintBox.left, tutorBox.left);
                const right = Math.min(hintBox.right, tutorBox.right);
                const top = Math.max(hintBox.top, tutorBox.top);
                const bottom = Math.min(hintBox.bottom, tutorBox.bottom);
                if (left >= right || top >= bottom) return "separate";
                const original = element.style.pointerEvents;
                element.style.pointerEvents = "auto";
                const painted = document.elementFromPoint(
                  (left + right) / 2,
                  (top + bottom) / 2,
                );
                element.style.pointerEvents = original;
                return painted === element ||
                  (painted != null && element.contains(painted))
                  ? "hint"
                  : "occluded";
              }),
            )
            .not.toBe("occluded");
        };

        await settings.hover();
        await expectHintAboveLauncher();
        await page.mouse.move(width / 2, 200);
        await page.getByTestId("nav-report-issue").focus();
        await page.keyboard.press("Tab");
        await expect(settings).toBeFocused();
        await expectHintAboveLauncher();
        await launcher.click();
        await expect(page.getByTestId("tutor-panel")).toBeVisible();
      });
    }
  });
}

test("the tutor lives in a corner launcher, not the rail", async ({ page }) => {
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.goto("/");
  await expect(page.getByTestId("nav-tutor")).toHaveCount(0);
  const launcher = page.getByTestId("tutor-launcher");
  await expect(launcher).toBeVisible();
  await expect(launcher).toHaveAttribute("aria-label", /\S/);
  await page.getByTestId("nav-settings").click();
  await expect(launcher).toBeVisible();
});

test("the tutor pops out, drags, minimizes to the launcher and docks back", async ({
  page,
}) => {
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/");
  const launcher = page.getByTestId("tutor-launcher");
  await launcher.click();
  const panel = page.getByTestId("tutor-panel");
  await expect(panel).toHaveAttribute("data-dock", "docked");

  await page.getByTestId("tutor-panel-float").click();
  await expect(panel).toHaveAttribute("data-dock", "floating");
  await panel.evaluate((el) =>
    Promise.all(el.getAnimations().map((animation) => animation.finished)),
  );
  const before = await panel.boundingBox();
  const handle = page.getByTestId("tutor-panel-drag");
  const grip = await handle.boundingBox();
  if (before == null || grip == null) throw new Error("tutor window missing");
  await page.mouse.move(grip.x + grip.width / 2, grip.y + grip.height / 2);
  await page.mouse.down();
  const rtl = await page.evaluate(
    () => getComputedStyle(document.documentElement).direction === "rtl",
  );
  const shift = rtl ? 200 : -200;
  await page.mouse.move(grip.x + grip.width / 2 + shift, grip.y + 120, {
    steps: 6,
  });
  await page.mouse.up();
  await panel.evaluate((el) =>
    Promise.all(el.getAnimations().map((animation) => animation.finished)),
  );
  const after = await panel.boundingBox();
  expect(Math.abs((after?.x ?? before.x) - before.x)).toBeGreaterThan(100);
  expect(after?.y).toBeGreaterThan(before.y + 80);

  await page.getByTestId("tutor-panel-fold").click();
  await expect(panel).toBeHidden();
  await expect(launcher).toHaveAttribute("aria-expanded", "false");
  await launcher.click();
  await expect(panel).toBeVisible();
  await panel.evaluate((el) =>
    Promise.all(el.getAnimations().map((animation) => animation.finished)),
  );
  const reopened = await panel.boundingBox();
  expect(Math.round(reopened?.x ?? 0)).toBe(Math.round(after?.x ?? -1));

  await page.reload();
  await page.getByTestId("tutor-launcher").click();
  await expect(page.getByTestId("tutor-panel")).toHaveAttribute(
    "data-dock",
    "floating",
  );
  await page.getByTestId("tutor-panel-dock").click();
  await expect(page.getByTestId("tutor-panel")).toHaveAttribute(
    "data-dock",
    "docked",
  );
});

test("a popped-out tutor stays popped out inside a Book", async ({ page }) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    bookAgentProbe: "notInBuild",
  });
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/");
  await page.getByTestId("tutor-launcher").click();
  await page.getByTestId("tutor-panel-float").click();
  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(page.getByTestId("companion-dock")).toBeVisible();
  await page.getByTestId("companion-dock").click();
  await page.getByTestId("reading-close").click();
  await page.getByTestId("tutor-launcher").click();
  await expect(page.getByTestId("tutor-panel")).toHaveAttribute(
    "data-dock",
    "docked",
  );
});

test("minimizing hands focus to the launcher and reopening hands it back", async ({
  page,
}) => {
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/");
  const launcher = page.getByTestId("tutor-launcher");
  const panel = page.getByTestId("tutor-panel");
  await launcher.click();
  await page.getByTestId("tutor-panel-float").click();

  await page.getByTestId("tutor-panel-fold").click();
  await expect(panel).toBeHidden();
  await expect(launcher).toBeFocused();

  await launcher.click();
  await expect(panel).toBeVisible();
  await expect(panel).toBeFocused();

  await page.keyboard.press("Escape");
  await expect(panel).toBeHidden();
  await expect(launcher).toBeFocused();
});
