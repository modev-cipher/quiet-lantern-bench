import { describe, expect, it } from "vitest";

import {
  consentCopyFor,
  hostnameOf,
  importDetailsOf,
  importSummaryOf,
  maskedKeyOf,
  needsConsent,
  providerLabelOf,
  sortedEntries,
  stateBadgeOf,
  storageNoticeOf,
  validateCustomUrl,
  type ImportReport,
  type VoiceKeyEntry,
  type VoiceProvider,
} from "./voiceKeysModel";

const CUSTOM_PROVIDER: VoiceProvider = {
  id: "openai_compat",
  defaultBaseUrl: null,
  capabilities: ["llm"],
};

const NAMED_PROVIDER: VoiceProvider = {
  id: "deepgram",
  defaultBaseUrl: "https://api.deepgram.com",
  capabilities: ["stt"],
};

function entry(overrides: Partial<VoiceKeyEntry> = {}): VoiceKeyEntry {
  const base = {
    baseUrl: "https://api.deepgram.com",
    slot: 0,
    provider: "deepgram",
    label: null,
    last4: "9xk2",
    state: "unchecked" as const,
    keyPresent: true,
    custom: false,
    addedAt: 1756000000000,
    lastUsed: null,
    ...overrides,
  };
  return {
    ...base,
    account:
      overrides.account ??
      (base.slot === 0 ? base.baseUrl : `${base.baseUrl}#${base.slot}`),
  };
}

describe("custom base URL admission", () => {
  it("accepts https and refuses plain http for anything that is not loopback", () => {
    expect(validateCustomUrl("https://api.example.dev/v1")).toEqual({
      url: "https://api.example.dev/v1",
    });
    const refused = validateCustomUrl("http://api.example.dev/v1");
    expect(refused).toHaveProperty("error");
    expect((refused as { error: string }).error).toContain("https://");
  });

  it("exempts localhost loopback exactly as the spec words it", () => {
    expect(validateCustomUrl("http://localhost:11434/v1")).toEqual({
      url: "http://localhost:11434/v1",
    });
    expect(validateCustomUrl("http://127.0.0.1:8080")).toEqual({
      url: "http://127.0.0.1:8080",
    });
    expect(validateCustomUrl("http://192.168.1.20:11434/v1")).toHaveProperty(
      "error",
    );
  });

  it("refuses embedded credentials and query strings before they reach the backend", () => {
    expect(validateCustomUrl("https://user:pw@api.example.dev")).toHaveProperty(
      "error",
    );
    expect(
      validateCustomUrl("https://api.example.dev/v1?key=abc"),
    ).toHaveProperty("error");
    expect(validateCustomUrl("not a url")).toHaveProperty("error");
    expect(validateCustomUrl("   ")).toHaveProperty("error");
  });
});

describe("consent gating", () => {
  it("requires consent for custom providers until the URL has been consented to", () => {
    expect(needsConsent(CUSTOM_PROVIDER, null)).toBe(true);
    expect(
      needsConsent(CUSTOM_PROVIDER, {
        canonicalUrl: "https://api.example.dev/v1",
        consented: false,
      }),
    ).toBe(true);
    expect(
      needsConsent(CUSTOM_PROVIDER, {
        canonicalUrl: "https://api.example.dev/v1",
        consented: true,
      }),
    ).toBe(false);
  });

  it("never interposes the interstitial for a named provider's fixed endpoint", () => {
    expect(needsConsent(NAMED_PROVIDER, null)).toBe(false);
  });

  it("names the destination host and states what it receives", () => {
    const copy = consentCopyFor("api.example.dev");
    expect(copy).toContain("api.example.dev");
    expect(copy).toContain("key");
    expect(copy).toContain("microphone audio");
    expect(copy).toContain("book excerpts");
    expect(copy).toContain("once per URL");
  });

  it("extracts the host including a non-default port for the interstitial", () => {
    expect(hostnameOf("https://api.example.dev:8443/v1")).toBe(
      "api.example.dev:8443",
    );
    expect(hostnameOf("garbage")).toBe("garbage");
  });
});

describe("key rows", () => {
  it("shows at most the last four characters and never a reveal", () => {
    expect(maskedKeyOf(entry({ last4: "9xk2" }))).toBe("…9xk2");
    expect(maskedKeyOf(entry({ keyPresent: false, last4: null }))).toBe(
      "key not loaded",
    );
  });

  it("maps each health state to its plain-language badge", () => {
    expect(stateBadgeOf("active")).toEqual({ label: "Working", tone: "ok" });
    expect(stateBadgeOf("invalid")).toEqual({
      label: "Not working",
      tone: "error",
    });
    expect(stateBadgeOf("out_of_quota")).toEqual({
      label: "Rate limited",
      tone: "warn",
    });
    expect(stateBadgeOf("unchecked")).toEqual({
      label: "Not yet used",
      tone: "muted",
    });
  });

  it("badge vocabulary is plain words only — no digits, codes, or jargon", () => {
    // The exact-label pin above can drift together with a bad rename; this
    // needle holds the CLASS of regression still: no close code, protocol
    // number, or enum name may ever reach a badge the user reads.
    for (const state of [
      "active",
      "invalid",
      "out_of_quota",
      "unchecked",
    ] as const) {
      const { label } = stateBadgeOf(state);
      expect(
        /^[A-Za-z ]+$/.test(label),
        `badge label "${label}" must be plain words, never codes`,
      ).toBe(true);
      expect(label.toLowerCase()).not.toContain("quota");
      expect(label.toLowerCase()).not.toContain("unchecked");
      expect(label.toLowerCase()).not.toContain("invalid");
    }
  });

  it("orders rows deterministically by provider then URL then slot", () => {
    const shuffled = [
      entry({ provider: "gemini", baseUrl: "https://g", slot: 10 }),
      entry({ provider: "cartesia", baseUrl: "https://c" }),
      entry({ provider: "gemini", baseUrl: "https://g", slot: 2 }),
      entry({ provider: "gemini", baseUrl: "https://a" }),
    ];
    expect(
      sortedEntries(shuffled).map((e) => `${e.baseUrl}#${e.slot}`),
    ).toEqual([
      "https://c#0",
      "https://a#0",
      // Numeric, not lexical: slot 2 before slot 10.
      "https://g#2",
      "https://g#10",
    ]);
  });

  it("labels every provider the backend registry exposes", () => {
    for (const id of ["gemini", "deepgram", "cartesia", "openai_compat"]) {
      expect(providerLabelOf(id)).not.toBe(id);
    }
    expect(providerLabelOf("someday_new")).toBe("someday_new");
  });
});

describe("bulk import receipt", () => {
  const report: ImportReport = {
    imported: [
      entry({
        provider: "gemini",
        label: "GEMINI_KEY_1",
        slot: 0,
        last4: "aa11",
      }),
      entry({
        provider: "gemini",
        label: "GEMINI_KEY_2",
        slot: 1,
        last4: "bb22",
      }),
    ],
    duplicates: [{ line: 7, label: "GEMINI_KEY_3" }],
    rejected: [{ line: 9, reason: "the value is empty" }],
  };

  it("summarizes counts in plain language", () => {
    expect(importSummaryOf(report)).toBe(
      "Imported 2 keys; skipped 1 already stored; could not read 1 line.",
    );
    expect(
      importSummaryOf({
        imported: [report.imported[0]],
        duplicates: [],
        rejected: [],
      }),
    ).toBe("Imported 1 key.");
    expect(
      importSummaryOf({ imported: [], duplicates: [], rejected: [] }),
    ).toBe("Imported 0 keys.");
  });

  it("details name labels, lines, and fixed reasons — never key material", () => {
    const details = importDetailsOf(report);
    expect(details).toEqual([
      "Added: GEMINI_KEY_1, GEMINI_KEY_2",
      "Line 7 (GEMINI_KEY_3): already stored, skipped",
      "Line 9: the value is empty",
    ]);
  });

  it("falls back to the slot number for an unlabeled import", () => {
    const unlabeled = {
      imported: [entry({ slot: 3, label: null })],
      duplicates: [],
      rejected: [],
    };
    expect(importDetailsOf(unlabeled)[0]).toBe("Added: slot 3");
  });
});

describe("fallback storage notice", () => {
  it("stays silent on the OS keychain and surfaces every fallback level", () => {
    expect(storageNoticeOf({ level: "os_keychain", locked: false })).toBeNull();
    expect(
      storageNoticeOf({ level: "encrypted_file", locked: true }),
    ).toContain("locked");
    expect(
      storageNoticeOf({ level: "encrypted_file", locked: false }),
    ).toContain("passphrase-encrypted");
    expect(storageNoticeOf({ level: "session_only", locked: false })).toContain(
      "forgotten when the app closes",
    );
  });
});
