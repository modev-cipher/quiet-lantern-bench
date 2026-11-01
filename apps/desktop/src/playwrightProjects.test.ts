import { describe, expect, it } from "vitest";

import { readingProjects, WEBKIT_ENV } from "./playwrightProjects";

describe("readingProjects", () => {
  it("declares no project at all by default", () => {
    expect(
      readingProjects({}),
      "an empty list spreads to nothing, so the default config stays exactly what it was",
    ).toEqual([]);
  });

  it("leaves a spread config byte-identical when it declares nothing", () => {
    const base = {
      testDir: "./e2e",
      use: { channel: "chromium" },
    };
    const composed = { ...base, ...(readingProjects({}).length > 0 ? { projects: readingProjects({}) } : {}) };

    expect(JSON.stringify(composed)).toBe(JSON.stringify(base));
  });

  it("declares chromium first and webkit second when webkit is asked for", () => {
    const projects = readingProjects({ [WEBKIT_ENV]: "1" });

    expect(projects.map((project) => project.name)).toEqual([
      "chromium",
      "webkit",
    ]);
  });

  it("names webkit so a skip is visible in the report rather than silent", () => {
    const projects = readingProjects({ [WEBKIT_ENV]: "1" });

    expect(projects[1].name).toBe("webkit");
    expect(projects[1].use.browserName).toBe("webkit");
  });

  it("keeps chromium as the gate, because CI installs only chromium", () => {
    const projects = readingProjects({ [WEBKIT_ENV]: "1" });

    expect(projects[0].use.channel).toBe("chromium");
  });

  it("ignores any value other than an explicit opt-in", () => {
    expect(readingProjects({ [WEBKIT_ENV]: "0" })).toEqual([]);
    expect(readingProjects({ [WEBKIT_ENV]: "true" })).toEqual([]);
    expect(readingProjects({ [WEBKIT_ENV]: "" })).toEqual([]);
  });
});
