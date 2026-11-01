
import { expect, test, type Page } from "@playwright/test";

import { enterReading, openReading, play } from "./readingHarness";

const WIDTHS = [1440, 1024, 768] as const;

const CONTROLS = [
  "outline-toggle",
  "reading-origin",
  "reading-fit-width",
  "reading-fit-page",
  "reading-zoom-out",
  "reading-zoom-reset",
  "reading-zoom-in",
  "reading-mode-script",
  "reading-mode-toggle",
  "reading-close",
  "focus-toggle",
] as const;

interface Box {
  id: string;
  x: number;
  y: number;
  width: number;
  height: number;
}

function intersects(a: Box, b: Box): boolean {
  return (
    a.x < b.x + b.width &&
    b.x < a.x + a.width &&
    a.y < b.y + b.height &&
    b.y < a.y + a.height
  );
}

async function headerBoxes(page: Page): Promise<Box[]> {
  const boxes: Box[] = [];
  for (const id of CONTROLS) {
    const locator = page.getByTestId(id);
    if ((await locator.count()) === 0) continue;
    const box = await locator.boundingBox();
    if (box == null) continue;
    boxes.push({ id, ...box });
  }
  return boxes;
}

async function setOutline(page: Page, open: boolean): Promise<void> {
  const toggle = page.getByTestId("outline-toggle");
  const expanded = (await toggle.getAttribute("aria-expanded")) === "true";
  if (expanded !== open) await toggle.click();
  await expect(toggle).toHaveAttribute("aria-expanded", String(open));
}

function describe(boxes: Box[]): string {
  return boxes
    .map(
      (b) =>
        `${b.id}[${Math.round(b.x)}..${Math.round(b.x + b.width)} × ${Math.round(b.y)}..${Math.round(b.y + b.height)}]`,
    )
    .join("\n");
}

test("header controls never overlap across widths and outline states", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await openReading(page);
  await play(page);
  await enterReading(page);
  await page.getByTestId("reading-mode-toggle").click();
  await page.waitForSelector('[data-testid="reading-page-live"]');

  for (const width of WIDTHS) {
    await page.setViewportSize({ width, height: 900 });
    for (const open of [true, false]) {
      await setOutline(page, open);
      await page.waitForTimeout(150);
      const header = await page.getByTestId("reading-header").boundingBox();
      expect(header).not.toBeNull();
      const boxes = await headerBoxes(page);
      const label = `${width}w outline ${open ? "open" : "closed"}\n${describe(boxes)}`;
      for (let i = 0; i < boxes.length; i += 1) {
        for (let j = i + 1; j < boxes.length; j += 1) {
          expect(
            intersects(boxes[i], boxes[j]),
            `${boxes[i].id} intersects ${boxes[j].id} at ${label}`,
          ).toBe(false);
        }
      }
      const chip = boxes.find((b) => b.id === "reading-origin");
      if (chip != null && header != null) {
        expect(
          chip.x + chip.width <= header.x + header.width + 0.5,
          `provenance chip leaves the header at ${label}`,
        ).toBe(true);
      }
      const fitWidth = boxes.find((b) => b.id === "reading-fit-width");
      expect(fitWidth, `fit controls missing at ${label}`).toBeDefined();
      if (fitWidth != null) {
        expect(
          fitWidth.x + fitWidth.width <= width,
          `fit controls leave the viewport at ${label}`,
        ).toBe(true);
      }
    }
  }
});
