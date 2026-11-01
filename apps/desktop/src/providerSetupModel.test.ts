import { describe, expect, it } from "vitest";
import directoryFixture from "../src-tauri/fixtures/providers/directory.json";
import {
  identityForUrl,
  projectProviders,
  type ProviderDefinition,
} from "./providerSetupModel";
import type { ProviderFamilyView } from "./providersModel";

const directory = directoryFixture as ProviderDefinition[];

describe("provider identity and credential boundaries", () => {
  it("recognizes only the exact official OpenAI aliases", () => {
    expect(
      identityForUrl(directory, "openai_compat", "https://api.openai.com"),
    ).toBe("openai");
    expect(
      identityForUrl(directory, "openai_compat", "https://api.openai.com/v1"),
    ).toBe("openai");
    expect(
      identityForUrl(
        directory,
        "openai_compat",
        "https://api.openai.com/proxy",
      ),
    ).toBe("custom");
    expect(
      identityForUrl(
        directory,
        "openai_compat",
        "https://api.openai.com.evil/v1",
      ),
    ).toBe("custom");
    expect(
      identityForUrl(
        directory,
        "openai_compat",
        "https://api.groq.com/openai/v1",
      ),
    ).toBe("groq");
  });

  it("keeps fixed OpenAI narration out of every other compatible vendor", () => {
    const source: ProviderFamilyView = {
      id: "openai_compat",
      displayName: "legacy",
      capabilities: [],
      card: { kind: "not_configured" },
      counts: {
        working: 0,
        checking: 0,
        unverified: 0,
        invalid: 0,
        rate_limited: 0,
        unreachable: 0,
        missing_bytes: 0,
        disabled: 0,
      },
      configuredKeys: 0,
      enabledKeys: 0,
      disabledConnections: 0,
      activeModels: [],
      vaultLocked: false,
      retentionLinks: [],
      pools: [],
      connections: [
        {
          id: "audio",
          name: "My narration",
          providerType: "openai_tts",
          hostname: null,
          baseUrl: null,
          defaultModel: "tts-1",
          capabilities: ["tts"],
          keyRequired: true,
          enabled: true,
          keyPresent: false,
          keyLast4: null,
        },
      ],
    };
    const projected = projectProviders(directory, [source]);
    expect(
      projected
        .find((entry) => entry.id === "openai")
        ?.connections.map((entry) => entry.id),
    ).toEqual(["audio"]);
    expect(
      projected
        .filter((entry) => entry.id !== "openai")
        .flatMap((entry) => entry.connections),
    ).toEqual([]);
  });

  it("uses the native health projection without reconstructing its priority rules", () => {
    const native: ProviderFamilyView = {
      id: "openai",
      custodyId: "openai_compat",
      displayName: "OpenAI",
      capabilities: [],
      card: { kind: "needs_unlock" },
      counts: {
        working: 0,
        checking: 0,
        unverified: 0,
        invalid: 0,
        rate_limited: 0,
        unreachable: 0,
        missing_bytes: 1,
        disabled: 0,
      },
      configuredKeys: 1,
      enabledKeys: 1,
      connections: [],
      disabledConnections: 0,
      activeModels: [],
      vaultLocked: true,
      retentionLinks: [],
      pools: [],
    };
    expect(
      projectProviders(directory, [native]).find(
        (entry) => entry.id === "openai",
      ),
    ).toBe(native);
  });
});
