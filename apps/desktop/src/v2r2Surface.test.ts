import { describe, expect, it } from "vitest";
import { readSource } from "./tests/readSource";

const read = (name: string) =>
  readSource(new URL(`./${name}`, import.meta.url));

const REGISTRY = read("V2R2.tsx");
const SHELL = read("V2R2Shell.tsx");
const STUDY = read("V2R2Study.tsx");
const SCREENS = read("V2R2Screens.tsx");
const DATA = read("V2R2Data.ts");
const PRODUCTION = read("V2R2Production.tsx");
const MAIN = read("main.tsx");

const ALL_TSX = [REGISTRY, SHELL, STUDY, SCREENS, PRODUCTION];

describe("v2r2 harness gating", () => {
  it("only renders in dev builds", () => {
    expect(REGISTRY).toContain("if (!import.meta.env.DEV) return null;");
  });

  it("exposes all seven deliverable screens", () => {
    for (const key of [
      "resume:",
      "home:",
      "study:",
      "focus:",
      "rtl:",
      "library:",
      "tray:",
      "production:",
    ]) {
      expect(REGISTRY).toContain(key);
    }
  });

  it("is wired ahead of App only via the search-param gate", () => {
    expect(MAIN).toContain("v2r2FromSearch(window.location.search)");
    expect(MAIN).toContain("<App />");
  });
});

describe("study layout honors the inline-start tutor ruling", () => {
  it("renders the companion zone before the reader", () => {
    const companion = STUDY.indexOf("{companionOpen && <Companion />}");
    const reader = STUDY.indexOf("<Reader");
    expect(companion).toBeGreaterThan(-1);
    expect(reader).toBeGreaterThan(companion);
  });

  it("uses only logical direction utilities", () => {
    for (const source of ALL_TSX) {
      expect(source).not.toMatch(
        /className="[^"]*\b(?:ml|mr|pl|pr)-\d|[^-\w](?:left|right)-\d|border-[lr][ -]/,
      );
    }
  });
});

describe("the live tutor never coexists with a text highlight", () => {
  it("gates the narration highlight on the tutor being closed", () => {
    expect(STUDY).toContain("paragraph.highlight && !tutorLive");
    expect(STUDY).not.toMatch(/\{paragraph\.highlight \?/);
  });

  it("marks the reader tutor-live whenever the live companion is open", () => {
    expect(STUDY).toContain("tutorLive={companionOpen}");
  });
});

describe("retired destinations keep permanent homes", () => {
  it("keeps a labeled Settings route in the shell chrome", () => {
    expect(SHELL).toContain('data-testid="v2r2-settings"');
    expect(SHELL).toContain("Settings\n      </button>");
  });

  it("routes from the tray peek to the full production surface", () => {
    expect(SHELL).toContain('data-testid="v2r2-open-production"');
    expect(PRODUCTION).toContain('data-testid="v2r2-production"');
  });

  it("keeps the documented 180px library grid contract", () => {
    expect(SCREENS).toContain("minmax(min(180px,100%),1fr)");
    expect(SCREENS).not.toContain("220px");
  });

  it("gives orphaned and missing exports a visible library home", () => {
    expect(SCREENS).toContain('data-testid="v2r2-orphans"');
    expect(DATA).toContain("missing: true");
  });
});

describe("owner rulings hold structurally", () => {
  it("ships no Notes tab or any disabled stub in the companion", () => {
    expect(STUDY).not.toContain("Notes");
    expect(STUDY).not.toContain("disabledReason");
    expect(STUDY).toContain('<CompanionTab label="Tutor" active />');
    expect(STUDY).toContain('<CompanionTab label="Book" />');
  });

  it("keeps Home to one hero and one jump row, nothing else", () => {
    const home = SCREENS.slice(
      0,
      SCREENS.indexOf("export function LibraryScreen"),
    );
    expect(home).toContain('data-testid="v2r2-hero"');
    expect(home).toContain('data-testid="v2r2-jump-row"');
    expect(home).not.toContain("v2r2-orphans");
    expect(home).not.toContain("Unattached audio");
  });

  it("gives the activity pill idle and engine forms, never a lone demo state", () => {
    expect(SHELL).toContain('| { kind: "idle" }');
    expect(SHELL).toContain('activity.kind === "idle"');
    expect(SHELL).toContain('{ kind: "engine"; label: string; severity:');
    const idle = SHELL.indexOf('activity.kind === "idle"');
    expect(SHELL.slice(idle, idle + 200)).not.toContain("Casting");
  });
});

describe("production stays under the hood", () => {
  it("states availability in chapter language, never a timeline percentage", () => {
    expect(DATA).toContain("Ready through Chapter 5");
    expect(DATA).not.toMatch(/availability: "[^"]*%/);
  });

  it("keeps the transport on the listening-object family", () => {
    expect(SHELL).toContain("bg-paper-base-e");
    expect(SHELL).toContain('data-testid="v2r2-transport"');
  });

  it("keeps queue copy serial-honest in the tray", () => {
    expect(SHELL).toContain("waiting for the current cast");
    expect(SHELL).toContain("One cast runs at a time.");
  });
});
