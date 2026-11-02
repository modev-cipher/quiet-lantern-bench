import { providerDisplayName } from "../src/i18n/providerNames";
import { expect, test, type Page, type Locator } from "@playwright/test";

import { fragment, msg, storeError } from "./i18nText";
import { fixtureBook, installTauriMock } from "./tauriMock";
import type {
  CastConsentRow,
  PoolSlotView,
  ProviderFamilyView,
  SlotHealth,
} from "../src/providersModel";

// S-5 Settings → Providers (Hermes pre-brief acceptance gates that live at
// the UI layer; PF-PH1 truth lives in Rust). GALLERY_SHOTS=1 captures the
// design gallery states in both themes.

const COUNTS0 = {
  working: 0,
  checking: 0,
  unverified: 0,
  invalid: 0,
  rate_limited: 0,
  unreachable: 0,
  missing_bytes: 0,
  disabled: 0,
};

function slot(
  index: number,
  health: SlotHealth,
  over: Partial<PoolSlotView> = {},
): PoolSlotView {
  return {
    account: `https://generativelanguage.googleapis.com#${index}`,
    slot: index,
    label: `Key ${index + 1}`,
    last4: String(1000 + index).slice(-4),
    health,
    dialEligible: health === "working" || health === "unverified",
    retryAfterUnixMs: null,
    addedAt: 1,
    lastUsed: null,
    ...over,
  };
}

function geminiFamily(
  over: Partial<ProviderFamilyView> = {},
): ProviderFamilyView {
  return {
    id: "gemini",
    displayName: "Google Gemini",
    capabilities: ["Scripting", "Voice Tutor"],
    card: { kind: "healthy" },
    counts: { ...COUNTS0, working: 2 },
    configuredKeys: 2,
    enabledKeys: 2,
    connections: [
      {
        id: "c-gem",
        name: "Work Gemini",
        providerType: "gemini",
        hostname: null,
        baseUrl: null,
        defaultModel: "gemini-2.5-flash",
        capabilities: ["script_llm"],
        keyRequired: true,
        enabled: true,
        keyPresent: true,
        keyLast4: "1000",
      },
    ],
    disabledConnections: 0,
    activeModels: ["gemini-2.5-flash"],
    vaultLocked: false,
    retentionLinks: [
      { kind: "gemini", url: "https://ai.google.dev/gemini-api/terms" },
    ],
    pools: [
      {
        baseUrl: "https://generativelanguage.googleapis.com",
        label: { kind: "healthy" },
        counts: { ...COUNTS0, working: 2 },
        configured: 2,
        operational: 2,
        voiceTutorBuiltIn: true,
        slots: [slot(0, "working"), slot(1, "working")],
      },
    ],
    ...over,
  };
}

function emptyFamily(id: string, name: string): ProviderFamilyView {
  return {
    id,
    displayName: name,
    capabilities: ["Scripting"],
    card: { kind: "not_configured" },
    counts: COUNTS0,
    configuredKeys: 0,
    enabledKeys: 0,
    connections: [],
    disabledConnections: 0,
    activeModels: [],
    vaultLocked: false,
    retentionLinks: [],
    pools: [],
  };
}

async function openProviders(
  page: Page,
  families: ProviderFamilyView[],
  consents: CastConsentRow[] = [],
) {
  await installTauriMock(page, {
    books: [],
    providersScenario: { families, consents },
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-providers").click();
  await expect(page.getByTestId("providers-page")).toBeVisible();
}

const gallery = process.env.GALLERY_SHOTS === "1";
const OUT = "../../docs/design/gallery";

async function shotBothThemes(page: Page, name: string) {
  if (!gallery) return;
  for (const theme of ["light", "dark"]) {
    await page.evaluate((value) => {
      document.documentElement.setAttribute("data-theme", value);
    }, theme);
    await page.waitForTimeout(300);
    await page.screenshot({ path: `${OUT}/providers-${name}-${theme}.png` });
  }
  await page.evaluate(() => {
    document.documentElement.setAttribute("data-theme", "light");
  });
}

test("zero configured: calm cards, no key rows, no marketing wall, deliberate add flow", async ({
  page,
}) => {
  await openProviders(page, [
    emptyFamily("gemini", "Google Gemini"),
    emptyFamily("anthropic", "Anthropic"),
    emptyFamily("openai_compat", "OpenAI-compatible"),
  ]);
  await expectProviderText(
    page.getByTestId("provider-card-gemini"),
    msg("providers.copy.80"),
  );
  // No raw key vocabulary or rows at top level, ever (gate 1).
  await expect(page.getByTestId("providers-page")).not.toContainText("····");
  expect(await page.locator("li[data-testid^=pool-slot-]").count()).toBe(0);
  await shotBothThemes(page, "zero-configured");
});

test("a healthy family card carries the owner's aggregated pool line", async ({
  page,
}) => {
  await openProviders(page, [
    geminiFamily({ configuredKeys: 47, enabledKeys: 47 }),
  ]);
  await expectProviderText(
    page.getByTestId("provider-pool-line-gemini"),
    `${msg("providers.keysConfigured", { count: 47 })} · ${msg("providers.copy.83")}`,
  );
  await shotBothThemes(page, "card-healthy");
});

test("top level stays bounded and key-free at pool scale (53 keys)", async ({
  page,
}) => {
  const slots = Array.from({ length: 53 }, (_, index) =>
    slot(index, index === 3 ? "invalid" : "working"),
  );
  const big = geminiFamily({
    card: { kind: "degraded" },
    counts: { ...COUNTS0, working: 52, invalid: 1 },
    configuredKeys: 53,
    enabledKeys: 53,
    pools: [
      {
        baseUrl: "https://generativelanguage.googleapis.com",
        label: { kind: "degraded", need_attention: 1 },
        counts: { ...COUNTS0, working: 52, invalid: 1 },
        configured: 53,
        operational: 53,
        voiceTutorBuiltIn: true,
        slots,
      },
    ],
  });
  await openProviders(page, [big]);
  // Gate 1: bounded card, zero slot rows at top level.
  expect(await page.locator("li[data-testid^=pool-slot-]").count()).toBe(0);
  const card = page.getByTestId("provider-card-gemini");
  await expectProviderText(
    card,
    msg("providers.keysConfigured", { count: 53 }),
  );
  const height = await card.boundingBox().then((box) => box?.height ?? 0);
  expect(height).toBeLessThan(160);

  // Sub-page: bounded rendering — 20 rows, then explicit Show all (gate 5).
  await card.click();
  await openAdvanced(page);
  await expect(page.getByTestId("provider-detail-gemini")).toBeVisible();
  expect(await page.locator("li[data-testid^=pool-slot-]").count()).toBe(20);
  await shotBothThemes(page, "pool-50-bounded");
  await page
    .getByTestId("pool-show-all-https://generativelanguage.googleapis.com")
    .click();
  expect(await page.locator("li[data-testid^=pool-slot-]").count()).toBe(53);
  await shotBothThemes(page, "pool-50-expanded");
  // Breadcrumb Back returns to the card grid (IA ruling).
  await page.getByTestId("providers-back").click();
  await expect(page.getByTestId("provider-cards")).toBeVisible();
});

test("health-state vocabulary renders typed labels, never zero-count noise", async ({
  page,
}) => {
  const states = geminiFamily({
    card: { kind: "degraded" },
    counts: {
      ...COUNTS0,
      working: 1,
      invalid: 1,
      rate_limited: 1,
      unreachable: 1,
      missing_bytes: 1,
      disabled: 1,
    },
    configuredKeys: 6,
    enabledKeys: 5,
    pools: [
      {
        baseUrl: "https://generativelanguage.googleapis.com",
        label: { kind: "degraded", need_attention: 4 },
        counts: {
          ...COUNTS0,
          working: 1,
          invalid: 1,
          rate_limited: 1,
          unreachable: 1,
          missing_bytes: 1,
          disabled: 1,
        },
        configured: 6,
        operational: 5,
        voiceTutorBuiltIn: true,
        slots: [
          slot(0, "working"),
          slot(1, "invalid"),
          slot(2, "rate_limited", { retryAfterUnixMs: Date.now() + 90_000 }),
          slot(3, "unreachable"),
          slot(4, "missing_bytes", { last4: null }),
          slot(5, "disabled"),
        ],
      },
    ],
  });
  await openProviders(page, [states]);
  await expectProviderText(
    page.getByTestId("provider-pool-line-gemini"),
    `${msg("providers.keysConfigured", { count: 6 })}${msg("providers.enabledCount", { count: 5 })} · ${msg("providers.copy.84")}`,
  );
  await page.getByTestId("provider-card-gemini").click();
  await openAdvanced(page);
  const pool = page.getByTestId(
    "pool-label-https://generativelanguage.googleapis.com",
  );
  await expectProviderText(pool, msg("providers.poolDegraded", { count: 4 }));
  await expectProviderText(
    page.getByTestId(
      "pool-slot-state-https://generativelanguage.googleapis.com#4",
    ),
    msg("providers.copy.90"),
  );
  await expectProviderText(
    page.getByTestId(
      "pool-slot-state-https://generativelanguage.googleapis.com#3",
    ),
    msg("providers.copy.89"),
  );
  await shotBothThemes(page, "pool-mixed-states");
});

test("locked vault shows the unlock remedy on the sub-page", async ({
  page,
}) => {
  await openProviders(page, [
    geminiFamily({
      card: { kind: "needs_unlock" },
      vaultLocked: true,
      pools: [
        {
          baseUrl: "https://generativelanguage.googleapis.com",
          label: { kind: "pool_locked" },
          counts: COUNTS0,
          configured: 2,
          operational: 2,
          voiceTutorBuiltIn: true,
          slots: [],
        },
      ],
    }),
  ]);
  await expectProviderText(
    page.getByTestId("provider-pool-line-gemini"),
    msg("providers.copy.82"),
  );
  await page.getByTestId("provider-card-gemini").click();
  await openAdvanced(page);
  await expect(page.getByTestId("providers-vault-locked")).toBeVisible();
  await shotBothThemes(page, "vault-locked");
});

test("consent rows list the exact tuple and revoke through the store", async ({
  page,
}) => {
  await openProviders(
    page,
    [geminiFamily()],
    [
      {
        id: "consent-1",
        book_id: "book-1",
        connection_id: "c-gem",
        provider: "gemini",
        phase: "script",
        hosts: ["generativelanguage.googleapis.com"],
        data_categories: ["source_text"],
        granted_unix: 1,
        revoked_unix: null,
      },
    ],
  );
  await page.getByTestId("provider-card-gemini").click();
  await openAdvanced(page);
  const row = page.getByTestId("consent-row-consent-1");
  await expectProviderText(row, "Work Gemini");
  await expectProviderText(row, msg("providers.data.source"));
  await expectProviderText(row, "generativelanguage.googleapis.com");
  await shotBothThemes(page, "consents");
  // The REAL flow: revocation mutates the store (revoked_unix), the list
  // re-read reflects it, and the row leaves the active list. Counting the
  // command invocation alone was a false green — the old mock threw.
  await page.getByTestId("consent-revoke-consent-1").click();
  await expect(page.getByTestId("consent-row-consent-1")).toHaveCount(0);
});

test("a rejected consent revocation surfaces its error and keeps the row", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    providersScenario: {
      families: [geminiFamily()],
      consents: [
        {
          id: "consent-1",
          book_id: "book-1",
          connection_id: "c-gem",
          provider: "gemini",
          phase: "script",
          hosts: ["generativelanguage.googleapis.com"],
          data_categories: ["source_text"],
          granted_unix: 1,
          revoked_unix: null,
        },
      ],
      revokeFails: ["consent-1"],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-providers").click();
  await page.getByTestId("provider-card-gemini").click();
  await openAdvanced(page);
  await page.getByTestId("consent-revoke-consent-1").click();
  await expectProviderText(
    page.getByTestId("providers-error"),
    fragment("providers.revokeFailed", 0),
  );
  await expect(page.getByTestId("consent-row-consent-1")).toBeVisible();
  await page.getByTestId("providers-error-dismiss").click();
  await expect(page.getByTestId("providers-error")).toHaveCount(0);
});

test("page entry never probes; Test pool is the only bulk trigger", async ({
  page,
}) => {
  await openProviders(page, [geminiFamily()]);
  await page.getByTestId("provider-card-gemini").click();
  await openAdvanced(page);
  const before = await page.evaluate(
    () =>
      (
        window as unknown as { __invokedCommands: string[] }
      ).__invokedCommands.filter(
        (cmd) => cmd === "provider_pool_test" || cmd === "probe_voice_keys",
      ).length,
  );
  expect(before).toBe(0);
  await page
    .getByTestId("pool-test-https://generativelanguage.googleapis.com")
    .click();
  const after = await page.evaluate(
    () =>
      (
        window as unknown as { __invokedCommands: string[] }
      ).__invokedCommands.filter((cmd) => cmd === "provider_pool_test").length,
  );
  expect(after).toBe(1);
});

function compatFamily(
  over: Partial<ProviderFamilyView> = {},
): ProviderFamilyView {
  return {
    ...emptyFamily("openai_compat", "OpenAI-compatible"),
    card: { kind: "healthy" },
    counts: { ...COUNTS0, working: 1 },
    configuredKeys: 1,
    enabledKeys: 1,
    connections: [
      {
        id: "c-router",
        name: "Work OpenRouter",
        providerType: "openai_compatible",
        hostname: "openrouter.ai",
        baseUrl: "https://openrouter.ai/api/v1",
        defaultModel: "some/model",
        capabilities: ["script_llm"],
        keyRequired: true,
        enabled: true,
        keyPresent: true,
        keyLast4: "ab12",
      },
    ],
    activeModels: ["some/model"],
    pools: [
      {
        baseUrl: "https://openrouter.ai/api/v1",
        label: { kind: "healthy" },
        counts: { ...COUNTS0, working: 1 },
        configured: 1,
        operational: 1,
        voiceTutorBuiltIn: false,
        slots: [
          slot(0, "working", { account: "https://openrouter.ai/api/v1#0" }),
        ],
      },
    ],
    ...over,
  };
}

async function openAdvanced(page: Page) {
  const manage = page.getByTestId("provider-manage-keys");
  if (await manage.count()) await manage.click();
  else if (await page.getByTestId("provider-advanced").count())
    await page.getByTestId("provider-advanced").click();
}

async function providerCalls(page: Page) {
  return page.evaluate(
    () =>
      (
        window as unknown as {
          __providerCalls: Array<{
            cmd: string;
            args: Record<string, unknown>;
          }>;
        }
      ).__providerCalls,
  );
}

test("adding a key to a FIXED family sends no base URL and lands (blocker 1)", async ({
  page,
}) => {
  await openProviders(page, [geminiFamily()]);
  await page.getByTestId("provider-card-gemini").click();
  await openAdvanced(page);
  const url = "https://generativelanguage.googleapis.com";
  await page.getByTestId(`pool-add-key-${url}`).fill("g-key-000011112222");
  await page.getByTestId(`pool-add-submit-${url}`).click();
  // The mock enforces the REAL backend law: a fixed provider refuses any
  // supplied base URL, so a receipt of "1 key added" proves none was sent.
  await expectProviderText(
    page.getByTestId(`pool-receipt-${url}`),
    msg("providers.copy.43"),
  );
});

test("compat key add walks the URL-consent gate, then bulk import works (blocker 1)", async ({
  page,
}) => {
  await openProviders(page, [compatFamily()]);
  await page.getByTestId("provider-card-openrouter").click();
  await openAdvanced(page);
  const url = "https://openrouter.ai/api/v1";
  await page.getByTestId(`pool-add-key-${url}`).fill("sk-or-000011112222");
  await page.getByTestId(`pool-add-submit-${url}`).click();
  // Unconsented endpoint: the consent prompt appears, nothing stored yet.
  await expectProviderText(
    page.getByTestId("pool-url-consent"),
    "openrouter.ai",
  );
  await page.getByTestId("pool-url-consent-allow").click();
  await expectProviderText(
    page.getByTestId(`pool-receipt-${url}`),
    msg("providers.copy.43"),
  );
  // Bulk paste now rides the recorded consent.
  await page.getByTestId(`pool-bulk-open-${url}`).click();
  await page
    .getByTestId(`pool-bulk-text-${url}`)
    .fill("A=sk-or-1111\nB=sk-or-2222\n# comment");
  await page.getByTestId(`pool-bulk-submit-${url}`).click();
  await expectProviderText(
    page.getByTestId(`pool-receipt-${url}`),
    msg("providers.importReceipt", { imported: 2, rejected: 0, count: 0 }),
  );
});

test("locked vault unlocks (or goes session-only) right on the sub-page (blocker 2)", async ({
  page,
}) => {
  await openProviders(page, [
    geminiFamily({ card: { kind: "needs_unlock" }, vaultLocked: true }),
  ]);
  await page.getByTestId("provider-card-gemini").click();
  await openAdvanced(page);
  await expect(page.getByTestId("providers-vault-locked")).toBeVisible();
  // Wrong passphrase surfaces the error; the remedy stays reachable.
  await page.getByTestId("providers-vault-passphrase").fill("wrong");
  await page.getByTestId("providers-vault-unlock").click();
  await expectProviderText(
    page.getByTestId("providers-vault-error"),
    msg("error.wrongPassphrase"),
  );
  await page.getByTestId("providers-vault-passphrase").fill("letmein");
  await page.getByTestId("providers-vault-unlock").click();
  const unlocks = await page.evaluate(
    () =>
      (
        window as unknown as { __invokedCommands: string[] }
      ).__invokedCommands.filter((cmd) => cmd === "unlock_voice_key_vault")
        .length,
  );
  expect(unlocks).toBe(2);
});

test("rename and disable round-trip the REAL backend contract (blocker 3)", async ({
  page,
}) => {
  await openProviders(page, [compatFamily()]);
  await page.getByTestId("provider-card-openrouter").click();
  await openAdvanced(page);
  const row = page.getByTestId("connection-row-c-router");
  await expectProviderText(row, "some/model");
  await expectProviderText(row, "openrouter.ai");
  await page.getByTestId("connection-toggle-c-router").click();
  const calls = await providerCalls(page);
  const update = calls.find(
    (call) => call.cmd === "provider_connection_update",
  );
  expect(update?.args.baseUrl).toBe("https://openrouter.ai/api/v1");
  expect(update?.args.defaultModel).toBe("some/model");
  expect(update?.args.credentialNotRequired).toBe(false);
  expect(update?.args.enabled).toBe(false);
});

test("delete impact renders the real fields and fails CLOSED (blocker 4)", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    providersScenario: {
      families: [geminiFamily()],
      deleteImpact: {
        referencingJobs: ["job-1"],
        referencingVoiceProfiles: ["Aria (cloud)"],
      },
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-providers").click();
  await page.getByTestId("provider-card-gemini").click();
  await openAdvanced(page);
  await page.getByTestId("connection-delete-c-gem").click();
  const impact = page.getByTestId("connection-impact-c-gem");
  await expectProviderText(impact, msg("providers.deleteJobs", { count: 1 }));
  await expectProviderText(impact, "Aria (cloud)");
  await expect(
    page.getByTestId("connection-delete-confirm-c-gem"),
  ).toBeVisible();
});

test("an unverifiable delete impact BLOCKS deletion (blocker 4 fail-closed)", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    providersScenario: { families: [geminiFamily()], deleteImpact: null },
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-providers").click();
  await page.getByTestId("provider-card-gemini").click();
  await openAdvanced(page);
  await page.getByTestId("connection-delete-c-gem").click();
  const impact = page.getByTestId("connection-impact-c-gem");
  await expectProviderText(impact, msg("providers.copy.62").trim());
  await expect(page.getByTestId("connection-delete-confirm-c-gem")).toHaveCount(
    0,
  );
});

test("pool test streams typed progress and cancels (blocker 6)", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    providersScenario: { families: [geminiFamily()], poolTestTotal: 40 },
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-providers").click();
  await page.getByTestId("provider-card-gemini").click();
  await openAdvanced(page);
  const url = "https://generativelanguage.googleapis.com";
  await page.getByTestId(`pool-test-${url}`).click();
  // Rendered progress arrives from the SAME key the writer used.
  await expectProviderText(
    page.getByTestId(`pool-label-${url}`),
    new RegExp(
      `${fragment("providers.progress", 0)}\\s*\\d+\\s*${fragment("providers.progress", 1)}\\s*40`,
    ),
  );
  await expect(page.getByTestId(`pool-test-${url}`)).toBeDisabled();
  await page.getByTestId(`pool-test-cancel-${url}`).click();
  // The terminal event re-enables the button well before 40 probes run.
  await expect(page.getByTestId(`pool-test-${url}`)).toBeEnabled();
  // Truthful terminal accounting: the label says how many probes actually
  // settled before the stop — never "40 of 40" for a cancelled run.
  const stoppedAfter = new RegExp(
    `${fragment("providers.stopped", 0)}\\s*(\\d+)\\s*${fragment("providers.stopped", 1)}\\s*40`,
  );
  await expectProviderText(page.getByTestId(`pool-label-${url}`), stoppedAfter);
  const label = await page.getByTestId(`pool-label-${url}`).textContent();
  const stopped = stoppedAfter.exec(
    (label ?? "").replace(/[\u2066-\u2069]/g, ""),
  );
  expect(Number(stopped?.[1])).toBeLessThan(40);
  const cancels = await page.evaluate(
    () =>
      (
        window as unknown as { __invokedCommands: string[] }
      ).__invokedCommands.filter((cmd) => cmd === "provider_pool_test_cancel")
        .length,
  );
  expect(cancels).toBe(1);
  // Streaming stays incremental: the full overview projection is re-read
  // only at entry and terminal settlement (dev StrictMode doubles mount
  // effects) — never per probe event, so the bound is far below the 40
  // events this run streamed.
  const overviewReads = await page.evaluate(
    () =>
      (
        window as unknown as { __invokedCommands: string[] }
      ).__invokedCommands.filter((cmd) => cmd === "providers_overview").length,
  );
  expect(overviewReads).toBeLessThanOrEqual(4);
});

test("an overview read failure keeps the last known state and retries (item 3)", async ({
  page,
}) => {
  await openProviders(page, [geminiFamily()]);
  await expect(page.getByTestId("provider-card-gemini")).toBeVisible();
  await page.evaluate(() => {
    (window as unknown as { __failNextOverview?: boolean }).__failNextOverview =
      true;
  });
  // A mutation triggers a reload; the reload fails; the page keeps the
  // last known state and the failure is a visible, retryable banner —
  // never a silent blank page.
  await page.getByTestId("provider-card-gemini").click();
  await openAdvanced(page);
  await page
    .getByTestId("pool-slot-toggle-https://generativelanguage.googleapis.com#0")
    .click();
  await expectProviderText(
    page.getByTestId("providers-load-failed"),
    msg("providers.copy.57").trim(),
  );
  await expect(page.getByTestId("provider-detail-gemini")).toBeVisible();
  await page.getByTestId("providers-load-retry").click();
  await expect(page.getByTestId("providers-load-failed")).toHaveCount(0);
});

test("a deep link is a DIRECT entry: goto lands on the detail with zero clicks (item 5)", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    providersScenario: { families: [geminiFamily()] },
  });
  // The app itself routes the launch address: no Settings click, no nav
  // click — the first paint after goto IS the addressed provider detail.
  await page.goto("/#providers/gemini");
  await expect(page.getByTestId("provider-detail-gemini")).toBeVisible();
  await expect(page.getByTestId("providers-page")).toBeVisible();
});

test("consent read failure keeps the rows and retries in place (item 1)", async ({
  page,
}) => {
  await openProviders(
    page,
    [geminiFamily()],
    [
      {
        id: "consent-1",
        book_id: "book-1",
        connection_id: "c-gem",
        provider: "gemini",
        phase: "script",
        hosts: ["generativelanguage.googleapis.com"],
        data_categories: ["source_text"],
        granted_unix: 1,
        revoked_unix: null,
      },
    ],
  );
  await page.getByTestId("provider-card-gemini").click();
  await openAdvanced(page);
  await expect(page.getByTestId("consent-row-consent-1")).toBeVisible();
  // The read AFTER a successful revocation fails: the section keeps the
  // last known rows (honestly stale) as a retryable state in place —
  // never a silent blank that reads as "no consents".
  await page.evaluate(() => {
    (
      window as unknown as { __failNextConsentList?: boolean }
    ).__failNextConsentList = true;
  });
  await page.getByTestId("consent-revoke-consent-1").click();
  await expectProviderText(
    page.getByTestId("consents-load-failed"),
    msg("providers.copy.56").trim(),
  );
  await expect(page.getByTestId("consent-row-consent-1")).toBeVisible();
  // Retry re-reads the store truth: the revoked row leaves, banner clears.
  await page.getByTestId("consents-load-retry").click();
  await expect(page.getByTestId("consents-load-failed")).toHaveCount(0);
  await expect(page.getByTestId("consent-row-consent-1")).toHaveCount(0);
});

test("provider detail is addressable: Back/Forward, Escape, opener focus (item 5)", async ({
  page,
}) => {
  await openProviders(page, [geminiFamily()]);
  // In-app open pushes the real address.
  await page.getByTestId("provider-card-gemini").click();
  await openAdvanced(page);
  await expect(page.getByTestId("provider-detail-gemini")).toBeVisible();
  expect(new URL(page.url()).hash).toBe("#providers/gemini");
  // Browser Back returns to the cards and restores focus to the opener.
  await page.goBack();
  await expect(page.getByTestId("provider-cards")).toBeVisible();
  await expect(page.getByTestId("provider-card-gemini")).toBeFocused();
  // Browser Forward re-enters the detail.
  await page.goForward();
  await expect(page.getByTestId("provider-detail-gemini")).toBeVisible();
  // Escape walks the same history entry back to the cards.
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("provider-cards")).toBeVisible();
  await expect(page.getByTestId("provider-card-gemini")).toBeFocused();
});

test("model discovery on the sub-page: keyed open fetches, search, apply, custom override (blocker 7 + F1)", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    castProviders: {
      scriptConnections: [],
      ttsVoices: [],
      fetchedModels: {
        models: ["luna-5.6", "luna-5.6-mini", "sol-2"],
        suggestedDefault: "luna-5.6",
      },
    },
    providersScenario: { families: [compatFamily()] },
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-providers").click();
  await page.getByTestId("provider-card-openrouter").click();
  await openAdvanced(page);
  await page.getByTestId("connection-models-c-router").click();
  // A keyed connection with no cache fetches on open (F1) — the list
  // appears without a second click, with its honest age fact.
  await expectProviderText(
    page.getByTestId("connection-models-age-c-router"),
    msg("providers.copy.67"),
  );
  await expectProviderText(
    page.getByTestId("connection-models-panel-c-router"),
    "luna-5.6-mini",
  );
  await page.getByTestId("connection-models-search-c-router").fill("sol");
  await expect(
    page.getByTestId("connection-models-panel-c-router"),
  ).not.toContainText("luna-5.6-mini");
  await page.getByTestId("connection-models-use-c-router-sol-2").click();
  const calls = await providerCalls(page);
  const update = calls.find(
    (call) => call.cmd === "provider_connection_update",
  );
  expect(update?.args.defaultModel).toBe("sol-2");
  // The explicit free-text override stands beside the list (F1).
  await page
    .getByTestId("connection-models-custom-c-router")
    .fill("hand/typed-model");
  await page.getByTestId("connection-models-custom-apply-c-router").click();
  const updates = (await providerCalls(page)).filter(
    (call) => call.cmd === "provider_connection_update",
  );
  expect(updates[updates.length - 1]?.args.defaultModel).toBe(
    "hand/typed-model",
  );
});

test("keyless-required connection without a key opens to an honest hint, no fetch (F1)", async ({
  page,
}) => {
  const family = compatFamily();
  family.connections[0].keyPresent = false;
  await openProviders(page, [family]);
  await page.getByTestId("provider-card-openrouter").click();
  await openAdvanced(page);
  await page.getByTestId("connection-models-c-router").click();
  await expectProviderText(
    page.getByTestId("connection-models-panel-c-router"),
    msg("providers.copy.53"),
  );
  const calls = await providerCalls(page);
  expect(
    calls.find((call) => call.cmd === "provider_fetch_models"),
  ).toBeUndefined();
});

test("listing failure is an inline error with the free-text fallback standing (F1)", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    providersScenario: {
      families: [compatFamily()],
      modelListError: "transport: connection refused",
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-providers").click();
  await page.getByTestId("provider-card-openrouter").click();
  await openAdvanced(page);
  await page.getByTestId("connection-models-c-router").click();
  await expectProviderText(
    page.getByTestId("connection-models-error-c-router"),
    storeError("connection refused"),
  );
  // The custom override still applies — never a dead dropdown.
  await page.getByTestId("connection-models-custom-c-router").fill("m-1");
  await page.getByTestId("connection-models-custom-apply-c-router").click();
  const update = (await providerCalls(page)).find(
    (call) => call.cmd === "provider_connection_update",
  );
  expect(update?.args.defaultModel).toBe("m-1");
});

test("named provider advanced controls keep endpoint and identity fixed", async ({
  page,
}) => {
  await openProviders(page, [compatFamily()]);
  await page.getByTestId("provider-card-openrouter").click();
  await openAdvanced(page);
  await expect(page.getByTestId("add-connection-preset")).toHaveCount(0);
  await expect(page.getByTestId("add-connection-url")).toHaveCount(0);
  await expectProviderText(
    page.getByTestId("retention-openrouter"),
    "openrouter.ai",
  );
});

test("add-connection form lists draft models through the validated seam and picks one (F1)", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    castProviders: {
      scriptConnections: [],
      ttsVoices: [],
      fetchedModels: {
        models: ["luna-5.6", "luna-5.6-mini"],
        suggestedDefault: "luna-5.6-mini",
      },
    },
    providersScenario: { families: [compatFamily()] },
  });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-providers").click();
  await page.getByTestId("provider-card-custom").click();
  // Custom with a cleared URL: listing is disabled, free text stands alone.

  await page.getByTestId("add-connection-url").fill("");
  await expect(page.getByTestId("add-connection-list-models")).toBeDisabled();
  // Back on the zero-knowledge default, listing is one click.
  await page
    .getByTestId("add-connection-url")
    .fill("http://localhost:11434/v1");
  await page.getByTestId("add-connection-keyless").check();
  await page.getByTestId("add-connection-list-models").click();
  // The suggestion pre-fills the empty model field; picking overrides it.
  await expect(page.getByTestId("add-connection-model")).toHaveValue(
    "luna-5.6-mini",
  );
  await page.getByTestId("add-connection-models-use-luna-5.6").click();
  await expect(page.getByTestId("add-connection-model")).toHaveValue(
    "luna-5.6",
  );
  const draft = (await providerCalls(page)).find(
    (call) => call.cmd === "provider_draft_models",
  );
  expect(draft?.args.providerType).toBe("openai_compatible");
  expect(draft?.args.baseUrl).toBe("http://localhost:11434/v1");
  expect(draft?.args.credentialNotRequired).toBe(true);
});

test("cloud voices are creatable from the connection sub-page (blocker 8)", async ({
  page,
}) => {
  const cartesia: ProviderFamilyView = {
    ...emptyFamily("cartesia", "Cartesia"),
    card: { kind: "healthy" },
    connections: [
      {
        id: "c-car",
        name: "Cartesia",
        providerType: "cartesia_tts",
        hostname: null,
        baseUrl: null,
        defaultModel: "sonic-3.6",
        capabilities: ["tts"],
        keyRequired: true,
        enabled: true,
        keyPresent: true,
        keyLast4: "cc11",
      },
    ],
    activeModels: ["sonic-3.6"],
  };
  await openProviders(page, [cartesia]);
  await page.getByTestId("provider-card-cartesia").click();
  await openAdvanced(page);
  await page.getByTestId("connection-add-voice-c-car").click();
  // Refusals surface: a blank voice id never lands.
  await page.getByTestId("connection-voice-name-c-car").fill("Aria");
  await page.getByTestId("connection-voice-save-c-car").click();
  await expectProviderText(
    page.getByTestId("connection-voice-notice-c-car"),
    msg("error.enterVoiceId"),
  );
  await page.getByTestId("connection-voice-id-c-car").fill("aria-voice-9");
  await page.getByTestId("connection-voice-save-c-car").click();
  await expectProviderText(
    page.getByTestId("connection-voice-notice-c-car"),
    msg("providers.copy.54"),
  );
  const calls = await providerCalls(page);
  // The first call was the refused blank one; the LAST call is the save.
  const creations = calls.filter(
    (call) => call.cmd === "create_cloud_voice_profile",
  );
  const created = creations[creations.length - 1];
  expect(created?.args.connectionId).toBe("c-car");
  expect(created?.args.voiceId).toBe("aria-voice-9");
});

test("no commercial vocabulary anywhere on the surface (O-1/R7)", async ({
  page,
}) => {
  await openProviders(page, [
    geminiFamily(),
    emptyFamily("anthropic", "Anthropic"),
  ]);
  for (const banned of [
    /upgrade/i,
    /sponsored/i,
    /best provider/i,
    /plan\b/i,
    /upsell/i,
  ]) {
    await expect(page.getByTestId("providers-page")).not.toContainText(banned);
  }
  await page.getByTestId("provider-card-gemini").click();
  await openAdvanced(page);
  for (const banned of [/upgrade/i, /sponsored/i, /recommended/i]) {
    await expect(page.getByTestId("providers-page")).not.toContainText(banned);
  }
});

test("a tutor sent to Providers can start as soon as a key lands there", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    providersScenario: { families: [geminiFamily()], consents: [] },
    voiceKeysScenario: { entries: [] },
  });
  await page.goto("/");
  await page.getByTestId("tutor-launcher").click();
  await expect(page.getByTestId("voice-session-start")).toBeDisabled();
  await page.getByTestId("voice-session-key-action").click();
  await expect(page.getByTestId("providers-page")).toBeVisible();
  await expect(page).toHaveURL(/#providers\/gemini$/);
  await openAdvanced(page);
  const url = "https://generativelanguage.googleapis.com";
  await page.getByTestId(`pool-add-key-${url}`).fill("g-key-000011112222");
  await page.getByTestId(`pool-add-submit-${url}`).click();
  await expectProviderText(
    page.getByTestId(`pool-receipt-${url}`),
    msg("providers.copy.43"),
  );

  await expect(page.getByTestId("voice-session-start")).toBeEnabled();
  await expect(page.getByTestId("voice-session-key-hint")).toHaveCount(0);
});

test("a tutor waiting on a locked vault notices the unlock made on Providers", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    providersScenario: {
      families: [geminiFamily({ vaultLocked: true })],
      consents: [],
    },
    voiceKeysScenario: {
      entries: [
        {
          baseUrl: "https://generativelanguage.googleapis.com",
          provider: "gemini",
          keyPresent: true,
          last4: "1234",
          state: "unchecked",
          custom: false,
        },
      ],
      storage: { level: "encrypted_file", locked: true },
    },
  });
  await page.goto("/");
  await page.getByTestId("tutor-launcher").click();
  const hint = page.getByTestId("voice-session-key-hint");
  await expectProviderText(
    hint,
    msg("voice.hint.locked", { provider: providerDisplayName("gemini") }),
  );
  await page.getByTestId("voice-session-key-action").click();
  await expect(page).toHaveURL(/#providers\/gemini$/);
  await openAdvanced(page);
  await page.getByTestId("providers-vault-passphrase").fill("letmein");
  await page.getByTestId("providers-vault-unlock").click();

  await expect(page.getByTestId("voice-session-start")).toBeEnabled();
  await expect(hint).toHaveCount(0);
});

async function expectProviderText(locator: Locator, expected: string | RegExp) {
  const text = expect.poll(async () =>
    ((await locator.textContent()) ?? "").replace(/[\u2066-\u2069]/g, ""),
  );
  if (typeof expected === "string") await text.toContain(expected);
  else await text.toMatch(expected);
}
