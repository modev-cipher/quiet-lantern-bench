import { describe, expect, it } from "vitest";

import { castScopeOf } from "./castScope";

describe("castScopeOf reads the scope the render wrote into the file name", () => {
  it("reads a contiguous chapter run", () => {
    expect(castScopeOf("C:\\exports\\a1\\Jane Eyre - Ch 1-3.mp3")).toEqual({
      kind: "chapters",
      list: "1–3",
      single: false,
    });
  });

  it("reads a single chapter", () => {
    expect(castScopeOf("/exports/b2/Fixture Book - Ch 2.mp3")).toEqual({
      kind: "chapters",
      list: "2",
      single: true,
    });
  });

  it("reads a scattered selection with its capped remainder", () => {
    expect(castScopeOf("/x/Book - Ch 3, 7, 12, …+8.mp3")).toEqual({
      kind: "chapters",
      list: "3, 7, 12, …+8",
      single: false,
    });
  });

  it("reads a page range", () => {
    expect(castScopeOf("/x/Book - Pages 1-12.mp3")).toEqual({
      kind: "pages",
      start: 1,
      end: 12,
    });
  });

  it("uses the last separator, so a title with a dash still parses", () => {
    expect(castScopeOf("/x/Word - Jane Eyre.doc - Ch 1-2.mp3")).toEqual({
      kind: "chapters",
      list: "1–2",
      single: false,
    });
  });

  it("refuses names the render marks as unknowable, and names with no scope", () => {
    expect(castScopeOf("/x/Book - Ch none.mp3")).toBeNull();
    expect(castScopeOf("/x/Book - Ch unknown.mp3")).toBeNull();
    expect(castScopeOf("/x/just-a-file.mp3")).toBeNull();
    expect(castScopeOf("")).toBeNull();
  });
});
