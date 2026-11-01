import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

import ReadingHeader from "./ReadingHeader";

function render(): string {
  return renderToStaticMarkup(
    createElement(ReadingHeader, {
      title: "A book",
      chapter: { title: "Chapter one" } as never,
      origin: "measured",
      mode: "page",
      onSelectMode: () => {},
      onClose: () => {},
      compact: true,
    }),
  );
}

describe("the provenance chip is a dev-build affordance (ruling #34)", () => {
  afterEach(() => vi.unstubAllEnvs());

  it("renders no chip and no chip hint in a production build", () => {
    vi.stubEnv("DEV", false);
    const html = render();
    expect(html).not.toContain('data-testid="reading-origin"');
    expect(html).not.toContain("measured from the encoded audio");
    expect(html).toContain('data-testid="reading-chapter"');
  });

  it("renders the chip with its hint in a dev build", () => {
    vi.stubEnv("DEV", true);
    const html = render();
    expect(html).toContain('data-testid="reading-origin"');
    expect(html).toContain("measured from the encoded audio");
  });
});
