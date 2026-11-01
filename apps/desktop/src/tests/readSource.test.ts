import { mkdtempSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { readSource } from "./readSource";

const SRC = fileURLToPath(new URL("..", import.meta.url));

describe("readSource keeps source-scanning tests line-ending neutral", () => {
  it("reads a CRLF checkout exactly like an LF checkout", () => {
    const dir = mkdtempSync(join(tmpdir(), "read-source-"));
    try {
      const lf = join(dir, "lf.rs");
      const crlf = join(dir, "crlf.rs");
      const body = 'format!(\n    "first \\\n     second",\n)\n';
      writeFileSync(lf, body);
      writeFileSync(crlf, body.replace(/\n/g, "\r\n"));
      expect(readSource(crlf)).toBe(body);
      expect(readSource(lf)).toBe(body);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("is the only way a test under src reads a file as text", () => {
    const offenders = readdirSync(SRC, { recursive: true, encoding: "utf8" })
      .filter((rel) => /\.test\.tsx?$/.test(rel))
      .filter((rel) => /\breadFileSync\(/.test(readSource(join(SRC, rel))));
    expect(offenders).toEqual([]);
  });
});
