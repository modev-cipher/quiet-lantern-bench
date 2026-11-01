import { describe, expect, it } from "vitest";
import {
  sortUncertainFirst,
  suppressedDetailLabel,
  suppressedSummaryLabel,
  type SuppressedBlock,
} from "./readingSuppressedModel";

function record(overrides: Partial<SuppressedBlock>): SuppressedBlock {
  return {
    id: "sb-1",
    jobId: "job-1",
    pageNo: 1,
    role: "header",
    reason: "repetition",
    certainty: "certain",
    text: "RUNNING HEAD",
    words: 2,
    ...overrides,
  };
}

describe("suppressedSummaryLabel", () => {
  it("returns null for zero records so no empty affordance renders", () => {
    expect(suppressedSummaryLabel(0)).toBeNull();
  });

  it("uses singular copy for one record", () => {
    expect(suppressedSummaryLabel(1)).toBe(
      "\u20661\u2069 block removed from narration",
    );
  });

  it("uses plural copy above one", () => {
    expect(suppressedSummaryLabel(3)).toBe(
      "\u20663\u2069 blocks removed from narration",
    );
  });
});

describe("sortUncertainFirst", () => {
  it("puts uncertain records ahead of certain ones regardless of page order", () => {
    const ordered = sortUncertainFirst([
      record({ id: "a", pageNo: 1, certainty: "certain" }),
      record({ id: "b", pageNo: 9, certainty: "uncertain" }),
      record({ id: "c", pageNo: 4, certainty: "certain" }),
    ]);
    expect(ordered.map((entry) => entry.id)).toEqual(["b", "a", "c"]);
  });

  it("orders by page within a certainty class and does not mutate its input", () => {
    const input = [
      record({ id: "a", pageNo: 8, certainty: "uncertain" }),
      record({ id: "b", pageNo: 2, certainty: "uncertain" }),
    ];
    const ordered = sortUncertainFirst(input);
    expect(ordered.map((entry) => entry.id)).toEqual(["b", "a"]);
    expect(input.map((entry) => entry.id)).toEqual(["a", "b"]);
  });
});

describe("suppressedDetailLabel", () => {
  it("names page, role and a spelled-out reason", () => {
    expect(suppressedDetailLabel(record({ pageNo: 7 }))).toBe(
      "p. 7 · header · repeated across pages",
    );
  });

  it("spells out the layout-label reason", () => {
    expect(
      suppressedDetailLabel(record({ reason: "layout-label", role: "footer" })),
    ).toBe("p. 1 · footer · the layout model read it as page furniture");
  });

  it("spells out the artifact-mark reason", () => {
    expect(
      suppressedDetailLabel(record({ reason: "artifact-mark", role: "header" })),
    ).toBe("p. 1 · header · the document marks it as page furniture");
  });

  it("passes an unknown reason through verbatim rather than hiding it", () => {
    expect(suppressedDetailLabel(record({ reason: "new-reason" }))).toBe(
      "p. 1 · header · new-reason",
    );
  });
});
