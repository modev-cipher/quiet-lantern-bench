import { t } from "./i18n";
export interface StorageDirView {
  path: string;
  defaultPath: string;
  isDefault: boolean;
  configured: string | null;
  rejected: string | null;
}

export type CapMode = "default" | "unlimited" | "custom";

export { formatBytes } from "./modelCatalogModel";

export interface EngineStorageInfo {
  status: string;
  diskBytes: number;
  path: string;
}

export interface ModelStorageInfo {
  modelId: string;
  name: string;
  task: string;
  diskBytes: number;
  installedAt: number | null;
}

export interface CacheStorageInfo {
  diskBytes: number;
  activeJobLocks: number;
}

export interface BooksStorageInfo {
  count: number;
  diskBytes: number;
}

export interface ExportsStorageInfo {
  count: number;
  diskBytes: number;
}

export interface StorageBreakdown {
  totalBytes: number;
  engine: EngineStorageInfo;
  models: ModelStorageInfo[];
  cache: CacheStorageInfo;
  books: BooksStorageInfo;
  exports: ExportsStorageInfo;
}

export interface StorageSettings {
  models: StorageDirView;
  exports: StorageDirView;
  cacheDir: string;
  cacheBytes: number;
  cacheCapBytes: number | null;
  cacheCapSource: CapMode;
  cacheCapRefused: string | null;
  defaultCacheCapBytes: number;
}

export const BYTES_PER_GB = 1024 * 1024 * 1024;

export function capRequest(mode: CapMode, gb: number): number | null {
  if (mode === "default") return null;
  if (mode === "unlimited") return 0;
  return Math.round(gb * BYTES_PER_GB);
}

export function parseCapGb(raw: string): { gb: number } | { error: string } {
  const trimmed = raw.trim();
  if (trimmed === "") return { error: t("settings.capErrorEmpty") };
  if (!/^\d+(\.\d+)?$/.test(trimmed)) {
    return { error: t("settings.capErrorShape") };
  }
  const gb = Number(trimmed);
  if (!Number.isFinite(gb)) return { error: t("settings.capErrorNumber") };
  if (gb <= 0) {
    return { error: t("settings.capErrorZero") };
  }
  if (Math.round(gb * BYTES_PER_GB) > Number.MAX_SAFE_INTEGER) {
    return { error: t("settings.capErrorHuge") };
  }
  return { gb };
}

export function formatCapBytes(bytes: number | null): string {
  if (bytes === null) return t("settings.capUnlimited");
  return t("settings.gbValue", { gb: formatGbValue(bytes / BYTES_PER_GB) });
}

/** Catalog key + params for the usage line; components render it with `tx` (visible islands as <bdi>). */
export function cacheUsageMessage(settings: StorageSettings): {
  key: "settings.cacheUsage" | "settings.cacheUsageUnlimited";
  params: Record<string, string>;
} {
  const used = formatGbValue(settings.cacheBytes / BYTES_PER_GB);
  return settings.cacheCapBytes === null
    ? { key: "settings.cacheUsageUnlimited", params: { used } }
    : {
        key: "settings.cacheUsage",
        params: {
          used,
          cap: formatGbValue(settings.cacheCapBytes / BYTES_PER_GB),
        },
      };
}

export function cacheUsageOf(settings: StorageSettings): string {
  const { key, params } = cacheUsageMessage(settings);
  return t(key, params);
}

export function capGbInputFor(settings: StorageSettings): string {
  const bytes = settings.cacheCapBytes ?? settings.defaultCacheCapBytes;
  return formatGbValue(bytes / BYTES_PER_GB);
}

function formatGbValue(gb: number): string {
  if (!Number.isFinite(gb)) return "0";
  const rounded = Math.round(gb * 10) / 10;
  return Number.isInteger(rounded) ? `${rounded}` : rounded.toFixed(1);
}

export function capNoticeOf(settings: StorageSettings): string | null {
  if (settings.cacheCapRefused === null) return null;
  return t("settings.capRefusedNotice", { value: settings.cacheCapRefused });
}

export function dirStatusOf(dir: StorageDirView): string {
  if (dir.rejected) {
    return t("settings.dirRefused", {
      configured: dir.configured ?? "",
      reason: dir.rejected,
    });
  }
  return dir.isDefault
    ? t("settings.defaultLocation")
    : t("settings.customLocation");
}

export const PATH_TAIL_SEGMENTS = 2;

export function pathTail(value: string, segments = PATH_TAIL_SEGMENTS): string {
  if (value === "" || segments < 1) return value;
  const separator = value.includes("\\") && !value.includes("/") ? "\\" : "/";
  const parts = value.split(separator).filter((part) => part !== "");
  if (parts.length <= segments) return value;
  return `…${separator}${parts.slice(-segments).join(separator)}`;
}
