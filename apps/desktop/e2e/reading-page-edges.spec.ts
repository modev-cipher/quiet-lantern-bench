import { expect, test, type Page } from "@playwright/test";

import { e2eLocale } from "./i18nText";
import { audioState, enterReading, openReading, play } from "./readingHarness";

const rtl = e2eLocale === "ar";

async function openPage(page: Page): Promise<void> {
  await page.setViewportSize({ width: 1440, height: 900 });
  await openReading(page);
  await play(page);
  await enterReading(page);
  await page.evaluate(() => {
    const audio = document.querySelector("audio");
    if (audio != null) {
      audio.pause();
      audio.currentTime = 0.25;
    }
  });
  await page.getByTestId("reading-mode-toggle").click();
  await expect(page.getByTestId("reading-surface")).toHaveAttribute(
    "data-mode",
    "page",
  );
}

test("page turns live on the page edges, not in the player bar", async ({
  page,
}) => {
  await openPage(page);
  const edges = page.getByTestId("reading-page-edges");
  const surface = (await page.getByTestId("reading-surface").boundingBox())!;
  const back = (await edges.getByTestId("reading-flip-back").boundingBox())!;
  const forward = (await edges
    .getByTestId("reading-flip-forward")
    .boundingBox())!;
  const surfaceMid = surface.x + surface.width / 2;
  if (rtl) {
    expect(back.x).toBeGreaterThan(surfaceMid);
    expect(forward.x).toBeLessThan(surfaceMid);
  } else {
    expect(back.x).toBeLessThan(surfaceMid);
    expect(forward.x).toBeGreaterThan(surfaceMid);
  }
  await expect(
    page.getByTestId("reading-page-controls").getByRole("button"),
  ).toHaveCount(0);

  await page.getByTestId("reading-flip-forward").click();
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "2",
  );
  await expect(page.getByTestId("reading-return")).toBeVisible();
});

test("the arrow keys turn pages in reading order without moving the audio", async ({
  page,
}) => {
  await openPage(page);
  const facsimile = page.getByTestId("reading-facsimile");
  await expect(facsimile).toHaveAttribute("data-page", "1");
  const before = (await audioState(page)).currentTime;
  await page.getByTestId("reading-surface").focus();
  await page.keyboard.press(rtl ? "ArrowLeft" : "ArrowRight");
  await expect(facsimile).toHaveAttribute("data-page", "2");
  await page.keyboard.press(rtl ? "ArrowRight" : "ArrowLeft");
  await expect(facsimile).toHaveAttribute("data-page", "1");
  expect((await audioState(page)).currentTime).toBe(before);
});

test("arrow keys on the scrubber still seek instead of turning the page", async ({
  page,
}) => {
  await openPage(page);
  await page.getByTestId("player-seek").focus();
  await page.keyboard.press(rtl ? "ArrowLeft" : "ArrowRight");
  await expect
    .poll(async () => (await audioState(page)).currentTime)
    .toBeGreaterThan(4);
  await expect(page.getByTestId("reading-page-controls")).toHaveAttribute(
    "data-browsing",
    "no",
  );
  await expect(page.getByTestId("reading-return")).toHaveCount(0);
});

test("the idle page arrows clear the 3:1 control contrast floor in both themes", async ({
  page,
}) => {
  await openPage(page);
  for (const theme of ["light", "dark"]) {
    await page.evaluate((value) => {
      document.documentElement.dataset.theme = value;
    }, theme);
    await page.mouse.move(0, 0);
    const measure = () =>
      page.getByTestId("reading-flip-forward").evaluate((button) => {
        const parse = (value: string) =>
          (value.match(/[\d.]+/g) ?? []).map(Number);
        const icon = button.querySelector("svg") ?? button;
        const [r, g, b, a = 1] = parse(getComputedStyle(icon).color);
        let opacity = a;
        for (
          let node: Element | null = icon;
          node != null;
          node = node.parentElement
        ) {
          opacity *= Number(getComputedStyle(node).opacity);
        }
        let backdrop = [255, 255, 255];
        for (
          let node: Element | null = button.parentElement;
          node != null;
          node = node.parentElement
        ) {
          const bg = parse(getComputedStyle(node).backgroundColor);
          if (bg.length >= 3 && (bg[3] ?? 1) > 0) {
            backdrop = bg.slice(0, 3);
            break;
          }
        }
        const mixed = [r, g, b].map(
          (channel, at) => channel * opacity + backdrop[at] * (1 - opacity),
        );
        const lum = (rgb: number[]) => {
          const [lr, lg, lb] = rgb.map((v) => {
            const c = v / 255;
            return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
          });
          return 0.2126 * lr + 0.7152 * lg + 0.0722 * lb;
        };
        const [hi, lo] = [lum(mixed), lum(backdrop)].sort((x, y) => y - x);
        return (hi + 0.05) / (lo + 0.05);
      });
    await expect
      .poll(measure, {
        message: `${theme} idle arrow contrast, once the theme's colour transition settles`,
        timeout: 3000,
      })
      .toBeGreaterThanOrEqual(3);
  }
});
