import { expect, test } from "@playwright/test";

import { fixtureBook, installTauriMock } from "./tauriMock";

const IDLE_CONTROLS = [
  "talk-book-book-fixture",
  "create-audio-book-fixture",
  "book-menu-book-fixture",
];

const CONFIRMING_CONTROLS = [
  "confirm-delete-book-book-fixture",
  "cancel-delete-book-book-fixture",
];

interface Edges {
  cardWidth: number;
  cardLeft: number;
  cardRight: number;
  controls: Array<{
    testid: string;
    left: number;
    right: number;
    textRight: number;
  }>;
}

async function measure(
  page: import("@playwright/test").Page,
  testids: string[],
): Promise<Edges> {
  return page.evaluate((testids) => {
    const card = document.querySelector('[data-testid="book-card"]');
    if (card == null) throw new Error("book-card not in the DOM");
    const cardRect = card.getBoundingClientRect();
    const body = card.querySelector(
      '[data-testid="book-card-title"]',
    )?.parentElement;
    if (body == null) throw new Error("book-card body not in the DOM");
    const bodyRect = body.getBoundingClientRect();
    const bodyStyle = getComputedStyle(body);
    return {
      cardWidth: cardRect.width,
      cardLeft: bodyRect.x + parseFloat(bodyStyle.paddingLeft),
      cardRight: bodyRect.right - parseFloat(bodyStyle.paddingRight),
      controls: testids.map((testid) => {
        const el = document.querySelector(`[data-testid="${testid}"]`);
        if (el == null) throw new Error(`${testid} not in the DOM`);
        const rect = el.getBoundingClientRect();
        let textRight = rect.x;
        for (const node of Array.from(el.childNodes)) {
          if (node.nodeType !== Node.TEXT_NODE) continue;
          const range = document.createRange();
          range.selectNodeContents(node);
          for (const r of Array.from(range.getClientRects())) {
            textRight = Math.max(textRight, r.x + r.width);
          }
        }
        return { testid, left: rect.x, right: rect.x + rect.width, textRight };
      }),
    };
  }, testids);
}

async function settle(
  page: import("@playwright/test").Page,
  width: number,
  testids: string[],
): Promise<Edges> {
  await page.setViewportSize({ width, height: 900 });
  await expect
    .poll(
      async () => {
        const first = (await measure(page, testids)).cardWidth;
        await page.waitForTimeout(60);
        const second = (await measure(page, testids)).cardWidth;
        return Math.abs(first - second) < 0.5 ? second : -1;
      },
      { message: `layout must settle after resizing to ${width}` },
    )
    .toBeGreaterThan(0);
  return measure(page, testids);
}

function expectInsideCard(settled: Edges, width: number) {
  for (const control of settled.controls) {
    expect(
      control.right,
      `${control.testid} bleeds past the card body's content edge at viewport ${width} (card ${settled.cardWidth}px) — overflow-hidden clips it there`,
    ).toBeLessThanOrEqual(settled.cardRight + 0.5);
    expect(
      control.textRight,
      `${control.testid}'s label crosses the card body's content edge at viewport ${width} (card ${settled.cardWidth}px)`,
    ).toBeLessThanOrEqual(settled.cardRight + 0.5);
    expect(
      control.left,
      `${control.testid} starts left of the card body's content edge at viewport ${width}`,
    ).toBeGreaterThanOrEqual(settled.cardLeft - 0.5);
  }
}

test("every footer control and its full label stay inside the card at narrow card widths", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await expect(page.getByTestId("book-card")).toBeVisible();

  let sawNarrowCard = false;
  for (const width of [876, 900, 940, 1100]) {
    const settled = await settle(page, width, IDLE_CONTROLS);
    if (settled.cardWidth <= 260) sawNarrowCard = true;
    expectInsideCard(settled, width);
  }
  expect(
    sawNarrowCard,
    "the sweep must include a near-minimum card or it proves nothing about the failing width",
  ).toBe(true);

  await page.setViewportSize({ width: 876, height: 900 });
  await page.getByTestId("book-menu-book-fixture").click();
  await page.getByTestId("delete-book-book-fixture").click();
  await expect(
    page.getByTestId("confirm-delete-book-book-fixture"),
  ).toBeVisible();
  expectInsideCard(await settle(page, 876, CONFIRMING_CONTROLS), 876);
  await page.getByTestId("cancel-delete-book-book-fixture").click();
  await expect(page.getByTestId("book-menu-book-fixture")).toBeVisible();

  await page.getByTestId("book-menu-book-fixture").click();
  await expect(page.getByTestId("book-menu-items-book-fixture")).toBeVisible();
  await expect(
    page.getByTestId("book-menu-item-improve-book-fixture"),
  ).toBeVisible();
});
