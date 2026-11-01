import { expect, test, type Page } from "@playwright/test";

import { fixtureBook, installTauriMock } from "./tauriMock";

// AT-2-1 (round-2 QA, S2): from the BookDirect reader — a Talk entry on a
// book with rendered pages and no cast — the Book tab's "Open in Library"
// must SHOW the Library. The reader is a fixed full-screen overlay App owns;
// an explicit route request has to dismiss it, or the destination stays
// covered (mouse and keyboard alike). The Book tab shows the metadata panel
// (agent commands absent, as in the native replay). Locale-agnostic: testids
// only, so the same spec runs under I18N_E2E_LOCALE=ar.

async function openBookDirectBookTab(page: Page) {
  await installTauriMock(page, {
    books: [fixtureBook()],
    bookAgentProbe: "notInBuild",
  });
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await page.getByTestId("companion-tab-book").click();
  await expect(page.getByTestId("companion-open-library")).toBeVisible();
}

async function expectLibraryShown(page: Page) {
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
  await expect(page.getByTestId("library")).toBeVisible();
  await expect(page.getByTestId("talk-book-book-fixture")).toBeVisible();
  await expect(page.getByTestId("nav-library")).toHaveAttribute(
    "aria-current",
    "page",
  );
}

test("Open in Library from the BookDirect reader shows the Library (mouse)", async ({
  page,
}) => {
  await openBookDirectBookTab(page);
  await page.getByTestId("companion-open-library").click();
  await expectLibraryShown(page);
});

test("Open in Library from the BookDirect reader shows the Library (keyboard)", async ({
  page,
}) => {
  await openBookDirectBookTab(page);
  await page.getByTestId("companion-open-library").focus();
  await page.keyboard.press("Enter");
  await expectLibraryShown(page);
});

test("the reader can be reopened after Open in Library, with no stale surface", async ({
  page,
}) => {
  await openBookDirectBookTab(page);
  await page.getByTestId("companion-open-library").click();
  await expectLibraryShown(page);
  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await page.getByTestId("reading-close").click();
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
  await expect(page.getByTestId("library")).toBeVisible();
});
