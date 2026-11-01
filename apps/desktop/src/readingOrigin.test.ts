import { describe, expect, it } from "vitest";
import { fileURLToPath } from "node:url";
import { readSource } from "./tests/readSource";

const header = readSource(
  fileURLToPath(new URL("./ReadingHeader.tsx", import.meta.url)),
);
const model = readSource(
  fileURLToPath(new URL("./readingModel.ts", import.meta.url)),
);

function mapEntries(source: string, name: string): Record<string, string> {
  const start = source.indexOf(`const ${name}`);
  const open = source.indexOf("{", start);
  const close = source.indexOf("};", open);
  const body = source.slice(open + 1, close);
  const out: Record<string, string> = {};
  for (const match of body.matchAll(/(\w+):\s*\n?\s*"((?:[^"\\]|\\.)*)"/g)) {
    out[match[1]] = match[2];
  }
  return out;
}

describe("timeline origin labels", () => {
  const labels = mapEntries(header, "ORIGIN_LABEL");
  const hints = mapEntries(header, "ORIGIN_HINT");

  it("carries the four wire spellings the backend serialises", () => {
    for (const origin of ["measured", "dynamic", "unverified", "legacy"]) {
      expect(model, `readingModel must accept ${origin}`).toContain(`"${origin}"`);
      expect(Object.keys(labels)).toContain(origin);
    }
    expect(model).not.toContain('"nominal"');
  });

  it("gives dynamic and unverified distinct labels", () => {
    expect(labels.dynamic).not.toBe(labels.unverified);
    expect(new Set(Object.values(labels)).size).toBe(
      Object.keys(labels).length,
    );
  });

  it("never claims the assembled basis was measured or exact", () => {
    expect(labels.dynamic).toBe("assembled");
    expect(labels.dynamic).not.toMatch(/exact|measured/i);
    expect(
      hints.dynamic,
      "the assembled basis is an unmeasured hypothesis, so the tooltip must say so rather than let a rename launder it",
    ).toMatch(/not separately measured/);
    expect(hints.measured).toMatch(/measured/);
  });
});
