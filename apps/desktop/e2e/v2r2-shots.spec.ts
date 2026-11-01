import { test, type Page } from "@playwright/test";

test.setTimeout(120_000);

test.skip(
  process.env.V2R2_SHOTS !== "1",
  "v2r2 capture rig — writes docs/design/v2r2-shots; set V2R2_SHOTS=1 to regenerate deliberately",
);

const OUT = "../../docs/design/v2r2-shots";

const SCREENS: { key: string; widths: number[] }[] = [
  { key: "resume", widths: [1440] },
  { key: "home", widths: [1440, 800] },
  { key: "study", widths: [1440, 800] },
  { key: "focus", widths: [1440] },
  { key: "rtl", widths: [1440] },
  { key: "library", widths: [1440, 800] },
  { key: "tray", widths: [1440] },
  { key: "production", widths: [1440] },
];

async function capture(
  page: Page,
  key: string,
  theme: "dark" | "light",
  width: number,
) {
  await page.setViewportSize({ width, height: 900 });
  await page.goto(`/?v2r2=${key}&theme=${theme}`);
  await page.waitForSelector("[data-testid^='v2r2-']");
  await page.waitForTimeout(600);
  await page.screenshot({ path: `${OUT}/${key}-${theme}-${width}.png` });
}

test("v2r2 screens", async ({ page }) => {
  for (const screen of SCREENS) {
    for (const width of screen.widths) {
      await capture(page, screen.key, "dark", width);
      if (width === 1440) await capture(page, screen.key, "light", width);
    }
  }
});
