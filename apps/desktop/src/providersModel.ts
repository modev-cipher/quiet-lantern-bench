import { t as providerText } from "./i18n";
// Settings → Providers view model (S-5). Every health fact arrives TYPED
// from the backend (PF-PH1): this module renders copy from tagged unions
// and typed counts — it never parses labels back into meaning, and no key
// bytes exist anywhere in these types (masked last-four only).

export type SlotHealth =
  | "working"
  | "checking"
  | "unverified"
  | "invalid"
  | "rate_limited"
  | "unreachable"
  | "missing_bytes"
  | "disabled";

export interface SlotStateCounts {
  working: number;
  checking: number;
  unverified: number;
  invalid: number;
  rate_limited: number;
  unreachable: number;
  missing_bytes: number;
  disabled: number;
}

export type PoolLabel =
  | { kind: "connection_pool_disabled" }
  | { kind: "no_key_required" }
  | { kind: "pool_locked" }
  | { kind: "checking"; completed: number; total: number }
  | { kind: "no_keys_configured" }
  | { kind: "keys_disabled" }
  | { kind: "healthy" }
  | { kind: "degraded"; need_attention: number }
  | { kind: "unverified" }
  | { kind: "keys_needed" }
  | { kind: "rate_limited"; earliest_retry_unix: number | null }
  | { kind: "unreachable" }
  | { kind: "not_working" };

export type ProviderCardLabel =
  | { kind: "not_configured" }
  | { kind: "disabled" }
  | { kind: "needs_unlock" }
  | { kind: "checking_all"; completed: number; total: number }
  | { kind: "degraded_checking"; checking_pools: number }
  | { kind: "checking_some"; checking: number; total: number }
  | { kind: "healthy" }
  | { kind: "degraded" }
  | { kind: "uniform"; label: PoolLabel }
  | { kind: "unavailable"; pools_need_attention: number };

export interface PoolSlotView {
  account: string;
  slot: number;
  label: string | null;
  last4: string | null;
  health: SlotHealth;
  dialEligible: boolean;
  retryAfterUnixMs: number | null;
  addedAt: number;
  lastUsed: number | null;
}

export interface PoolView {
  baseUrl: string;
  label: PoolLabel;
  counts: SlotStateCounts;
  configured: number;
  operational: number;
  voiceTutorBuiltIn: boolean;
  slots: PoolSlotView[];
}

export interface ProviderConnectionView {
  id: string;
  name: string;
  providerType: string;
  hostname: string | null;
  /// Canonical base URL for custom-endpoint connections; null for fixed
  /// providers (their URLs are adapter-owned).
  baseUrl: string | null;
  defaultModel: string;
  capabilities: string[];
  keyRequired: boolean;
  enabled: boolean;
  keyPresent: boolean;
  keyLast4: string | null;
}

/// What deleting a connection would strand — the REAL backend fields.
export interface DeleteImpact {
  referencingJobs: string[];
  referencingVoiceProfiles: string[];
}

/// Fail-CLOSED delete-impact parse (checkpoint-3 blocker 4): anything that
/// is not the exact backend shape returns null, and the caller must BLOCK
/// deletion — never render "nothing references it" from absent fields.
export function parseDeleteImpact(raw: unknown): DeleteImpact | null {
  if (typeof raw !== "object" || raw == null) return null;
  const record = raw as Record<string, unknown>;
  const strings = (value: unknown): value is string[] =>
    Array.isArray(value) && value.every((item) => typeof item === "string");
  if (
    !strings(record.referencingJobs) ||
    !strings(record.referencingVoiceProfiles)
  ) {
    return null;
  }
  return {
    referencingJobs: record.referencingJobs,
    referencingVoiceProfiles: record.referencingVoiceProfiles,
  };
}

/// The typed pool-test progress event (one writer, one reader key).
export interface PoolTestProgress {
  provider: string;
  baseUrl: string;
  completed: number;
  total: number;
  done: boolean;
  cancelled: boolean;
}

/// The ONE key both the event listener and the sub-page renderer use —
/// checkpoint-3 blocker 6 was a writer/reader key mismatch.
export function poolTestKey(provider: string, baseUrl: string): string {
  return `${provider} ${baseUrl}`;
}

/// The per-connection fetched-model cache row.
export interface CachedModels {
  models: string[];
  suggestedDefault: string | null;
  fetchedUnix: number;
}

/// Honest cache-age copy for the models list (blocker 7): fresh under a
/// day, aged in days, stale wording past 30 days.
export function modelsCacheAge(fetchedUnix: number, nowUnix: number): string {
  const days = Math.floor(Math.max(0, nowUnix - fetchedUnix) / 86_400);
  if (days < 1) return providerText("providers.copy.67");
  if (days > 30) return providerText("providers.cacheStale", { count: days });
  return providerText("providers.cacheAge", { count: days });
}

export interface RetentionLink {
  kind: string;
  url: string;
}

/// One recorded cast consent as `cast_consent_list` returns it (the store
/// shape is snake_case on the wire). Shared with the e2e mock so the
/// boundary is compile-checked (shape-gate philosophy, round-3 item 2).
export interface CastConsentRow {
  id: string;
  book_id: string;
  connection_id: string;
  provider: string;
  phase: string;
  hosts: string[];
  data_categories: string[];
  granted_unix: number;
  revoked_unix: number | null;
}

export interface ProviderFamilyView {
  id: string;
  custodyId?: string;
  displayName: string;
  capabilities: string[];
  card: ProviderCardLabel;
  counts: SlotStateCounts;
  configuredKeys: number;
  enabledKeys: number;
  connections: ProviderConnectionView[];
  disabledConnections: number;
  activeModels: string[];
  vaultLocked: boolean;
  retentionLinks: RetentionLink[];
  pools?: PoolView[];
}

// ---- calm copy, rendered from typed values only ----

export function poolLabelText(label: PoolLabel): string {
  switch (label.kind) {
    case "connection_pool_disabled":
      return providerText("providers.copy.68");
    case "no_key_required":
      return providerText("providers.copy.69");
    case "pool_locked":
      return providerText("providers.copy.70");
    case "checking":
      return label.total > 0
        ? providerText("providers.progress", {
            completed: label.completed,
            total: label.total,
          })
        : providerText("providers.copy.71");
    case "no_keys_configured":
      return providerText("providers.copy.72");
    case "keys_disabled":
      return providerText("providers.copy.73");
    case "healthy":
      return providerText("providers.copy.74");
    case "degraded":
      return providerText("providers.poolDegraded", {
        count: label.need_attention,
      });
    case "unverified":
      return providerText("providers.copy.75");
    case "keys_needed":
      return providerText("providers.copy.76");
    case "rate_limited":
      return label.earliest_retry_unix != null
        ? `Pool Rate limited · retry ${new Date(
            label.earliest_retry_unix * 1000,
          ).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`
        : providerText("providers.copy.77");
    case "unreachable":
      return providerText("providers.copy.78");
    case "not_working":
      return providerText("providers.copy.79");
  }
}

export function cardLabelText(card: ProviderCardLabel): string {
  switch (card.kind) {
    case "not_configured":
      return providerText("providers.copy.80");
    case "disabled":
      return providerText("providers.copy.81");
    case "needs_unlock":
      return providerText("providers.copy.82");
    case "checking_all":
      return providerText("providers.copy.71");
    case "degraded_checking":
      return providerText("providers.degradedChecking", {
        count: card.checking_pools,
      });
    case "checking_some":
      return providerText("providers.checkingPools", {
        completed: card.checking,
        total: card.total,
      });
    case "healthy":
      return providerText("providers.copy.83");
    case "degraded":
      return providerText("providers.copy.84");
    case "uniform":
      return poolLabelText(card.label);
    case "unavailable":
      return providerText("providers.unavailablePools", {
        count: card.pools_need_attention,
      });
  }
}

/// The one aggregated pool line the top-level card carries:
/// "47 keys configured · Pool Healthy" (+ " · N enabled" when disabled
/// slots make the counts differ — the count never hides capacity).
export function cardPoolLine(family: ProviderFamilyView): string {
  const keys = providerText("providers.keysConfigured", {
    count: family.configuredKeys,
  });
  const enabled =
    family.enabledKeys !== family.configuredKeys
      ? providerText("providers.enabledCount", { count: family.enabledKeys })
      : "";
  return `${keys}${enabled} · ${cardLabelText(family.card)}`;
}

export function slotHealthText(health: SlotHealth): string {
  switch (health) {
    case "working":
      return providerText("providers.copy.85");
    case "checking":
      return providerText("providers.copy.71");
    case "unverified":
      return providerText("providers.copy.86");
    case "invalid":
      return providerText("providers.copy.87");
    case "rate_limited":
      return providerText("providers.copy.88");
    case "unreachable":
      return providerText("providers.copy.89");
    case "missing_bytes":
      return providerText("providers.copy.90");
    case "disabled":
      return providerText("providers.copy.81");
  }
}

/// Accessible enumeration of every state count plus disabled separately
/// (composition clarification 2 — the aggregate never hides a blocker).
export function countsSummary(counts: SlotStateCounts): string {
  const parts: string[] = [];
  for (const health of Object.keys(counts) as SlotHealth[]) {
    if (counts[health] > 0)
      parts.push(
        providerText("providers.countState", {
          count: counts[health],
          state: providerText(`providers.state.${health}`),
        }),
      );
  }
  return parts.length > 0
    ? parts.join(", ")
    : providerText("providers.copy.91");
}

/// Compact active-model facts with an overflow count (card rule 5).
export function activeModelsLine(models: string[], max = 3): string | null {
  if (models.length === 0) return null;
  const shown = models.slice(0, max).join(" · ");
  const overflow = models.length - max;
  return overflow > 0
    ? providerText("providers.modelsOverflow", {
        models: shown,
        count: overflow,
      })
    : shown;
}

/// Bounded rendering for pool-scale lists (sub-page rule 3): the page
/// renders at most `page` rows until the user asks for more.
export function visibleSlots<T>(
  slots: T[],
  showAll: boolean,
  page = 20,
): { visible: T[]; hidden: number } {
  if (showAll || slots.length <= page) {
    return { visible: slots, hidden: 0 };
  }
  return { visible: slots.slice(0, page), hidden: slots.length - page };
}
