import { describe, expect, it } from "vitest";

import {
  activeModelsLine as rawactiveModelsLine,
  cardLabelText as rawcardLabelText,
  cardPoolLine as rawcardPoolLine,
  countsSummary as rawCountsSummary,
  poolLabelText as rawpoolLabelText,
  slotHealthText,
  visibleSlots,
  type ProviderFamilyView,
  type SlotStateCounts,
} from "./providersModel";

// PF-PH1 frontend law: copy renders from TYPED values only — these pins
// hold the exact vocabulary of the binding table (S-5 pre-brief).

const COUNTS: SlotStateCounts = {
  working: 0,
  checking: 0,
  unverified: 0,
  invalid: 0,
  rate_limited: 0,
  unreachable: 0,
  missing_bytes: 0,
  disabled: 0,
};

function family(over: Partial<ProviderFamilyView>): ProviderFamilyView {
  return {
    id: "gemini",
    displayName: "Google Gemini",
    capabilities: ["Scripting"],
    card: { kind: "healthy" },
    counts: COUNTS,
    configuredKeys: 0,
    enabledKeys: 0,
    connections: [],
    disabledConnections: 0,
    activeModels: [],
    vaultLocked: false,
    retentionLinks: [],
    ...over,
  };
}

const countsSummary = (...args: Parameters<typeof rawCountsSummary>) =>
  rawCountsSummary(...args).replace(/[\u2066-\u2069]/g, "");
describe("pool label vocabulary (the 14-rule table)", () => {
  it("renders every label from its tagged kind, never by parsing", () => {
    expect(poolLabelText({ kind: "connection_pool_disabled" })).toBe(
      "Connection pool disabled",
    );
    expect(poolLabelText({ kind: "no_key_required" })).toBe("No key required");
    expect(poolLabelText({ kind: "pool_locked" })).toBe("Pool locked");
    expect(poolLabelText({ kind: "checking", completed: 3, total: 7 })).toBe(
      "Checking… 3 of 7",
    );
    expect(poolLabelText({ kind: "no_keys_configured" })).toBe(
      "No keys configured",
    );
    expect(poolLabelText({ kind: "keys_disabled" })).toBe("Keys disabled");
    expect(poolLabelText({ kind: "healthy" })).toBe("Pool Healthy");
    expect(poolLabelText({ kind: "degraded", need_attention: 3 })).toBe(
      "Pool Degraded · 3 need attention",
    );
    expect(poolLabelText({ kind: "unverified" })).toBe("Pool Unverified");
    expect(poolLabelText({ kind: "keys_needed" })).toBe("Keys needed");
    expect(
      poolLabelText({ kind: "rate_limited", earliest_retry_unix: null }),
    ).toBe("Pool Rate limited");
    expect(poolLabelText({ kind: "unreachable" })).toBe(
      "Cannot reach provider",
    );
    expect(poolLabelText({ kind: "not_working" })).toBe("Pool Not working");
  });
});

describe("the aggregated card pool line", () => {
  it("says N keys configured · aggregate, exactly the owner's shape", () => {
    expect(
      cardPoolLine(
        family({
          configuredKeys: 47,
          enabledKeys: 47,
          card: { kind: "healthy" },
        }),
      ),
    ).toBe("47 keys configured · Healthy");
  });

  it("adds M enabled whenever disabled slots make counts differ", () => {
    expect(
      cardPoolLine(
        family({
          configuredKeys: 5,
          enabledKeys: 3,
          card: { kind: "degraded" },
        }),
      ),
    ).toBe("5 keys configured · 3 enabled · Degraded");
  });

  it("aggregates never hide blockers behind big counts", () => {
    expect(
      cardLabelText({ kind: "unavailable", pools_need_attention: 2 }),
    ).toBe("Unavailable — 2 enabled pools need attention");
    expect(cardLabelText({ kind: "needs_unlock" })).toBe("Needs unlock");
    expect(
      cardLabelText({ kind: "uniform", label: { kind: "unreachable" } }),
    ).toBe("Cannot reach provider");
  });
});

describe("accessible enumeration and slot copy", () => {
  it("enumerates every non-zero state count, disabled separately", () => {
    expect(
      countsSummary({
        ...COUNTS,
        working: 2,
        invalid: 1,
        disabled: 3,
      }),
    ).toBe("2 working, 1 not working, 3 disabled");
    expect(countsSummary(COUNTS)).toBe("no keys");
  });

  it("slot copy never calls missing bytes provider-invalid", () => {
    expect(slotHealthText("missing_bytes")).toBe("Key needs re-entry");
    expect(slotHealthText("unreachable")).toBe("Unreachable");
    expect(slotHealthText("invalid")).toBe("Not working");
  });
});

describe("bounded pool-scale rendering", () => {
  it("caps at the page size until asked, exact at the boundary", () => {
    const slots = Array.from({ length: 53 }, (_, index) => index);
    const capped = visibleSlots(slots, false, 20);
    expect(capped.visible.length).toBe(20);
    expect(capped.hidden).toBe(33);
    expect(visibleSlots(slots, true, 20).hidden).toBe(0);
    expect(visibleSlots(slots.slice(0, 20), false, 20).hidden).toBe(0);
  });
});

describe("active models are facts with overflow", () => {
  it("compacts with a +N count and yields null when empty", () => {
    expect(activeModelsLine([])).toBeNull();
    expect(activeModelsLine(["a", "b"])).toBe("a · b");
    expect(activeModelsLine(["a", "b", "c", "d", "e"])).toBe(
      "a · b · c +2 more",
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

function activeModelsLine(
  ...args: Parameters<typeof rawactiveModelsLine>
): ReturnType<typeof rawactiveModelsLine> {
  const value = rawactiveModelsLine(...args);
  return value == null ? value : value.replace(/[\u2066-\u2069]/g, "");
}
