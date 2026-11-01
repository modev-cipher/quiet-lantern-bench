// @vitest-environment jsdom
import { createElement } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import ReadingScript from "./ReadingScript";
import type { ReadingSegment } from "./readingModel";

const segment: ReadingSegment = {
  segmentId: "s1",
  idx: 0,
  pageNo: 1,
  kind: "text",
  figureId: null,
  text: "API يشرح 12 خطوة.",
  startMs: 0,
  endMs: 1000,
  provenance: "fresh",
  anchor: "page",
  rects: [],
  words: [{ w: "API", startMs: 0, endMs: 1000, rect: null }],
};
afterEach(cleanup);

describe("book content direction is independent of the shell", () => {
  it.each([
    ["ar", "ltr", "rtl"],
    ["en", "rtl", "ltr"],
    ["az-Arab", "ltr", "rtl"],
    ["az-Latn", "rtl", "ltr"],
    [null, "rtl", "auto"],
    ["unknown", "rtl", "auto"],
  ] as const)("uses metadata %s in a %s shell", (language, shell, expected) => {
    document.documentElement.dir = shell;
    const view = render(
      createElement(ReadingScript, {
        segment,
        language,
        underlineRef: null,
        renderCount: 1,
        onRepair: null,
      }),
    );
    const text = view.container.querySelector("p")!;
    expect(text.getAttribute("dir")).toBe(expected);
    expect(text.getAttribute("lang")).toBe(expected === "auto" ? "" : language);
    expect(text.textContent).toBe(segment.text);
    expect(document.documentElement.dir).toBe(shell);
    expect(view.container.querySelector("[data-word]")?.textContent).toBe(
      "API",
    );
  });

  it("isolates fallback text without changing the notice's UI direction", () => {
    document.documentElement.dir = "rtl";
    render(
      createElement(ReadingScript, {
        segment: { ...segment, words: [], provenance: "stale" },
        language: "en",
        underlineRef: null,
        renderCount: 1,
        onRepair: () => {},
      }),
    );
    expect(
      screen.getByTestId("reading-fallback-text").getAttribute("dir"),
    ).toBe("ltr");
    expect(screen.getByTestId("reading-fallback-notice").closest("[dir]")).toBe(
      document.documentElement,
    );
  });
});
