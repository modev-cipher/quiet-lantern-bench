import { expect, test } from "@playwright/test";

import { msg } from "./i18nText";
import { advanceLanSend, fixtureBook, installTauriMock } from "./tauriMock";

const SEEDED_EXPORTS = [
  {
    id: "export-lan-1",
    bookId: "book-fixture",
    jobId: "job-lan-1",
    path: "/exports/Fixture Book - full - narrate.mp3",
    bytes: 5_000_000,
    format: "mp3",
    mode: "narrate",
    stale: false,
    createdAt: 1_755_000_000_000,
  },
  {
    id: "export-lan-2",
    bookId: "book-fixture",
    jobId: "job-lan-2",
    path: "/exports/Fixture Book - chapter 1 - narrate.mp3",
    bytes: 1_000_000,
    format: "mp3",
    mode: "narrate",
    stale: false,
    createdAt: 1_755_100_000_000,
  },
];

async function openSendModal(
  page: import("@playwright/test").Page,
  hostLocale?: string,
) {
  await installTauriMock(page, {
    hostLocale,
    books: [fixtureBook()],
    seededExports: SEEDED_EXPORTS,
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("nav-audio").click();
  await page.getByTestId("send-to-phone").click();
  await expect(page.getByTestId("send-to-phone-modal")).toBeVisible();
}

test("QR pairing shows progress rows and finishes honestly", async ({
  page,
}) => {
  await openSendModal(page);

  await expect(page.getByTestId("lan-send-pick-export-lan-1")).toBeVisible();
  await expect(page.getByTestId("lan-send-pick-export-lan-2")).toBeVisible();

  await page.getByTestId("lan-send-show-qr").click();
  await expect(page.getByTestId("lan-send-qr").locator("svg")).toBeVisible();
  await expect(page.getByText(msg("lan.scan"))).toBeVisible();
  await expect(
    page.getByText(msg("lan.safety", { code: "aaaaaaaa" })),
  ).toBeVisible();
  await expect(page.getByTestId("lan-send-status")).toHaveText(
    msg("lan.waiting"),
  );

  await advanceLanSend(page, "sending");
  await expect(page.getByTestId("lan-send-status")).toHaveText(
    msg("lan.sending"),
  );
  await expect(
    page.getByTestId("lan-send-progress-export-lan-1"),
  ).toContainText("50%");
  await expect(
    page.getByTestId("lan-send-progress-export-lan-2"),
  ).toBeVisible();

  await advanceLanSend(page, "done");
  await expect(page.getByTestId("lan-send-done")).toHaveText(
    msg("lan.done", { count: 2 }),
  );
});

test("stopping mid-transfer asks before killing the session", async ({
  page,
}) => {
  await openSendModal(page);
  await page.getByTestId("lan-send-show-qr").click();
  await advanceLanSend(page, "sending");

  await page.getByTestId("lan-send-stop").click();
  const confirm = page.getByTestId("lan-send-stop-confirm");
  await expect(confirm).toContainText(msg("lan.stopConfirm"));
  await page.getByTestId("lan-send-keep").click();
  await expect(confirm).toHaveCount(0);
  await expect(page.getByTestId("send-to-phone-modal")).toBeVisible();

  await page.getByTestId("lan-send-stop").click();
  await page.getByTestId("lan-send-stop-confirmed").click();
  await expect(page.getByTestId("send-to-phone-modal")).toHaveCount(0);
});

test("a small window keeps the QR scannable and the footer reachable", async ({
  page,
}) => {
  await page.setViewportSize({ width: 900, height: 620 });
  await openSendModal(page);
  await page.getByTestId("lan-send-show-qr").click();

  const qr = page.getByTestId("lan-send-qr");
  await expect(qr).toBeVisible();
  const box = await qr.boundingBox();
  expect(box).not.toBeNull();
  expect(box!.width).toBeGreaterThanOrEqual(200);

  const stop = page.getByTestId("lan-send-stop");
  await expect(stop).toBeVisible();
  await stop.click();
  await expect(page.getByTestId("send-to-phone-modal")).toHaveCount(0);
});

test("Arabic skipped-file recovery fits the dialog with a long mixed-script title", async ({
  page,
}) => {
  await page.setViewportSize({ width: 900, height: 620 });
  await openSendModal(page, "ar");
  await page.getByTestId("lan-send-show-qr").click();
  await expect(page.getByTestId("lan-send-qr")).toBeVisible();
  await page.evaluate(() => {
    (
      window as unknown as {
        __mockEmitEvent: (name: string, payload: unknown) => void;
      }
    ).__mockEmitEvent("lan-send-progress", {
      phase: "sending",
      pairedDevices: 1,
      acceptedConnections: 1,
      pairLocked: false,
      deviceNames: ["Phone"],
      casts: [
        {
          castId: "missing",
          title: "API_كتاب_".repeat(40),
          bytesSent: 0,
          bytesTotal: 0,
          sentComplete: false,
          missing: true,
        },
      ],
    });
  });
  const row = page.getByTestId("lan-send-progress-missing");
  await expect(row).toContainText("تم تخطّيه");
  expect(
    await row.evaluate((el) => el.scrollWidth - el.clientWidth),
  ).toBeLessThanOrEqual(1);
  await expect(page.getByTestId("lan-send-stop")).toBeInViewport();
});
