import { describe, expect, it } from "vitest";
import { readSource } from "./tests/readSource";

// I18N-2: the companion tab testids used to be `companion-tab-${label.toLowerCase()}`,
// so translating the label to Arabic renamed them (AR geometry rig: tabs "missing",
// click timeout). Testids are machine ids and never derive from copy.
const SOURCE = readSource(new URL("./StudyCompanion.tsx", import.meta.url));

describe("StudyCompanion tab ids are stable across locales", () => {
  it("derives the testid from the tab id, not the translated label", () => {
    expect(SOURCE).not.toMatch(/companion-tab-\$\{label/);
    expect(SOURCE).toContain("data-testid={`companion-tab-${id}`}");
    expect(SOURCE).toContain('id="tutor"');
    expect(SOURCE).toContain('id="book"');
  });
});
