import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import CloneVoiceModal from "./CloneVoiceModal";

describe("CloneVoiceModal Component", () => {
  it("renders closed when open is false", () => {
    const html = renderToStaticMarkup(
      createElement(CloneVoiceModal, {
        open: false,
        cloningReady: true,
        existing: [],
        onClose: vi.fn(),
        onSaved: vi.fn(),
      }),
    );
    expect(html).toBe("");
  });

  it("renders needs-model screen when cloningReady is false with custom size", () => {
    const html = renderToStaticMarkup(
      createElement(CloneVoiceModal, {
        open: true,
        cloningReady: false,
        existing: [],
        cloningModelSize: "1.8 GB",
        onClose: vi.fn(),
        onSaved: vi.fn(),
      }),
    );
    expect(html).toContain('data-testid="clone-needs-model"');
    expect(html).toContain("1.8 GB");
    expect(html).toContain('data-testid="clone-open-models"');
  });

  it("renders engine dropdown when multiple cloning engines are available", () => {
    const cloningEngines = [
      { id: "studio-tts", name: "Voice · Pro", nonCommercial: true },
      { id: "chatterbox-0.5b", name: "Voice · Fast" },
    ];
    const html = renderToStaticMarkup(
      createElement(CloneVoiceModal, {
        open: true,
        cloningReady: true,
        existing: [],
        cloningEngines,
        initialEngine: "studio-tts",
        onClose: vi.fn(),
        onSaved: vi.fn(),
      }),
    );
    expect(html).toContain('data-testid="clone-engine-select"');
    expect(html).toContain("Voice · Pro");
    expect(html).toContain("Voice · Fast");
    expect(html).toContain('data-testid="clone-record"');
    expect(html).toContain('data-testid="clone-import"');
  });

  it("omits engine dropdown when only one cloning engine is available", () => {
    const cloningEngines = [
      { id: "studio-tts", name: "Voice · Pro", nonCommercial: true },
    ];
    const html = renderToStaticMarkup(
      createElement(CloneVoiceModal, {
        open: true,
        cloningReady: true,
        existing: [],
        cloningEngines,
        initialEngine: "studio-tts",
        onClose: vi.fn(),
        onSaved: vi.fn(),
      }),
    );
    expect(html).not.toContain('data-testid="clone-engine-select"');
    expect(html).toContain('data-testid="clone-record"');
    expect(html).toContain('data-testid="clone-import"');
  });
});

