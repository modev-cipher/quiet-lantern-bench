// Field session 2026-10-09: the tutor's Arabic answers, dense with English
// terms, rendered left to right whenever a block opened with a Latin word
// ("1. Introduction: بنتعرف …"), and replies arrived in sentence-sized
// jumps. These pin per-block direction, the small Markdown subset, and the
// streaming reveal step.

import { describe, expect, it } from "vitest";

import {
  REVEAL_MIN_CHARS_PER_SECOND,
  blockDirection,
  messageDirection,
  parseInlines,
  parseTutorBlocks,
  revealStep,
  trimOpenMarkers,
} from "./tutorTextModel";

// The reply from session 01a121d5, abridged.
const FIELD_REPLY = [
  "الكتاب بيغطي مفاهيم الـ Distributed Systems الأساسية بتسلسل تصاعدي منظم جدا.",
  "",
  "الـ Layout بتاع الكتاب متقسم لـ 9 فصول رئيسية:",
  "",
  "1. Introduction: بنتعرف فيه على تعريف الـ distributed system، والأهداف الجوهرية لبنائه زي الـ resource sharing والـ distribution transparency والـ openness والـ scalability.",
  "2. Architectures: بيدرس الأنماط المعمارية للأنظمة الموزعة، سواء على مستوى الـ software architecture زي الـ layered والـ event-based.",
].join("\n");

describe("per-block direction follows the words, not the first letter", () => {
  it("reads an Arabic sentence that opens with an English term right to left", () => {
    expect(
      blockDirection(
        "Introduction: بنتعرف فيه على تعريف الـ distributed system، والأهداف الجوهرية لبنائه زي الـ resource sharing",
      ),
    ).toBe("rtl");
  });

  it("reads Arabic around a book title and author right to left", () => {
    expect(
      blockDirection("كتاب Distributed Systems لـ Maarten van Steen و"),
    ).toBe("rtl");
  });

  it("keeps English with an Arabic word or two left to right", () => {
    expect(blockDirection("The word موزعة means distributed in Arabic.")).toBe(
      "ltr",
    );
    expect(blockDirection("A layered architecture stacks components.")).toBe(
      "ltr",
    );
  });

  it("ignores inline code when judging", () => {
    expect(blockDirection("استخدم `fetch(url, options)` عشان تبعت الطلب")).toBe(
      "rtl",
    );
  });

  it("treats text without letters as left to right", () => {
    expect(blockDirection("1.2.3 — 42%")).toBe("ltr");
  });
});

describe("the tutor's Markdown subset", () => {
  it("splits the field reply into paragraphs and numbered items, each with its direction", () => {
    const blocks = parseTutorBlocks(FIELD_REPLY);
    expect(blocks.map((block) => block.kind)).toEqual([
      "paragraph",
      "paragraph",
      "item",
      "item",
    ]);
    for (const block of blocks) {
      expect(
        block.kind === "code" || block.kind === "table" ? null : block.dir,
      ).toBe("rtl");
    }
    const first = blocks[2];
    expect(first.kind === "item" && first.marker).toBe("1.");
    expect(messageDirection(FIELD_REPLY)).toBe("rtl");
  });

  it("parses bullets, headings and fenced code; code never takes a direction", () => {
    const blocks = parseTutorBlocks(
      "## Layers\n- one\n* two\n```\nlet x = 1;\n```\nafter",
    );
    expect(blocks.map((block) => block.kind)).toEqual([
      "heading",
      "item",
      "item",
      "code",
      "paragraph",
    ]);
    expect(blocks[3]).toEqual({ kind: "code", text: "let x = 1;" });
  });

  it("runs an unclosed fence to the end (a reply still streaming)", () => {
    expect(parseTutorBlocks("look:\n```\nfn main() {")).toEqual([
      expect.objectContaining({ kind: "paragraph" }),
      { kind: "code", text: "fn main() {" },
    ]);
  });

  it("keeps bold and code spans, and leaves unclosed markers literal", () => {
    expect(parseInlines("the **layered** style uses `send()` calls")).toEqual([
      { kind: "text", text: "the " },
      { kind: "strong", text: "layered" },
      { kind: "text", text: " style uses " },
      { kind: "code", text: "send()" },
      { kind: "text", text: " calls" },
    ]);
    expect(parseInlines("a **b")).toEqual([{ kind: "text", text: "a **b" }]);
  });

  it("never interprets HTML: tags stay text", () => {
    expect(parseInlines("<img src=x onerror=alert(1)>")).toEqual([
      { kind: "text", text: "<img src=x onerror=alert(1)>" },
    ]);
  });

  it("hides a marker that has not closed yet while streaming", () => {
    expect(trimOpenMarkers("the **lay")).toBe("the lay");
    expect(trimOpenMarkers("call `sen")).toBe("call sen");
    expect(trimOpenMarkers("the **layered** style")).toBe(
      "the **layered** style",
    );
  });
});

describe("the streaming reveal", () => {
  const burst =
    "Distributed systems hide their distribution from the user, which is called transparency.";

  it("glides a burst in and always lands on a word boundary", () => {
    const step = revealStep(burst, 0, 50);
    expect(step).toBeGreaterThan(0);
    expect(step).toBeLessThan(burst.length);
    expect(burst[step]).toMatch(/\s/);
  });

  it("never falls far behind: the rate grows with the backlog", () => {
    const long = `${burst} `.repeat(20);
    const slow = (REVEAL_MIN_CHARS_PER_SECOND * 50) / 1000;
    expect(revealStep(long, 0, 50)).toBeGreaterThan(slow * 4);
    expect(revealStep(long, 0, 500)).toBe(long.length);
  });

  it("is done when everything is shown", () => {
    expect(revealStep(burst, burst.length, 16)).toBe(burst.length);
  });
});

describe("review 2026-10-09 rendering (U3/U4)", () => {
  it("gives each line of a mixed paragraph its own direction", () => {
    const blocks = parseTutorBlocks(
      "الـ layered architecture بتقسم النظام لطبقات.\nThe OSI model has seven layers.",
    );
    expect(
      blocks.map((b) =>
        b.kind === "paragraph" ? [b.dir, b.continued === true] : null,
      ),
    ).toEqual([
      ["rtl", false],
      ["ltr", true],
    ]);
  });

  it("keeps URLs and inline math as LTR islands that never vote on direction", () => {
    const [block] = parseTutorBlocks(
      "المعادلة $T = n^2 / p$ موجودة في https://example.com/book/ch1 بالتفصيل",
    );
    expect(block.kind === "paragraph" && block.dir).toBe("rtl");
    const inlines = block.kind === "paragraph" ? block.inlines : [];
    expect(inlines.filter((i) => i.kind === "math")).toEqual([
      { kind: "math", text: "T = n^2 / p" },
    ]);
    expect(inlines.filter((i) => i.kind === "url")).toEqual([
      { kind: "url", text: "https://example.com/book/ch1" },
    ]);
  });

  it("does not read prices as math", () => {
    const [block] = parseTutorBlocks("It costs $5 and $10 per month.");
    const kinds =
      block.kind === "paragraph" ? block.inlines.map((i) => i.kind) : [];
    expect(kinds).not.toContain("math");
  });

  it("keeps a table as one LTR block and nested items at their depth", () => {
    const blocks = parseTutorBlocks(
      [
        "| layer | role |",
        "|---|---|",
        "| data | storage |",
        "",
        "- top",
        "  - nested",
      ].join("\n"),
    );
    expect(blocks[0]).toEqual({
      kind: "table",
      text: "| layer | role |\n|---|---|\n| data | storage |",
    });
    expect(
      blocks.slice(1).map((b) => (b.kind === "item" ? b.depth : -1)),
    ).toEqual([0, 1]);
  });
});
