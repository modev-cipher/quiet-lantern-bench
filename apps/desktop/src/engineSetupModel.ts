import type { MessageKey } from "./i18n";
import type { EngineStatus } from "./engineStatus";

export type EngineNeed = "documents" | "localAi";

export const LITE_VARIANT = "lite";

export interface VariantOffer {
  variant: string;
  downloadBytes: number;
  installedBytes: number;
  spaceNeededBytes: number;
}

export interface EngineChoice {
  variant: string;
  reason:
    | "nvidia_ready"
    | "no_nvidia_gpu"
    | "driver_too_old"
    | "gpu_too_old"
    | "not_offered";
  gpuName: string | null;
}

export interface EngineInstallInfo {
  installed: {
    engineVersion: string;
    engineApi: number;
    variant: string;
    platform: string;
  } | null;
  requiredApi: number;
  platform: string;
  offered: VariantOffer[];
  engineVersion: string | null;
  recommended: EngineChoice;
  spaceAvailableBytes: number | null;
  offerError: string | null;
  installing: boolean;
  liteAvailable: boolean;
}

export type InstallEvent =
  | {
      phase: "downloading";
      bytesDone: number;
      bytesTotal: number;
      speedBps: number;
    }
  | { phase: "verifying" }
  | { phase: "extracting"; filesDone: number }
  | { phase: "starting" }
  | { phase: "installed"; engine: { variant: string } }
  | { phase: "failed"; code: string; message: string };

export type InstallProgress = Exclude<
  InstallEvent,
  { phase: "installed" | "failed" }
>;

export function engineSatisfies(
  status: EngineStatus | null,
  activeVariant: string | null,
  need: EngineNeed,
): boolean {
  if (status?.state === "notInstalled" || status?.state === "needsUpdate")
    return false;
  return !(need === "localAi" && activeVariant === LITE_VARIANT);
}

/** Native gates answer `ERR_ENGINE_…:<need>`; anything else is not an engine refusal. */
export function engineRefusal(raw: string): EngineNeed | null {
  const match =
    /ERR_ENGINE_(?:NOT_INSTALLED|LOCAL_AI_REQUIRED):(documents|localAi)/.exec(
      raw,
    );
  return match == null ? null : (match[1] as EngineNeed);
}

export function installOptions(
  info: EngineInstallInfo,
  need: EngineNeed,
  fullVariant: string,
): VariantOffer[] {
  const full = info.offered.find((offer) => offer.variant === fullVariant);
  const lite = info.offered.find((offer) => offer.variant === LITE_VARIANT);
  const options = need === "documents" ? [lite, full] : [full];
  return options.filter((offer): offer is VariantOffer => offer != null);
}

export function fullVariantBody(
  choice: EngineChoice,
  variant: string,
): MessageKey {
  if (variant !== choice.variant) {
    return variant === "cpu" ? "engine.setup.fullCpu" : "engine.setup.fullGpu";
  }
  switch (choice.reason) {
    case "nvidia_ready":
      return "engine.setup.fullGpu";
    case "driver_too_old":
      return "engine.setup.fullCpuOldDriver";
    case "gpu_too_old":
      return "engine.setup.fullCpuOldGpu";
    default:
      return "engine.setup.fullCpu";
  }
}

export function installErrorKey(raw: string): MessageKey {
  const code = raw.split(":")[0]?.trim() ?? "";
  switch (code) {
    case "ERR_ENGINE_OFFLINE":
      return "engine.setup.error.offline";
    case "ERR_ENGINE_NOT_CONFIGURED":
      return "engine.setup.error.notConfigured";
    case "ERR_ENGINE_INCOMPATIBLE":
      return "engine.setup.error.incompatible";
    case "ERR_ENGINE_NO_SPACE":
      return "engine.setup.error.noSpace";
    case "ERR_ENGINE_VERIFY":
    case "ERR_ENGINE_UNPACK":
      return "engine.setup.error.verify";
    case "ERR_JOB_ACTIVE":
      return "engine.setup.error.jobs";
    case "ERR_ENGINE_START":
      return "engine.setup.error.startFailed";
    case "ERR_ENGINE_NOT_OFFERED":
      return "engine.setup.error.notOffered";
    default:
      return "engine.setup.error.generic";
  }
}

export function progressFraction(progress: InstallProgress): number | null {
  if (progress.phase !== "downloading" || progress.bytesTotal <= 0) return null;
  return Math.min(1, progress.bytesDone / progress.bytesTotal);
}
