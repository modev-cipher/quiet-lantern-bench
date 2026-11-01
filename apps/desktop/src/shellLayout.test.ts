import { describe, expect, it } from "vitest";
import { readSource } from "./tests/readSource";

const CSS = readSource(new URL("./index.css", import.meta.url));

const VIEW_ROOTS = [
  "Library.tsx",
  "JobsPanel.tsx",
  "SettingsFrame.tsx",
] as const;

const SETTINGS_SECTIONS = [
  "Settings.tsx",
  "ModelsPanel.tsx",
  "ProvidersPage.tsx",
] as const;

const ROLES = ["shelf", "panel"] as const;

const SHELL_CONTAINER = /mx-auto flex w-full max-w-([a-z0-9-]+) flex-col/g;

function body(name: string): string {
  return readSource(new URL(`./${name}`, import.meta.url));
}

function themeBlock(): string {
  const start = CSS.indexOf("@theme");
  return CSS.slice(start, CSS.indexOf("}", start));
}

describe("the shell gives every view one of the measures it owns", () => {
  it("declares each role as a container token", () => {
    for (const role of ROLES) {
      expect(
        themeBlock(),
        `--container-${role} is missing, so max-w-${role} emits nothing`,
      ).toContain(`--container-${role}:`);
    }
  });

  it("routes every view root through a named role, never a bare Tailwind step", () => {
    for (const name of VIEW_ROOTS) {
      const found = [...body(name).matchAll(SHELL_CONTAINER)].map(
        (match) => match[1],
      );

      expect(
        found.length,
        `${name} has no shell container, so this scan says nothing about it`,
      ).toBeGreaterThan(0);

      for (const measure of found) {
        expect(
          (ROLES as readonly string[]).includes(measure),
          `${name} sizes its shell with max-w-${measure}; a view picks a role, not a width, or the four views drift apart again`,
        ).toBe(true);
      }
    }
  });

  it("lets the Settings frame own the measure, so its three sections share one edge", () => {
    for (const name of SETTINGS_SECTIONS) {
      expect(
        body(name),
        `${name} sizes itself again; inside the Settings frame a section takes the frame's width`,
      ).not.toMatch(/max-w-(panel|shelf)/);
    }
  });

  it("keeps the shelf wider than the panel, because grids scale and rows do not", () => {
    const width = (role: string) =>
      Number(
        /(\d+)px/.exec(
          new RegExp(`--container-${role}:\\s*([^;]+);`).exec(CSS)?.[1] ?? "",
        )?.[1],
      );

    expect(width("shelf")).toBeGreaterThan(width("panel"));
  });
});
