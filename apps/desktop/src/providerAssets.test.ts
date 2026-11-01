import { readdirSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import { PRESET_GLYPHS, presetIdForBaseUrl } from "./providerGlyphs";
import { OPENAI_COMPAT_PRESETS } from "./providerPresets";
import { readSource } from "./tests/readSource";

// S-5 asset law (acceptance gate 10), extended by F8: every shipped glyph
// has a manifest entry naming its source and license; no unmanifested
// asset ships; no runtime hotlink exists; assets are single-path
// monochrome geometry; text-only fallback is the declared state for
// families and presets without a lawful glyph.

const ASSETS = join(__dirname, "assets", "providers");

interface ManifestSource {
  repo: string;
  version: string;
  license: string;
  licenseUrl: string;
}

interface ManifestEntry {
  id: string;
  scope: "family" | "preset";
  asset: string | null;
  source?: string;
  slug?: string;
  textOnly?: boolean;
  reason?: string;
}

interface Manifest {
  sources: Record<string, ManifestSource>;
  retrieved: string;
  trademarkNote: string;
  entries: ManifestEntry[];
}

const manifest: Manifest = JSON.parse(
  readSource(join(ASSETS, "manifest.json")),
);

const glyphModule = readSource(join(__dirname, "providerGlyphs.tsx"));

describe("provider brand-asset manifest", () => {
  it("covers every shipped svg and ships nothing unmanifested", () => {
    const shipped = readdirSync(ASSETS).filter((name) => name.endsWith(".svg"));
    const manifested = manifest.entries
      .map((entry) => entry.asset)
      .filter((asset): asset is string => asset != null);
    expect(shipped.sort()).toEqual([...manifested].sort());
  });

  it("records source, version, license, retrieval date, and trademark note", () => {
    expect(manifest.retrieved).toMatch(/^\d{4}-\d{2}-\d{2}$/);
    expect(manifest.trademarkNote).toContain("identification only");
    for (const [name, source] of Object.entries(manifest.sources)) {
      expect(source.repo, `${name} needs a repo URL`).toMatch(/^https:\/\//);
      expect(source.version, `${name} needs a version`).toMatch(
        /^\d+\.\d+\.\d+$/,
      );
      expect(["CC0-1.0", "MIT"], `${name} license`).toContain(source.license);
      expect(source.licenseUrl, `${name} needs a license URL`).toMatch(
        /^https:\/\//,
      );
    }
    for (const entry of manifest.entries) {
      expect(["family", "preset"]).toContain(entry.scope);
      if (entry.asset != null) {
        expect(
          entry.source != null && manifest.sources[entry.source] != null,
          `${entry.id} needs a declared source`,
        ).toBe(true);
        expect(entry.slug, `${entry.id} needs its source slug`).toBeTruthy();
      } else {
        expect(entry.textOnly).toBe(true);
        expect(
          entry.reason,
          `${entry.id} text-only needs a reason`,
        ).toBeTruthy();
      }
    }
  });

  it("assets are single-path monochrome geometry: no styling, no scripts", () => {
    for (const entry of manifest.entries) {
      if (entry.asset == null) continue;
      const svg = readSource(join(ASSETS, entry.asset));
      expect(svg).toContain('viewBox="0 0 24 24"');
      expect(svg.match(/<path /g)?.length).toBe(1);
      for (const banned of [
        "<script",
        'fill="#',
        'fill="url',
        "style=",
        "gradient",
        "animate",
        "<image",
        "href=",
      ]) {
        expect(svg.includes(banned), `${entry.asset} contains ${banned}`).toBe(
          false,
        );
      }
    }
  });

  it("no runtime hotlink: glyphs are local bundled imports only", () => {
    for (const file of ["ProvidersPage.tsx", "providerGlyphs.tsx"]) {
      const text = readSource(join(__dirname, file));
      expect(text).not.toMatch(/src=["']https?:\/\//);
      expect(text).not.toMatch(/url\(["']?https?:\/\//);
    }
    // Every referenced glyph import resolves inside assets/providers and
    // is manifested; the page itself imports no assets directly.
    expect(readSource(join(__dirname, "ProvidersPage.tsx"))).not.toContain(
      'from "./assets/providers/',
    );
    const imports = [
      ...glyphModule.matchAll(/from "\.\/assets\/providers\/(.+?)\?raw"/g),
    ];
    expect(imports.length).toBeGreaterThan(0);
    for (const match of imports) {
      const manifested = manifest.entries.some((e) => e.asset === match[1]);
      expect(manifested, `${match[1]} imported but unmanifested`).toBe(true);
    }
  });

  it("text-only entries never resolve a glyph (fail-closed identity)", () => {
    for (const entry of manifest.entries) {
      if (entry.textOnly === true) {
        expect(
          glyphModule.includes(`${entry.id}: `),
          `${entry.id} must not be in any glyph table`,
        ).toBe(false);
      }
    }
  });

  it("every manifested preset glyph is reachable from its documented base URL", () => {
    for (const entry of manifest.entries) {
      if (entry.scope !== "preset" || entry.asset == null) continue;
      const preset = OPENAI_COMPAT_PRESETS.find((p) => p.id === entry.id);
      expect(preset, `${entry.id} has no matching preset`).toBeTruthy();
      expect(presetIdForBaseUrl(preset!.baseUrl)).toBe(entry.id);
      expect(PRESET_GLYPHS[entry.id]).toBeTruthy();
    }
  });
});

describe("presetIdForBaseUrl", () => {
  it("identifies loopback servers by their documented default port", () => {
    expect(presetIdForBaseUrl("http://localhost:11434/v1")).toBe("ollama");
    expect(presetIdForBaseUrl("http://127.0.0.1:11434/v1")).toBe("ollama");
    expect(presetIdForBaseUrl("http://localhost:1234/v1")).toBe("lmstudio");
  });

  it("identifies hosted providers by API hostname", () => {
    expect(presetIdForBaseUrl("https://openrouter.ai/api/v1")).toBe(
      "openrouter",
    );
    expect(presetIdForBaseUrl("https://api.groq.com/openai/v1")).toBe("groq");
    expect(presetIdForBaseUrl("https://api.together.ai/v1")).toBe("together");
    expect(presetIdForBaseUrl("https://api.mistral.ai/v1")).toBe("mistral");
    expect(presetIdForBaseUrl("https://api.deepseek.com")).toBe("deepseek");
  });

  it("fails closed to text identity for everything else", () => {
    expect(presetIdForBaseUrl(null)).toBeNull();
    expect(presetIdForBaseUrl("not a url")).toBeNull();
    expect(presetIdForBaseUrl("https://api.openai.com/v1")).toBeNull();
    expect(presetIdForBaseUrl("http://localhost:8080/v1")).toBeNull();
    expect(presetIdForBaseUrl("https://example.com/v1")).toBeNull();
  });
});
