// Owner ruling #31.3 (public-copy blackbox), extended to UI strings by
// ruling #50: no engine, library, runtime or model name reaches the user in
// any locale. Test-support constant shared by the catalog sweep and the
// Library presenter pins (round-2 finding AT-2-2 was filed against the
// scanned-import notice naming the layout model). Vendor/provider names are
// allowed on the Providers surface only; the i18n-model-names e2e pin owns
// those, so they are deliberately not in this set.
// Extended by the BLACKOUT-SWEEP lane (ruling #31.3 + #25 Q2, 2026-09-09) with
// the terms scrubbed from README.md / docs / AGENTS.md so each stays pinned:
// engines (piper, coqui, orpheus, f5-tts), PDF layers (mupdf, fitz), the ML
// packaging stack (pytorch, fastapi, pyinstaller) and versioned model ids
// (gpt-N, claude-N, gemini-N, mistral-N, phi-N). Provider/vendor names stay
// out on purpose (Providers surface, e2e pin i18n-model-names).
export const BANNED_PUBLIC_TOKENS =
  /docling|kokoro|whisper|pdfium|pypdfium|chatterbox|xtts|orpheus|\bqwen|gemma|llama|\bphi-?\d|f5-tts|tauri|\brust\b|sidecar|manim|cairo|gguf|onnx|pymupdf|mupdf|\bfitz\b|tesseract|\bpiper\b|coqui|pytorch|fastapi|pyinstaller|gpt-\d|claude-\d|gemini-\d|mistral-\d|silero|\bort\b/i;
