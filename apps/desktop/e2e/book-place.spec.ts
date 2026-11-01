import { expect, test, type Page } from "@playwright/test";

import { msg } from "./i18nText";
import { wavDataUrl } from "./pcmFixture";
import { fixtureBook, fixtureChapters, installTauriMock } from "./tauriMock";

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

async function bootWithSession(page: Page) {
  await installTauriMock(page, {
    books: [fixtureBook()],
    seededExports: [seeded],
    readingScenario: { mediaDataUrl: wavDataUrl() },
    bookAgentProbe: "notInBuild",
  });
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
  await page.goto("/");
}

async function storedPlace(page: Page): Promise<string | null> {
  return page.evaluate(() => localStorage.getItem("txtcaster.lastPlace"));
}

async function audioPaused(page: Page): Promise<boolean> {
  return page.evaluate(() => document.querySelector("audio")?.paused ?? true);
}

test("the Book's back path names the Shelf and lands there whatever was underneath", async ({
  page,
}) => {
  await bootWithSession(page);
  await page.getByTestId("nav-audio").click();
  await page.getByTestId("cover-quick-toggle-export-seeded").click();
  await expect(page.getByTestId("mini-player")).toBeVisible();
  await page.getByTestId("nav-settings").click();
  await expect(page.getByTestId("appearance-settings")).toBeVisible();
  await page.getByTestId("mini-player-follow-along").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();

  const back = page.getByTestId("reading-close");
  await expect(back).toContainText(msg("nav.shelf"));
  await back.click();
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
  await expect(page.getByTestId("library")).toBeVisible();
  await expect(page.getByTestId("nav-library")).toHaveAttribute(
    "aria-current",
    "page",
  );
});

test("launch restores the Book that was open, paused", async ({ page }) => {
  await bootWithSession(page);
  await page.getByTestId("study-resume").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  expect(await storedPlace(page)).toContain("export-seeded");

  await page.reload();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(page.getByTestId("reading-header")).toContainText(
    fixtureBook().title,
  );
  expect(await audioPaused(page), "nothing plays by itself on launch").toBe(
    true,
  );
});

test("going back to the Shelf forgets the Book: the next launch opens on the Shelf", async ({
  page,
}) => {
  await bootWithSession(page);
  await page.getByTestId("study-resume").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await page.getByTestId("reading-close").click();
  await expect(page.getByTestId("library")).toBeVisible();
  expect(await storedPlace(page)).toBeNull();

  await page.reload();
  await expect(page.getByTestId("library")).toBeVisible();
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
});

test("a remembered Book that no longer exists falls back to the Shelf and is forgotten", async ({
  page,
}) => {
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.addInitScript(() => {
    if (sessionStorage.getItem("seeded-place") != null) return;
    sessionStorage.setItem("seeded-place", "1");
    localStorage.setItem(
      "txtcaster.lastPlace",
      JSON.stringify({ kind: "pages", bookId: "book-that-was-deleted" }),
    );
  });
  await page.goto("/");
  await expect(page.getByTestId("library")).toBeVisible();
  await expect(page.getByTestId("book-card")).toHaveCount(1);
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
  await expect.poll(() => storedPlace(page)).toBeNull();
});

test("a corrupt remembered place is ignored", async ({ page }) => {
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.addInitScript(() => {
    localStorage.setItem("txtcaster.lastPlace", "{not json");
  });
  await page.goto("/");
  await expect(page.getByTestId("library")).toBeVisible();
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
});

test("a book opened on its pages is restored on its pages, without starting a tutor session", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    bookAgentProbe: "notInBuild",
  });
  await page.goto("/");
  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  expect(await storedPlace(page)).toBe(
    JSON.stringify({ kind: "pages", bookId: "book-fixture" }),
  );

  await page.reload();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  const starts = await page.evaluate(
    () =>
      (
        ((window as unknown as Record<string, unknown>).__invokedCommands ??
          []) as string[]
      ).filter((command) => command === "voice_session_start").length,
  );
  expect(starts).toBe(0);
});

async function bootWithHeldRestore(page: Page) {
  await installTauriMock(page, {
    books: [fixtureBook()],
    bookAgentProbe: "notInBuild",
  });
  await page.addInitScript(() => {
    localStorage.setItem(
      "txtcaster.lastPlace",
      JSON.stringify({ kind: "pages", bookId: "book-fixture" }),
    );
    const scope = window as unknown as Record<string, unknown>;
    const internals = scope.__TAURI_INTERNALS__ as {
      invoke: (cmd: string, args?: unknown) => Promise<unknown>;
    };
    const original = internals.invoke.bind(internals);
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    scope.__releaseExports = release;
    scope.__exportsSettled = false;
    internals.invoke = async (cmd, args) => {
      if (cmd !== "list_exports") return original(cmd, args);
      await held;
      const result = await original(cmd, args);
      setTimeout(() => {
        scope.__exportsSettled = true;
      }, 50);
      return result;
    };
  });
  await page.goto("/");
}

async function releaseRestore(page: Page) {
  await page.evaluate(() =>
    (
      (window as unknown as Record<string, unknown>)
        .__releaseExports as () => void
    )(),
  );
  await page.waitForFunction(
    () =>
      (window as unknown as Record<string, unknown>).__exportsSettled === true,
  );
}

test("a restore that resolves after the user has moved on does not reopen the Book", async ({
  page,
}) => {
  await bootWithHeldRestore(page);
  await page.getByTestId("nav-settings").click();
  await expect(page.getByTestId("appearance-settings")).toBeVisible();

  await releaseRestore(page);
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
  await expect(page.getByTestId("appearance-settings")).toBeVisible();
});

test("a restore that resolves after the user opened the tutor leaves the tutor open", async ({
  page,
}) => {
  await bootWithHeldRestore(page);
  await page.getByTestId("tutor-launcher").click();
  await expect(page.getByTestId("tutor-panel")).toBeVisible();

  await releaseRestore(page);
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
  await expect(page.getByTestId("tutor-panel")).toBeVisible();
});

test("Activity and Settings are reachable from inside a Book", async ({
  page,
}) => {
  await bootWithSession(page);
  await page.getByTestId("study-resume").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();

  await page.getByTestId("book-activity").click();
  const tray = page.getByTestId("activity-tray");
  await expect(tray).toBeVisible();
  const onTop = await tray.evaluate((element) => {
    const box = element.getBoundingClientRect();
    const hit = document.elementFromPoint(
      box.left + box.width / 2,
      box.top + 12,
    );
    return hit != null && element.contains(hit);
  });
  expect(onTop, "the tray is drawn above the Book, not under it").toBe(true);
  await page.keyboard.press("Escape");
  await expect(tray).toHaveCount(0);
  await expect(page.getByTestId("reading-view")).toBeVisible();

  await page.getByTestId("book-settings").click();
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
  await expect(page.getByTestId("appearance-settings")).toBeVisible();
});

test("a book's cover opens the Book: its pages when it has no audio, with no tutor session started", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    bookAgentProbe: "notInBuild",
  });
  await page.goto("/");
  const cover = page.getByTestId("open-book-book-fixture");
  await expect(cover).toHaveAccessibleName(/Fixture Book Title/);
  await cover.click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(page.getByTestId("reading-header")).toContainText(
    fixtureBook().title,
  );
  const starts = await page.evaluate(
    () =>
      (
        ((window as unknown as Record<string, unknown>).__invokedCommands ??
          []) as string[]
      ).filter((command) => command === "voice_session_start").length,
  );
  expect(starts).toBe(0);
  await page.getByTestId("reading-close").click();
  await expect(page.getByTestId("library")).toBeVisible();
});

test("the cover of a book with audio opens its session, like Study", async ({
  page,
}) => {
  await bootWithSession(page);
  await page.getByTestId("open-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  expect(await storedPlace(page)).toContain("export-seeded");
});

test("a book with nothing to open has a cover that is not a button", async ({
  page,
}) => {
  await installTauriMock(page, { books: [fixtureBook({ pageCount: 0 })] });
  await page.goto("/");
  await expect(page.getByTestId("book-card")).toHaveCount(1);
  await expect(page.getByTestId("open-book-book-fixture")).toHaveCount(0);
});

test("inside a Book with no audio, Cast this book opens the cast dialog for it", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    bookAgentProbe: "notInBuild",
  });
  await page.goto("/");
  await page.getByTestId("open-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(page.getByTestId("book-cast-notice")).toBeVisible();

  await page.getByTestId("book-cast-action").click();
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
  const dialog = page.getByTestId("job-modal");
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText(fixtureBook().title);
});

test("a restored page-only Book still offers to cast it", async ({ page }) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    bookAgentProbe: "notInBuild",
  });
  await page.goto("/");
  await page.getByTestId("open-book-book-fixture").click();
  await expect(page.getByTestId("book-cast-notice")).toBeVisible();
  await page.reload();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(page.getByTestId("book-cast-notice")).toBeVisible();
});

test("a Book that has audio never shows the cast notice", async ({ page }) => {
  await bootWithSession(page);
  await page.getByTestId("study-resume").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(page.getByTestId("book-cast-notice")).toHaveCount(0);
  await page.getByTestId("reading-close").click();

  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(
    page.getByTestId("book-cast-notice"),
    "Talk opens the pages of a book that already has a cast; offering to cast it again would be wrong",
  ).toHaveCount(0);
});

test("Talk into a Book with no audio offers to cast it, like the cover does", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    bookAgentProbe: "notInBuild",
  });
  await page.goto("/");
  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(page.getByTestId("book-cast-notice")).toBeVisible();
});

test("once its cast exists, the Book stops offering to cast it, whichever way it is entered", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    bookAgentProbe: "notInBuild",
    jobScenario: { chapters: fixtureChapters(), stageDelayMs: 10 },
  });
  await page.goto("/");
  await page.getByTestId("open-book-book-fixture").click();
  await expect(page.getByTestId("book-cast-notice")).toBeVisible();
  await page.getByTestId("book-cast-action").click();
  await expect(page.getByTestId("job-modal")).toBeVisible();
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-modal")).not.toBeVisible();
  await expect
    .poll(
      () =>
        page.evaluate(async () => {
          const internals = (window as unknown as Record<string, unknown>)
            .__TAURI_INTERNALS__ as {
            invoke: (cmd: string) => Promise<unknown[]>;
          };
          return (await internals.invoke("list_exports")).length;
        }),
      { timeout: 15_000 },
    )
    .toBeGreaterThan(0);

  await page.getByTestId("nav-library").click();
  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(page.getByTestId("book-cast-notice")).toHaveCount(0);
});

test("a cover whose only audio cannot be followed opens the Book's pages", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    seededExports: [{ ...seeded, id: "export-jobless", jobId: null }],
    readingScenario: { mediaDataUrl: wavDataUrl() },
    bookAgentProbe: "notInBuild",
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("open-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(page.getByTestId("now-playing-view")).toHaveCount(0);
});

test("a cover-open that resolves after the user has moved on does not open the Book", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    bookAgentProbe: "notInBuild",
  });
  await page.addInitScript(() => {
    const scope = window as unknown as Record<string, unknown>;
    const internals = scope.__TAURI_INTERNALS__ as {
      invoke: (cmd: string, args?: unknown) => Promise<unknown>;
    };
    const original = internals.invoke.bind(internals);
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    scope.__holdBooks = false;
    scope.__releaseBooks = release;
    scope.__booksSettled = false;
    internals.invoke = async (cmd, args) => {
      if (cmd !== "list_books" || scope.__holdBooks !== true) {
        return original(cmd, args);
      }
      await held;
      const result = await original(cmd, args);
      setTimeout(() => {
        scope.__booksSettled = true;
      }, 50);
      return result;
    };
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await expect(page.getByTestId("open-book-book-fixture")).toBeVisible();
  await page.evaluate(() => {
    (window as unknown as Record<string, unknown>).__holdBooks = true;
  });
  await page.getByTestId("open-book-book-fixture").click();
  await page.getByTestId("nav-settings").click();
  await expect(page.getByTestId("appearance-settings")).toBeVisible();

  await page.evaluate(() =>
    (
      (window as unknown as Record<string, unknown>)
        .__releaseBooks as () => void
    )(),
  );
  await page.waitForFunction(
    () =>
      (window as unknown as Record<string, unknown>).__booksSettled === true,
  );
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
  await expect(page.getByTestId("appearance-settings")).toBeVisible();
});
