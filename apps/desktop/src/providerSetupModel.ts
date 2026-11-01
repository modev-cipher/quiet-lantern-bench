import type { ProviderFamilyView, SlotStateCounts } from "./providersModel";

export type Support = "supported" | "unsupported" | "unknown";
export interface ProviderDefinition {
  id: string;
  name: string;
  group: "main" | "others" | "local";
  authentication:
    "bearer" | "google_api_key" | "deepgram_token" | "optional_bearer";
  custodyId: string;
  providerType: string | null;
  baseUrl: string | null;
  modelsPath: string;
  keyUrl: string | null;
  privacyUrl: string | null;
  documentationUrl: string;
  apiVersion: string | null;
  local: boolean;
  preferredScript: string[];
  previewModels?: string[];
  retirements?: Record<string, string>;
  preferredNarration: string[];
  voices: string[];
  reviewedOn: string;
}

export interface ModelDescriptor {
  id: string;
  name: string;
  script: Support;
  vision: Support;
  narration: Support;
  active: boolean;
  preview: boolean;
  contextWindow: number | null;
  outputLimit: number | null;
  supportedParameters: string[];
  retirement: string | null;
  source: string;
  inputModalities?: string[];
  outputModalities?: string[];
  lifecycle?: string;
  family?: string | null;
  releasedAt?: string | null;
  protocols?: string[];
  protocolCapabilities?: Record<string, unknown> | null;
  effortLevels?: string[];
}

export interface Recommendations {
  script: string | null;
  narration: string | null;
  voice: string | null;
}

export interface ProviderCatalog {
  providerId: string;
  models: ModelDescriptor[];
  voices: { id: string; name: string; language: string | null }[];
  recommendations: Recommendations;
  configured?: Recommendations;
  revision: string;
  fetchedUnix: number;
  complete: boolean;
  nextCursor: string | null;
  voicesComplete: boolean;
  credentialRequired: boolean;
}

export function identityForUrl(
  directory: ProviderDefinition[],
  custody: string,
  url: string,
): string {
  if (
    custody === "openai_compat" &&
    ["https://api.openai.com", "https://api.openai.com/v1"].includes(url)
  )
    return "openai";
  return (
    directory.find(
      (entry) => entry.custodyId === custody && entry.baseUrl === url,
    )?.id ?? "custom"
  );
}

export function projectProviders(
  directory: ProviderDefinition[],
  families: ProviderFamilyView[],
): ProviderFamilyView[] {
  return directory.map((definition) => {
    const projected = families.find(
      (family) =>
        family.id === definition.id &&
        family.custodyId === definition.custodyId,
    );
    if (projected) return projected;
    const source = families.find(
      (family) => family.id === definition.custodyId,
    );
    const belongs = (url: string) =>
      identityForUrl(directory, definition.custodyId, url) === definition.id;
    const connections = (source?.connections ?? []).filter((connection) =>
      belongs(
        connection.baseUrl ??
          (connection.providerType === "openai_tts"
            ? "https://api.openai.com"
            : (definition.baseUrl ?? "")),
      ),
    );
    const pools = (source?.pools ?? []).filter((pool) => belongs(pool.baseUrl));
    const counts: SlotStateCounts = {
      working: 0,
      checking: 0,
      unverified: 0,
      invalid: 0,
      rate_limited: 0,
      unreachable: 0,
      missing_bytes: 0,
      disabled: 0,
    };
    for (const pool of pools)
      for (const key of Object.keys(counts) as (keyof SlotStateCounts)[])
        counts[key] += pool.counts[key];
    const completeFamily = definition.custodyId !== "openai_compat";
    const configuredKeys = completeFamily
      ? (source?.configuredKeys ?? 0)
      : pools.reduce((count, pool) => count + pool.configured, 0);
    const enabledKeys = completeFamily
      ? (source?.enabledKeys ?? 0)
      : pools.reduce((count, pool) => count + pool.operational, 0);
    const card =
      completeFamily && source
        ? source.card
        : source?.vaultLocked && configuredKeys > 0
          ? { kind: "needs_unlock" as const }
          : configuredKeys === 0 && connections.length === 0
            ? { kind: "not_configured" as const }
            : counts.working > 0
              ? { kind: "healthy" as const }
              : pools[0]
                ? { kind: "uniform" as const, label: pools[0].label }
                : { kind: "not_configured" as const };
    return {
      id: definition.id,
      custodyId: definition.custodyId,
      displayName: definition.name,
      capabilities: completeFamily
        ? (source?.capabilities ?? [])
        : [
            ...new Set(
              connections.flatMap((connection) => connection.capabilities),
            ),
          ],
      card,
      counts: completeFamily && source ? source.counts : counts,
      configuredKeys,
      enabledKeys,
      connections,
      disabledConnections: connections.filter(
        (connection) => !connection.enabled,
      ).length,
      activeModels: [
        ...new Set(
          connections
            .filter((connection) => connection.enabled)
            .map((connection) => connection.defaultModel),
        ),
      ],
      vaultLocked: source?.vaultLocked ?? false,
      retentionLinks: definition.privacyUrl
        ? [{ kind: definition.id, url: definition.privacyUrl }]
        : [],
      pools,
    };
  });
}
