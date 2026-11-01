import type {
  CastCloudVoiceOption,
  CastConnectionOption,
  CastProviderOptions,
} from "./castProviderModel";
import type {
  VoiceProfileSummary,
} from "./jobFormModel";
import type {
  CatalogModelEntry,
} from "./modelCatalogModel";
import { t } from "./i18n";

export type CastCategory = "tts" | "llm";

export type RenderPlanMode = "hidden" | "fixed-auto" | "dropdown" | "cascading";

export interface ModelOption {
  id: string;
  name: string;
  sublabel?: string;
  badge?: string;
  providerId: string;
  providerName: string;
  isLocal: boolean;
  rawOption?: unknown;
}

export interface ProviderOption {
  id: string;
  name: string;
  isLocal: boolean;
  kind?: "local" | "cloud";
  loopback?: boolean;
}

export interface CategoryRenderPlan {
  category: CastCategory;
  mode: RenderPlanMode;
  providerCount: number;
  totalOptionsCount: number;
  fixedOption?: ModelOption;
  dropdownOptions: ModelOption[];
  selectedModelId?: string;
  providers: ProviderOption[];
  selectedProviderId?: string;
  cascadingModelOptions: ModelOption[];
  cascadingModelIsFixed?: boolean;
}

export interface AdaptiveCastPlan {
  tts: CategoryRenderPlan;
  llm: CategoryRenderPlan;
}

export interface ProviderSelectionMemory {
  [category: string]: {
    [providerId: string]: string;
  };
}

export function countCloudProviders(
  options: CastProviderOptions | null | undefined,
  category: CastCategory,
): number {
  if (!options) return 0;
  if (category === "tts") {
    const providers = new Set<string>();
    for (const v of options.ttsVoices) {
      providers.add(v.provider || v.connectionId);
    }
    return providers.size;
  }
  if (category === "llm") {
    const providers = new Set<string>();
    for (const c of options.scriptConnections) {
      providers.add(c.provider || c.connectionId);
    }
    return providers.size;
  }
  return 0;
}

export function getCloudProvidersForCategory(
  options: CastProviderOptions | null | undefined,
  category: CastCategory,
): ProviderOption[] {
  if (!options) return [];
  const map = new Map<string, ProviderOption>();
  if (category === "tts") {
    for (const v of options.ttsVoices) {
      if (!map.has(v.connectionId)) {
        map.set(v.connectionId, {
          id: v.connectionId,
          name: v.connectionName || v.provider,
          isLocal: false,
          kind: "cloud",
          loopback: v.loopback,
        });
      }
    }
  } else if (category === "llm") {
    for (const c of options.scriptConnections) {
      if (!map.has(c.connectionId)) {
        map.set(c.connectionId, {
          id: c.connectionId,
          name: c.name || c.provider,
          isLocal: false,
          kind: "cloud",
          loopback: c.loopback,
        });
      }
    }
  }
  return Array.from(map.values());
}

export function buildLocalTtsOptions(
  voices: VoiceProfileSummary[],
  installedIds: ReadonlySet<string>,
): ModelOption[] {
  return voices
    .filter((v) => installedIds.has(v.engine))
    .map((v) => ({
      id: v.id,
      name: v.name,
      sublabel: v.engine,
      badge: t("cast.engine.local"),
      providerId: "local",
      providerName: t("cast.onDeviceLocal"),
      isLocal: true,
      rawOption: v,
    }));
}

export function buildCloudTtsOptions(
  ttsVoices: CastCloudVoiceOption[],
): ModelOption[] {
  return ttsVoices.map((v) => ({
    id: `cloud:${v.voiceProfileId}`,
    name: v.name,
    sublabel: `${v.model} · ${v.voiceId}`,
    badge: t("cast.engine.cloud"),
    providerId: v.connectionId,
    providerName: v.connectionName,
    isLocal: false,
    rawOption: v,
  }));
}

export function buildLocalLlmOptions(
  models: CatalogModelEntry[],
  installedIds: ReadonlySet<string>,
): ModelOption[] {
  return models
    .filter((m) => m.task === "llm" && !m.deprecated && installedIds.has(m.id))
    .map((m) => ({
      id: m.id,
      name: m.display_name || m.id,
      sublabel: m.badge ? `badge: ${m.badge}` : undefined,
      badge: t("cast.engine.local"),
      providerId: "local",
      providerName: t("cast.onDeviceLocal"),
      isLocal: true,
      rawOption: m,
    }));
}

export function buildCloudLlmOptions(
  scriptConnections: CastConnectionOption[],
  fetchedModelsMap: Record<string, string[]> = {},
): ModelOption[] {
  const opts: ModelOption[] = [];
  for (const c of scriptConnections) {
    const models = fetchedModelsMap[c.connectionId];
    if (models && models.length > 0) {
      for (const m of models) {
        opts.push({
          id: `cloud:${c.connectionId}:${m}`,
          name: `${c.name} · ${m}`,
          sublabel: c.provider,
          badge: t("cast.engine.cloud"),
          providerId: c.connectionId,
          providerName: c.name,
          isLocal: false,
          rawOption: { connection: c, model: m },
        });
      }
    } else {
      opts.push({
        id: `cloud:${c.connectionId}:${c.defaultModel}`,
        name: `${c.name} · ${c.defaultModel}`,
        sublabel: c.provider,
        badge: t("cast.engine.cloud"),
        providerId: c.connectionId,
        providerName: c.name,
        isLocal: false,
        rawOption: { connection: c, model: c.defaultModel },
      });
    }
  }
  return opts;
}

export function computeCategoryRenderPlan(params: {
  category: CastCategory;
  mode: "narrator" | "teacher";
  localOptions: ModelOption[];
  cloudOptions: ModelOption[];
  cloudProviders: ProviderOption[];
  selectedProviderId?: string | null;
  selectedModelId?: string | null;
  smartDefaultLocalId?: string | null;
  smartDefaultCloudIds?: Record<string, string>;
  memory?: Record<string, string>;
}): CategoryRenderPlan {
  const {
    category,
    mode,
    localOptions,
    cloudOptions,
    cloudProviders,
  } = params;

  if (category === "llm" && mode === "narrator") {
    return {
      category,
      mode: "hidden",
      providerCount: cloudProviders.length,
      totalOptionsCount: 0,
      dropdownOptions: [],
      providers: [],
      cascadingModelOptions: [],
    };
  }

  const cloudCount = cloudProviders.length;
  const totalOptionsCount = localOptions.length + (cloudCount > 0 ? cloudOptions.length : 0);

  if (cloudCount === 0) {
    if (localOptions.length === 0) {
      return {
        category,
        mode: "hidden",
        providerCount: 0,
        totalOptionsCount: 0,
        dropdownOptions: [],
        providers: [],
        cascadingModelOptions: [],
      };
    }
    if (localOptions.length === 1) {
      return {
        category,
        mode: "fixed-auto",
        providerCount: 0,
        totalOptionsCount: 1,
        fixedOption: localOptions[0],
        selectedModelId: localOptions[0].id,
        dropdownOptions: localOptions,
        providers: [],
        cascadingModelOptions: [],
      };
    }
    const selectedModelId =
      params.selectedModelId && localOptions.some((o) => o.id === params.selectedModelId)
        ? params.selectedModelId
        : params.smartDefaultLocalId && localOptions.some((o) => o.id === params.smartDefaultLocalId)
          ? params.smartDefaultLocalId
          : localOptions[0].id;
    return {
      category,
      mode: "dropdown",
      providerCount: 0,
      totalOptionsCount: localOptions.length,
      dropdownOptions: localOptions,
      selectedModelId,
      providers: [],
      cascadingModelOptions: [],
    };
  }

  if (cloudCount === 1) {
    const singleCloudProvider = cloudProviders[0];
    const availableOptions = [
      ...localOptions,
      ...cloudOptions.filter((o) => o.providerId === singleCloudProvider.id),
    ];
    if (availableOptions.length === 0) {
      return {
        category,
        mode: "hidden",
        providerCount: 1,
        totalOptionsCount: 0,
        dropdownOptions: [],
        providers: [],
        cascadingModelOptions: [],
      };
    }
    if (availableOptions.length === 1) {
      return {
        category,
        mode: "fixed-auto",
        providerCount: 1,
        totalOptionsCount: 1,
        fixedOption: availableOptions[0],
        selectedModelId: availableOptions[0].id,
        dropdownOptions: availableOptions,
        providers: [],
        cascadingModelOptions: [],
      };
    }
    let selectedModelId: string | undefined;
    if (params.selectedModelId && availableOptions.some((o) => o.id === params.selectedModelId)) {
      selectedModelId = params.selectedModelId;
    } else if (params.smartDefaultLocalId && localOptions.some((o) => o.id === params.smartDefaultLocalId)) {
      selectedModelId = params.smartDefaultLocalId;
    } else {
      selectedModelId = availableOptions[0].id;
    }
    return {
      category,
      mode: "dropdown",
      providerCount: 1,
      totalOptionsCount: availableOptions.length,
      dropdownOptions: availableOptions,
      selectedModelId,
      providers: [],
      cascadingModelOptions: [],
    };
  }

  const providers: ProviderOption[] = [
    ...(localOptions.length > 0
      ? [
          {
            id: "local",
            name: t("cast.onDeviceLocal"),
            isLocal: true,
            kind: "local" as const,
          },
        ]
      : []),
    ...cloudProviders,
  ];

  const selectedProviderId =
    params.selectedProviderId && providers.some((p) => p.id === params.selectedProviderId)
      ? params.selectedProviderId
      : (providers[0]?.id ?? "local");

  const cascadingModelOptions =
    selectedProviderId === "local"
      ? localOptions
      : cloudOptions.filter((o) => o.providerId === selectedProviderId);

  let selectedModelIdInCascade: string | undefined;
  const remembered = params.memory?.[selectedProviderId];
  if (remembered && cascadingModelOptions.some((o) => o.id === remembered)) {
    selectedModelIdInCascade = remembered;
  } else if (params.selectedModelId && cascadingModelOptions.some((o) => o.id === params.selectedModelId)) {
    selectedModelIdInCascade = params.selectedModelId;
  } else if (selectedProviderId === "local" && params.smartDefaultLocalId && cascadingModelOptions.some((o) => o.id === params.smartDefaultLocalId)) {
    selectedModelIdInCascade = params.smartDefaultLocalId;
  } else if (selectedProviderId !== "local" && params.smartDefaultCloudIds?.[selectedProviderId] && cascadingModelOptions.some((o) => o.id === params.smartDefaultCloudIds![selectedProviderId])) {
    selectedModelIdInCascade = params.smartDefaultCloudIds[selectedProviderId];
  } else {
    selectedModelIdInCascade = cascadingModelOptions[0]?.id;
  }

  const cascadingModelIsFixed = cascadingModelOptions.length === 1;

  return {
    category,
    mode: "cascading",
    providerCount: cloudCount,
    totalOptionsCount,
    dropdownOptions: [],
    providers,
    selectedProviderId,
    cascadingModelOptions,
    cascadingModelIsFixed,
    fixedOption: cascadingModelIsFixed ? cascadingModelOptions[0] : undefined,
    selectedModelId: selectedModelIdInCascade,
  };
}

export function switchProviderInPlan(params: {
  plan: CategoryRenderPlan;
  newProviderId: string;
  memory: Record<string, string>;
  smartDefaultLocalId?: string | null;
  smartDefaultCloudIds?: Record<string, string>;
  localOptions: ModelOption[];
  cloudOptions: ModelOption[];
}): {
  updatedProviderId: string;
  updatedModelId: string | undefined;
  updatedMemory: Record<string, string>;
} {
  const { newProviderId, memory, localOptions, cloudOptions } = params;
  const updatedMemory = { ...memory };
  if (params.plan.selectedProviderId && params.plan.selectedModelId) {
    updatedMemory[params.plan.selectedProviderId] = params.plan.selectedModelId;
  }

  const targetModels =
    newProviderId === "local"
      ? localOptions
      : cloudOptions.filter((o) => o.providerId === newProviderId);

  const remembered = updatedMemory[newProviderId];
  let updatedModelId: string | undefined;
  if (remembered && targetModels.some((o) => o.id === remembered)) {
    updatedModelId = remembered;
  } else if (newProviderId === "local" && params.smartDefaultLocalId && targetModels.some((o) => o.id === params.smartDefaultLocalId)) {
    updatedModelId = params.smartDefaultLocalId;
  } else if (newProviderId !== "local" && params.smartDefaultCloudIds?.[newProviderId] && targetModels.some((o) => o.id === params.smartDefaultCloudIds![newProviderId])) {
    updatedModelId = params.smartDefaultCloudIds[newProviderId];
  } else {
    updatedModelId = targetModels[0]?.id;
  }

  if (updatedModelId) {
    updatedMemory[newProviderId] = updatedModelId;
  }

  return {
    updatedProviderId: newProviderId,
    updatedModelId,
    updatedMemory,
  };
}
