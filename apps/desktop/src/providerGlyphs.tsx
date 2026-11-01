import deepgramGlyph from "./assets/providers/deepgram.svg?raw";
import anthropicGlyph from "./assets/providers/anthropic.svg?raw";
import deepseekGlyph from "./assets/providers/deepseek.svg?raw";
import geminiGlyph from "./assets/providers/gemini.svg?raw";
import groqGlyph from "./assets/providers/groq.svg?raw";
import lmstudioGlyph from "./assets/providers/lmstudio.svg?raw";
import mistralGlyph from "./assets/providers/mistral.svg?raw";
import ollamaGlyph from "./assets/providers/ollama.svg?raw";
import openrouterGlyph from "./assets/providers/openrouter.svg?raw";
import togetherGlyph from "./assets/providers/together.svg?raw";

// Provider marks (owner feedback F8, 2026-08-29): bundled monochrome
// glyphs rendered inline so `fill` inherits currentColor and follows both
// themes through the ink tokens — the Icons.tsx idiom, replacing the CSS
// mask-image approach that WebKitGTK painted as a solid gray square.
// Sources, versions, and per-asset licenses: assets/providers/manifest.json.
// Families and presets without a lawfully tintable mark fall back to a
// letter (asset-law rule 8, fail-closed); the manifest records each reason.

export const FAMILY_GLYPHS: Record<string, string> = {
  gemini: geminiGlyph,
  anthropic: anthropicGlyph,
  deepgram: deepgramGlyph,
};

export const PRESET_GLYPHS: Record<string, string> = {
  ollama: ollamaGlyph,
  lmstudio: lmstudioGlyph,
  openrouter: openrouterGlyph,
  groq: groqGlyph,
  together: togetherGlyph,
  mistral: mistralGlyph,
  deepseek: deepseekGlyph,
};

const HOST_PRESETS: Record<string, string> = {
  "openrouter.ai": "openrouter",
  "api.groq.com": "groq",
  "api.together.ai": "together",
  "api.mistral.ai": "mistral",
  "api.deepseek.com": "deepseek",
};

const LOOPBACK_PORT_PRESETS: Record<string, string> = {
  "11434": "ollama",
  "1234": "lmstudio",
};

/// Identify a connection's vendor from where its bytes actually go: the
/// documented API host for hosted presets, the documented default port for
/// loopback servers. Anything else — including api.openai.com, which has no
/// lawful glyph — resolves to null and renders as text identity.
export function presetIdForBaseUrl(baseUrl: string | null): string | null {
  if (baseUrl == null) return null;
  let url: URL;
  try {
    url = new URL(baseUrl);
  } catch {
    return null;
  }
  const host = url.hostname.toLowerCase();
  const hosted = HOST_PRESETS[host];
  if (hosted != null) return hosted;
  if (host === "localhost" || host === "127.0.0.1" || host === "[::1]") {
    return LOOPBACK_PORT_PRESETS[url.port] ?? null;
  }
  return null;
}

/// The two mark components below are the ONLY feeders of the inner-HTML
/// sink: they look glyphs up in the tables above (compile-time bundled,
/// manifest-gated by providerAssets.test.ts), so no caller can route an
/// arbitrary string into it.
export function FamilyMark({
  familyId,
  displayName,
}: {
  familyId: string;
  displayName: string;
}) {
  return (
    <GlyphMark
      svg={FAMILY_GLYPHS[familyId] ?? PRESET_GLYPHS[familyId] ?? null}
      fallbackText={displayName}
      size="md"
    />
  );
}

export function ConnectionPresetMark({
  baseUrl,
  name,
}: {
  baseUrl: string | null;
  name: string;
}) {
  const presetId = presetIdForBaseUrl(baseUrl);
  return (
    <GlyphMark
      svg={presetId == null ? null : (PRESET_GLYPHS[presetId] ?? null)}
      fallbackText={name}
      size="sm"
    />
  );
}

function GlyphMark({
  svg,
  fallbackText,
  size,
}: {
  svg: string | null;
  fallbackText: string;
  size: "sm" | "md";
}) {
  const box = size === "md" ? "h-9 w-9 rounded-lg" : "h-6 w-6 rounded-md";
  const glyphBox = size === "md" ? "h-5 w-5" : "h-3.5 w-3.5";
  if (svg == null) {
    return (
      <span
        aria-hidden="true"
        className={`flex shrink-0 items-center justify-center border border-ink-800 bg-ink-950 font-serif font-semibold text-ink-300 ${box} ${
          size === "md" ? "text-body" : "text-label"
        }`}
      >
        {fallbackText.slice(0, 1)}
      </span>
    );
  }
  return (
    <span
      aria-hidden="true"
      className={`flex shrink-0 items-center justify-center border border-ink-800 bg-ink-950 text-ink-300 ${box}`}
    >
      <span
        className={`${glyphBox} fill-current [&>svg]:h-full [&>svg]:w-full`}
        // Bundled, manifest-gated assets only (providerAssets.test.ts scans
        // this module); no runtime content ever reaches this sink.
        dangerouslySetInnerHTML={{ __html: svg }}
      />
    </span>
  );
}
