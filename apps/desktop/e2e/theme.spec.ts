import { expect, test, type Page } from "@playwright/test";

import { enterReading, openReading, play } from "./readingHarness";
import { installTauriMock } from "./tauriMock";

// The vitest contrast gate proves the token DEFINITIONS clear their WCAG
// tiers; these specs prove the running app actually paints with them — the
// theme resolves before boot, switches live, persists, and the follow-along
// highlight stays legible on the dark page.

const theme = (page: Page) => page.locator("html");

async function invoked(page: Page): Promise<string[]> {
  return page.evaluate(
    () =>
      ((window as unknown as Record<string, unknown>).__invokedCommands ??
        []) as string[],
  );
}

// Contrast of two computed colours as rendered, compositing the word's
// ancestor backgrounds so translucent surfaces are measured as painted.
async function readHighlightContrast(page: Page): Promise<{
  wordOnHighlight: number;
  highlightVsSurface: number;
}> {
  const word = page.locator("[data-word].bg-reading-highlight").first();
  await expect(word).toBeVisible();
  const sample = () =>
    word.evaluate((element) => {
      function channels(value: string): number[] | null {
        const match =
          /^rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)(?:\s*,\s*([\d.]+))?/.exec(
            value,
          );
        if (match == null) return null;
        return [
          Number(match[1]),
          Number(match[2]),
          Number(match[3]),
          match[4] == null ? 1 : Number(match[4]),
        ];
      }
      function surfaceBehind(start: Element | null): number[] {
        const layers: number[][] = [];
        for (let node = start; node != null; node = node.parentElement) {
          const parsed = channels(getComputedStyle(node).backgroundColor);
          if (parsed != null && parsed[3] > 0) {
            layers.push(parsed);
            if (parsed[3] === 1) break;
          }
        }
        let [red, green, blue] = [255, 255, 255];
        for (const [r, g, b, alpha] of layers.reverse()) {
          red = r * alpha + red * (1 - alpha);
          green = g * alpha + green * (1 - alpha);
          blue = b * alpha + blue * (1 - alpha);
        }
        return [red, green, blue];
      }
      function luminance([red, green, blue]: number[]): number {
        const [r, g, b] = [red, green, blue].map((channel) => {
          const v = channel / 255;
          return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
        });
        return 0.2126 * r + 0.7152 * g + 0.0722 * b;
      }
      function ratio(a: number[], b: number[]): number {
        const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
        return (hi + 0.05) / (lo + 0.05);
      }
      const style = getComputedStyle(element);
      const highlight = channels(style.backgroundColor);
      const ink = channels(style.color);
      if (highlight == null || ink == null) {
        throw new Error(
          `highlighted word paints no resolvable colours: bg=${style.backgroundColor} fg=${style.color}`,
        );
      }
      return {
        wordOnHighlight: ratio(ink, highlight),
        highlightVsSurface: ratio(
          highlight,
          surfaceBehind(element.parentElement),
        ),
      };
    });
  let previous = await sample();
  for (let attempt = 0; attempt < 20; attempt += 1) {
    await page.evaluate(
      () =>
        new Promise((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(resolve)),
        ),
    );
    const next = await sample();
    if (
      next.wordOnHighlight === previous.wordOnHighlight &&
      next.highlightVsSurface === previous.highlightVsSurface
    ) {
      return next;
    }
    previous = next;
  }
  return previous;
}

test.describe("system preference decides the boot theme", () => {
  test.use({ colorScheme: "dark" });

  test("a dark OS boots the app dark before anything is stored", async ({
    page,
  }) => {
    await installTauriMock(page, { books: [] });
    await page.goto("/");
    await expect(theme(page)).toHaveAttribute("data-theme", "dark");
    const commands = await invoked(page);
    expect(
      commands,
      "the boot chain reads the stored preference and only then reveals the window",
    ).toContain("get_theme_preference");
    expect(commands).toContain("reveal_main_window");
  });
});

test.describe("light OS", () => {
  test.use({ colorScheme: "light" });

  test("a light OS boots the app light", async ({ page }) => {
    await installTauriMock(page, { books: [] });
    await page.goto("/");
    await expect(theme(page)).toHaveAttribute("data-theme", "light");
  });

  test("the native frame is sent the theme tokens and follows a live switch", async ({
    page,
  }) => {
    const chrome = () =>
      page.evaluate(
        () => (window as unknown as Record<string, unknown>).__windowChrome,
      );
    await installTauriMock(page, { books: [] });
    await page.goto("/");
    await expect(theme(page)).toHaveAttribute("data-theme", "light");
    await expect.poll(chrome).toEqual({
      caption: { red: 239, green: 239, blue: 241 },
      text: { red: 24, green: 25, blue: 28 },
      borderFocused: { red: 64, green: 98, blue: 189 },
      borderUnfocused: { red: 201, green: 204, blue: 210 },
      scheme: "system",
    });

    await page.getByTestId("nav-settings").click();
    await page.getByTestId("theme-choice-dark").click();
    await expect(theme(page)).toHaveAttribute("data-theme", "dark");
    await expect.poll(chrome).toEqual({
      caption: { red: 18, green: 18, blue: 20 },
      text: { red: 230, green: 231, blue: 234 },
      borderFocused: { red: 74, green: 102, blue: 176 },
      borderUnfocused: { red: 46, green: 48, blue: 53 },
      scheme: "dark",
    });
  });

  test("the settings toggle switches live and the choice survives a relaunch without the mirror", async ({
    page,
  }) => {
    await installTauriMock(page, { books: [] });
    await page.goto("/");
    await page.getByTestId("nav-settings").click();
    await expect(page.getByTestId("appearance-settings")).toBeVisible();

    const canvasBefore = await page.evaluate(
      () => getComputedStyle(document.body).backgroundColor,
    );
    await page.getByTestId("theme-choice-dark").click();
    await expect(theme(page)).toHaveAttribute("data-theme", "dark");
    await expect(page.getByTestId("theme-choice-dark")).toHaveAttribute(
      "aria-checked",
      "true",
    );
    const canvasAfter = await page.evaluate(
      () => getComputedStyle(document.body).backgroundColor,
    );
    expect(
      canvasAfter,
      "the canvas must actually repaint, not just the attribute",
    ).not.toBe(canvasBefore);

    // A relaunch that lost localStorage still owes the user their theme:
    // the database preference (sessionStorage in the mock) is the truth.
    await page.evaluate(() => localStorage.clear());
    await page.reload();
    await expect(theme(page)).toHaveAttribute("data-theme", "dark");
  });

  test("under system the theme follows a live OS switch; an explicit choice does not", async ({
    page,
  }) => {
    await installTauriMock(page, { books: [] });
    await page.goto("/");
    await expect(theme(page)).toHaveAttribute("data-theme", "light");

    await page.emulateMedia({ colorScheme: "dark" });
    await expect(theme(page)).toHaveAttribute("data-theme", "dark");

    await page.getByTestId("nav-settings").click();
    await page.getByTestId("theme-choice-light").click();
    await expect(theme(page)).toHaveAttribute("data-theme", "light");
    await page.emulateMedia({ colorScheme: "light" });
    await page.emulateMedia({ colorScheme: "dark" });
    await expect(
      theme(page),
      "an explicit preference must not follow the OS",
    ).toHaveAttribute("data-theme", "light");
  });
});

test.describe("the follow-along highlight in dark", () => {
  test.use({ colorScheme: "dark" });

  test("the spoken word stays readable and the highlight stays visible on the dark surface", async ({
    page,
  }) => {
    await openReading(page);
    await play(page);
    await enterReading(page);
    await expect(theme(page)).toHaveAttribute("data-theme", "dark");

    const contrast = await readHighlightContrast(page);
    expect(
      contrast.wordOnHighlight,
      "SC 1.4.3: the highlighted word's ink on its highlight block",
    ).toBeGreaterThanOrEqual(4.5);
    expect(
      contrast.highlightVsSurface,
      "the moving highlight block against the dark reading surface is what carries follow-along",
    ).toBeGreaterThanOrEqual(3.0);
  });
});

test.describe("the follow-along highlight in light", () => {
  test.use({ colorScheme: "light" });

  test("the spoken word stays readable on the light highlight too", async ({
    page,
  }) => {
    await openReading(page);
    await play(page);
    await enterReading(page);
    await expect(theme(page)).toHaveAttribute("data-theme", "light");

    const contrast = await readHighlightContrast(page);
    expect(contrast.wordOnHighlight).toBeGreaterThanOrEqual(4.5);
  });
});
