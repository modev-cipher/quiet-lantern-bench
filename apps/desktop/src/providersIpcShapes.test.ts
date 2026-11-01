// The frontend half of the Providers IPC shape gate (checkpoint-3
// structural cure): the SAME fixture files the Rust serializer tests pin
// are parsed here into the frontend model types. A wire-contract drift on
// either side is a red test on that side — camelCase renames, dropped
// fields, and phantom fields (the old `credentialNotRequired`) can no
// longer ship silently.

import { describe, expect, it } from "vitest";

import connectionViews from "../src-tauri/fixtures/providers/ipc_shapes/connection_views.json";
import deleteImpact from "../src-tauri/fixtures/providers/ipc_shapes/delete_impact.json";
import familyViews from "../src-tauri/fixtures/providers/ipc_shapes/provider_family_view.json";
import poolTestProgress from "../src-tauri/fixtures/providers/ipc_shapes/pool_test_progress.json";
import {
  cardLabelText as rawcardLabelText,
  cardPoolLine as rawcardPoolLine,
  countsSummary as rawCountsSummary,
  parseDeleteImpact,
  poolLabelText as rawpoolLabelText,
  poolTestKey,
  slotHealthText,
  type PoolTestProgress,
  type ProviderConnectionView,
  type ProviderFamilyView,
} from "./providersModel";

const countsSummary = (...args: Parameters<typeof rawCountsSummary>) =>
  rawCountsSummary(...args).replace(/[\u2066-\u2069]/g, "");
describe("providers IPC shape gate (shared fixtures, two consumers)", () => {
  it("connection views parse into the frontend model with every fact present", () => {
    // The compile-time assignment IS the contract; the runtime asserts
    // catch a fixture edited out from under the type.
    const rows: ProviderConnectionView[] = connectionViews;
    expect(rows).toHaveLength(2);
    const [custom, fixed] = rows;
    expect(custom.providerType).toBe("openai_compatible");
    expect(custom.baseUrl).toBe("https://openrouter.ai/api/v1");
    expect(custom.keyRequired).toBe(true);
    expect(custom.defaultModel).toBe("some/model");
    expect(custom.keyLast4).toBe("ab12");
    expect(fixed.baseUrl).toBeNull();
    expect(fixed.hostname).toBeNull();
    expect(fixed.enabled).toBe(false);
    // Every field the page renders or round-trips is DEFINED — undefined
    // facts were exactly blocker 3.
    for (const row of rows) {
      for (const key of [
        "id",
        "name",
        "providerType",
        "hostname",
        "baseUrl",
        "defaultModel",
        "capabilities",
        "keyRequired",
        "enabled",
        "keyPresent",
        "keyLast4",
      ] as const) {
        expect(row[key], key).not.toBeUndefined();
      }
    }
  });

  it("delete impact parses fail-closed through the one parser", () => {
    const parsed = parseDeleteImpact(deleteImpact);
    expect(parsed).not.toBeNull();
    expect(parsed?.referencingJobs).toEqual(["job-1", "job-2"]);
    expect(parsed?.referencingVoiceProfiles).toEqual(["Aria (cloud)"]);
    // Anything that is NOT the backend shape blocks deletion.
    expect(parseDeleteImpact(null)).toBeNull();
    expect(parseDeleteImpact({})).toBeNull();
    expect(parseDeleteImpact({ referencingJobs: [] })).toBeNull();
    expect(
      parseDeleteImpact({
        referencedByDefaults: false,
        unfinishedJobs: 0,
        consents: 0,
        cloudVoiceProfiles: 0,
      }),
      "the OLD invented shape must read as unverifiable, never as clean",
    ).toBeNull();
    expect(
      parseDeleteImpact({ referencingJobs: [1], referencingVoiceProfiles: [] }),
    ).toBeNull();
  });

  it("the family projection parses and renders through the typed model", () => {
    // JSON imports widen tag literals to `string`, so the tagged-union
    // card/label fields need the cast; the renderer calls below are the
    // runtime half of the pin (an unknown tag renders nothing and fails).
    const families = familyViews as unknown as ProviderFamilyView[];
    const [gemini] = families;
    expect(cardLabelText(gemini.card)).toBe("Healthy");
    expect(cardPoolLine(gemini)).toBe("1 key configured · Healthy");
    expect(countsSummary(gemini.counts)).toBe("1 working");
    const [pool] = gemini.pools ?? [];
    expect(poolLabelText(pool.label)).toBe("Checking… 1 of 3");
    expect(slotHealthText(pool.slots[0].health)).toBe("Working");
    expect(pool.slots[0].dialEligible).toBe(true);
    expect(gemini.connections[0].defaultModel).toBe("gemini-3.7-flash");
  });

  it("pool test progress parses with the one shared reader key", () => {
    const progress: PoolTestProgress = poolTestProgress;
    expect(progress.done).toBe(false);
    expect(progress.cancelled).toBe(false);
    expect(poolTestKey(progress.provider, progress.baseUrl)).toBe(
      "gemini https://generativelanguage.googleapis.com",
    );
  });
});

function poolLabelText(
  ...args: Parameters<typeof rawpoolLabelText>
): ReturnType<typeof rawpoolLabelText> {
  const value = rawpoolLabelText(...args);
  return value == null ? value : value.replace(/[\u2066-\u2069]/g, "");
}

function cardPoolLine(
  ...args: Parameters<typeof rawcardPoolLine>
): ReturnType<typeof rawcardPoolLine> {
  const value = rawcardPoolLine(...args);
  return value == null ? value : value.replace(/[\u2066-\u2069]/g, "");
}

function cardLabelText(
  ...args: Parameters<typeof rawcardLabelText>
): ReturnType<typeof rawcardLabelText> {
  const value = rawcardLabelText(...args);
  return value == null ? value : value.replace(/[\u2066-\u2069]/g, "");
}
