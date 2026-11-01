import { expect, test } from "@playwright/test";
import golden from "./fixtures/reading-timeline.golden.json" with { type: "json" };
import { enterReading, openReading, play } from "./readingHarness";

for (const example of [
  {
    ui: "en",
    language: "ar",
    text: "API يشرح 12 خطوة.",
    direction: "rtl",
    shell: "ltr",
  },
  {
    ui: "ar",
    language: "en",
    text: "API explains 12 steps.",
    direction: "ltr",
    shell: "rtl",
  },
  {
    ui: "ar",
    language: null,
    text: "هذه 12 خطوة.",
    direction: "rtl",
    shell: "rtl",
  },
] as const) {
  test(`book language ${example.language} in ${example.ui} UI preserves text and follow-along`, async ({
    page,
  }) => {
    const reading = structuredClone(golden);
    const first = reading.chapters[0].segments[0];
    first.text = example.text;
    first.words = [{ ...first.words[0], w: example.text.split(" ")[0] }];
    const stale = reading.chapters[0].segments[1];
    stale.text = example.text;
    await openReading(
      page,
      {},
      {},
      { language: example.language },
      { hostLocale: example.ui, reading },
    );
    await play(page);
    await enterReading(page);
    await page.evaluate(() => {
      const audio = document.querySelector("audio")!;
      audio.pause();
      audio.currentTime = 0.25;
    });
    const text = page.getByTestId("reading-script").locator("p");
    await expect(text).toHaveText(example.text);
    await expect(text).toHaveAttribute(
      "dir",
      example.language ? example.direction : "auto",
    );
    await expect(text).toHaveAttribute("lang", example.language ?? "");
    await expect(text).toHaveCSS("direction", example.direction);
    await expect(page.locator("html")).toHaveAttribute("dir", example.shell);
    await expect(page.locator("[data-word]")).toHaveCount(1);
    await page.evaluate((ms) => {
      document.querySelector("audio")!.currentTime = ms / 1000;
    }, stale.startMs + 100);
    const fallback = page.getByTestId("reading-fallback-text");
    await expect(fallback).toHaveText(example.text);
    await expect(fallback).toHaveCSS("direction", example.direction);
    await expect(page.getByTestId("reading-fallback-notice")).toHaveCSS(
      "direction",
      example.shell,
    );
  });
}
