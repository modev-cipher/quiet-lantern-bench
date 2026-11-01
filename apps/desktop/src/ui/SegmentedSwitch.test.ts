import { describe, expect, it } from "vitest";

import { nextSegment, type SegmentOption } from "./SegmentedSwitch";

const modes: SegmentOption<"text" | "voice">[] = [
  { value: "text", label: "Type" },
  { value: "voice", label: "Talk" },
];

describe("nextSegment", () => {
  it("moves with the reading direction: right is forward in LTR, backward in RTL", () => {
    const three: SegmentOption<"a" | "b" | "c">[] = [
      { value: "a", label: "A" },
      { value: "b", label: "B" },
      { value: "c", label: "C" },
    ];
    expect(nextSegment(three, "b", "ArrowRight", false)).toBe("c");
    expect(nextSegment(three, "b", "ArrowRight", true)).toBe("a");
    expect(nextSegment(three, "b", "ArrowLeft", false)).toBe("a");
    expect(nextSegment(three, "b", "ArrowLeft", true)).toBe("c");
    expect(nextSegment(modes, "text", "ArrowRight", false)).toBe("voice");
    expect(nextSegment(modes, "text", "ArrowLeft", true)).toBe("voice");
  });

  it("wraps and answers vertical arrows the same in both directions", () => {
    expect(nextSegment(modes, "voice", "ArrowRight", false)).toBe("text");
    expect(nextSegment(modes, "text", "ArrowDown", true)).toBe("voice");
    expect(nextSegment(modes, "text", "ArrowUp", false)).toBe("voice");
  });

  it("jumps to the ends on Home and End and ignores other keys", () => {
    expect(nextSegment(modes, "voice", "Home", false)).toBe("text");
    expect(nextSegment(modes, "text", "End", true)).toBe("voice");
    expect(nextSegment(modes, "text", "Enter", false)).toBeNull();
    expect(nextSegment(modes, "text", "a", true)).toBeNull();
  });

  it("never lands on a disabled option", () => {
    const noMic = [modes[0], { ...modes[1], disabled: true }];
    expect(nextSegment(noMic, "text", "ArrowRight", false)).toBe("text");
    expect(nextSegment(noMic, "text", "End", false)).toBe("text");
    expect(nextSegment([{ ...modes[0], disabled: true }], "text", "Home", false)).toBeNull();
  });
});
