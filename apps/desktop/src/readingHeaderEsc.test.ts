import { describe, expect, it } from "vitest";
import { readSource } from "./tests/readSource";

const SOURCE = readSource(new URL("./ReadingHeader.tsx", import.meta.url));

// The standalone <kbd>Esc</kbd> was clipped to a sliver at narrow widths
// (owner report). The hint now lives on the close button, which always
// reserves its own space.
describe("reading header Esc hint", () => {
  it("renders no standalone kbd element that could be clipped", () => {
    expect(SOURCE).not.toContain("<kbd");
  });

  it("carries the Esc hint on the close button instead", () => {
    const close = SOURCE.slice(SOURCE.indexOf('data-testid="reading-close"'));
    expect(close).toContain('title={t("reader.closeEsc")}');
    expect(close).toContain('aria-label={t("reader.closeFollowAlongEsc")}');
  });
});
