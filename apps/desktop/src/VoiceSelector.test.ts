import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { VoiceSelector } from "./VoiceSelector";
import type { VoiceProfileSummary } from "./jobFormModel";
import type { CastCloudVoiceOption } from "./castProviderModel";
import type { ProviderOption } from "./castModelSelectionModel";

const mockLocalVoices: VoiceProfileSummary[] = [
  {
    id: "voice-1",
    name: "Heart",
    engine: "kokoro-82m",
    kind: "preset",
    presetVoiceId: "af_heart",
  },
  {
    id: "voice-2",
    name: "Kumo",
    engine: "kokoro-82m",
    kind: "preset",
    presetVoiceId: "jm_kumo",
  },
];

const mockCloudProviders: ProviderOption[] = [
  { id: "conn-gemini", name: "Google Gemini", isLocal: false, kind: "cloud" },
  { id: "conn-cartesia", name: "Cartesia", isLocal: false, kind: "cloud" },
];

const mockCloudVoices: CastCloudVoiceOption[] = [
  {
    voiceProfileId: "gem-puck",
    name: "Gemini Puck",
    voiceId: "Puck",
    connectionId: "conn-gemini",
    connectionName: "Google Gemini",
    provider: "gemini",
    model: "gemini-2.5-flash",
    loopback: false,
  },
  {
    voiceProfileId: "cart-warm",
    name: "Cartesia Warm",
    voiceId: "warm",
    connectionId: "conn-cartesia",
    connectionName: "Cartesia",
    provider: "cartesia_tts",
    model: "sonic-3.6",
    loopback: false,
  },
];

describe("VoiceSelector Component", () => {
  it("renders segmented control with Local active and local voice dropdown", () => {
    const html = renderToStaticMarkup(
      createElement(VoiceSelector, {
        usableVoices: mockLocalVoices,
        selectedLocalVoiceId: "voice-1",
        onSelectLocalVoice: vi.fn(),
        cloudVoices: mockCloudVoices,
        cloudProviders: mockCloudProviders,
        selectedCloudProviderId: "conn-gemini",
        selectedCloudVoiceId: "gem-puck",
        onSelectCloudProvider: vi.fn(),
        onSelectCloudVoice: vi.fn(),
        engineMode: "local",
        onSwitchEngine: vi.fn(),
        voiceMismatch: () => null,
        onOpenModels: vi.fn(),
        onCloneVoice: vi.fn(),
        bookId: "book-1",
        scopeArg: "all",
        mode: "narrator",
      }),
    );

    // Segmented control presence
    expect(html).toContain('data-testid="voice-engine-segmented"');
    expect(html).toContain('data-testid="voice-engine-local"');
    expect(html).toContain('data-testid="voice-engine-cloud"');
    expect(html).toMatch(/aria-checked="true"[^>]*data-testid="voice-engine-local"/);
    expect(html).toMatch(/aria-checked="false"[^>]*data-testid="voice-engine-cloud"/);

    // Local view elements
    expect(html).toContain('data-testid="voice-select"');
    expect(html).toContain("Heart");
    expect(html).toContain('data-testid="voice-preview"');
    expect(html).toContain('data-testid="clone-voice-open"');

    // Cloud cascading elements NOT rendered in local mode
    expect(html).not.toContain('data-testid="cast-cascade-provider-tts"');
    expect(html).not.toContain('data-testid="cast-cascade-model-tts"');
  });

  it("renders cascading selectors when Cloud engine is active with providers", () => {
    const html = renderToStaticMarkup(
      createElement(VoiceSelector, {
        usableVoices: mockLocalVoices,
        selectedLocalVoiceId: "voice-1",
        onSelectLocalVoice: vi.fn(),
        cloudVoices: mockCloudVoices,
        cloudProviders: mockCloudProviders,
        selectedCloudProviderId: "conn-gemini",
        selectedCloudVoiceId: "gem-puck",
        onSelectCloudProvider: vi.fn(),
        onSelectCloudVoice: vi.fn(),
        engineMode: "cloud",
        onSwitchEngine: vi.fn(),
        voiceMismatch: () => null,
        onOpenModels: vi.fn(),
        onCloneVoice: vi.fn(),
        bookId: "book-1",
        scopeArg: "all",
        mode: "narrator",
      }),
    );

    // Segmented control: Cloud is active
    expect(html).toMatch(/aria-checked="false"[^>]*data-testid="voice-engine-local"/);
    expect(html).toMatch(/aria-checked="true"[^>]*data-testid="voice-engine-cloud"/);

    // Cloud cascading selectors
    expect(html).toContain('data-testid="cast-cascade-provider-tts"');
    expect(html).toContain('data-testid="cast-cascade-model-tts"');
    expect(html).toContain("Google Gemini");
    expect(html).toContain("Gemini Puck");

    // Local single dropdown NOT rendered
    expect(html).not.toContain('data-testid="voice-select"');
    expect(html).not.toContain('data-testid="clone-voice-open"');
    expect(html).not.toContain('data-testid="voice-preview"');
  });

  it("renders empty state when Cloud engine is active but no cloud providers exist", () => {
    const html = renderToStaticMarkup(
      createElement(VoiceSelector, {
        usableVoices: mockLocalVoices,
        selectedLocalVoiceId: "voice-1",
        onSelectLocalVoice: vi.fn(),
        cloudVoices: [],
        cloudProviders: [],
        selectedCloudProviderId: null,
        selectedCloudVoiceId: null,
        onSelectCloudProvider: vi.fn(),
        onSelectCloudVoice: vi.fn(),
        engineMode: "cloud",
        onSwitchEngine: vi.fn(),
        voiceMismatch: () => null,
        onOpenModels: vi.fn(),
        onCloneVoice: vi.fn(),
        bookId: "book-1",
        scopeArg: "all",
        mode: "narrator",
      }),
    );

    expect(html).toContain('data-testid="cast-no-cloud-voices"');
    expect(html).toContain('data-testid="cast-open-providers-btn"');
    expect(html).not.toContain('data-testid="cast-cascade-provider-tts"');
  });

  it("renders voice-required warning when in local mode with 0 usable voices", () => {
    const html = renderToStaticMarkup(
      createElement(VoiceSelector, {
        usableVoices: [],
        selectedLocalVoiceId: null,
        onSelectLocalVoice: vi.fn(),
        cloudVoices: mockCloudVoices,
        cloudProviders: mockCloudProviders,
        selectedCloudProviderId: null,
        selectedCloudVoiceId: null,
        onSelectCloudProvider: vi.fn(),
        onSelectCloudVoice: vi.fn(),
        engineMode: "local",
        onSwitchEngine: vi.fn(),
        voiceMismatch: () => null,
        onOpenModels: vi.fn(),
        onCloneVoice: vi.fn(),
        bookId: "book-1",
        scopeArg: "all",
        mode: "narrator",
      }),
    );

    expect(html).toContain('data-testid="voice-required"');
    expect(html).not.toContain('data-testid="voice-select"');
  });

  it("renders language-advisory in Cloud mode when selectedMismatch is present", () => {
    const html = renderToStaticMarkup(
      createElement(VoiceSelector, {
        usableVoices: mockLocalVoices,
        selectedLocalVoiceId: "voice-1",
        onSelectLocalVoice: vi.fn(),
        cloudVoices: mockCloudVoices,
        cloudProviders: mockCloudProviders,
        selectedCloudProviderId: "p-gemini",
        selectedCloudVoiceId: "vp-puck",
        onSelectCloudProvider: vi.fn(),
        onSelectCloudVoice: vi.fn(),
        engineMode: "cloud",
        onSwitchEngine: vi.fn(),
        voiceMismatch: () => null,
        selectedMismatch: { book: "ar", voiceSpeaks: "en" },
        onOpenModels: vi.fn(),
        onCloneVoice: vi.fn(),
        bookId: "book-1",
        scopeArg: "all",
        mode: "narrator",
      }),
    );

    expect(html).toContain('data-testid="language-advisory"');
    expect(html).toContain("Arabic");
    expect(html).toContain("English");
  });

  it("renders Tier 1 model dropdown when multiple local engines exist and filters voices", () => {
    const multiEngineVoices: VoiceProfileSummary[] = [
      ...mockLocalVoices,
      {
        id: "studio-1",
        name: "Studio Voice",
        engine: "studio-tts",
        kind: "preset",
        presetVoiceId: "Studio Voice",
      },
    ];

    const localEngines = [
      { id: "kokoro-82m", name: "Voice · Fast", badge: "green" as const },
      { id: "studio-tts", name: "Voice · Pro", badge: "yellow" as const, nonCommercial: true },
    ];

    const html = renderToStaticMarkup(
      createElement(VoiceSelector, {
        usableVoices: multiEngineVoices,
        selectedLocalVoiceId: "studio-1",
        onSelectLocalVoice: vi.fn(),
        localEngines,
        selectedEngineId: "studio-tts",
        onSelectEngine: vi.fn(),
        preferredLanguage: "ar",
        cloudVoices: mockCloudVoices,
        cloudProviders: mockCloudProviders,
        selectedCloudProviderId: null,
        selectedCloudVoiceId: null,
        onSelectCloudProvider: vi.fn(),
        onSelectCloudVoice: vi.fn(),
        engineMode: "local",
        onSwitchEngine: vi.fn(),
        voiceMismatch: () => null,
        onOpenModels: vi.fn(),
        onCloneVoice: vi.fn(),
        bookId: "book-1",
        scopeArg: "all",
        mode: "narrator",
      }),
    );

    expect(html).toContain('data-testid="voice-engine-select"');
    expect(html).toContain("Voice · Fast");
    expect(html).toContain("Voice · Pro");
    expect(html).toContain("Studio Voice");
    expect(html).not.toContain("Heart");
  });

  it("omits Tier 1 model dropdown when only 1 local engine exists", () => {
    const localEngines = [
      { id: "kokoro-82m", name: "Voice · Fast", badge: "green" as const },
    ];

    const html = renderToStaticMarkup(
      createElement(VoiceSelector, {
        usableVoices: mockLocalVoices,
        selectedLocalVoiceId: "voice-1",
        onSelectLocalVoice: vi.fn(),
        localEngines,
        selectedEngineId: "kokoro-82m",
        onSelectEngine: vi.fn(),
        cloudVoices: mockCloudVoices,
        cloudProviders: mockCloudProviders,
        selectedCloudProviderId: null,
        selectedCloudVoiceId: null,
        onSelectCloudProvider: vi.fn(),
        onSelectCloudVoice: vi.fn(),
        engineMode: "local",
        onSwitchEngine: vi.fn(),
        voiceMismatch: () => null,
        onOpenModels: vi.fn(),
        onCloneVoice: vi.fn(),
        bookId: "book-1",
        scopeArg: "all",
        mode: "narrator",
      }),
    );

    expect(html).not.toContain('data-testid="voice-engine-select"');
    expect(html).toContain('data-testid="voice-select"');
    expect(html).toContain("Heart");
  });
});

