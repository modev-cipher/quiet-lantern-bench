import { t } from "./i18n";

export type EngineStatus =
  | { state: "notInstalled" }
  | { state: "needsUpdate"; installedApi: number | null; requiredApi: number }
  | { state: "downloading"; bytesDone: number; bytesTotal: number; speedBps: number }
  | { state: "extracting"; filesDone: number; filesTotal: number }
  | { state: "starting" }
  | { state: "ready"; port: number }
  | { state: "restarting"; attempt: number }
  | { state: "failed"; errorCode: string; message: string }
  | { state: "down" };

export interface BadgePresentation {
  label: string;
  dotClass: string;
}

export function presentStatus(status: EngineStatus | null): BadgePresentation {
  switch (status?.state) {
    case "ready":
      return { label: t("engine.status.ready"), dotClass: "bg-emerald-500" };
    case "starting":
      return { label: t("engine.status.starting"), dotClass: "bg-amber-500" };
    case "restarting":
      return {
        label: t("engine.status.restarting", { attempt: status.attempt }),
        dotClass: "bg-amber-500",
      };
    case "downloading": {
      const pct =
        status.bytesTotal > 0
          ? Math.round((status.bytesDone / status.bytesTotal) * 100)
          : 0;
      return { label: t("engine.status.downloading", { percent: pct }), dotClass: "bg-amber-500" };
    }
    case "extracting": {
      const pct =
        status.filesTotal > 0
          ? Math.round((status.filesDone / status.filesTotal) * 100)
          : 0;
      return { label: t("engine.status.extracting", { percent: pct }), dotClass: "bg-amber-500" };
    }
    case "needsUpdate":
      return { label: t("engine.status.needsUpdate"), dotClass: "bg-amber-500" };
    case "notInstalled":
      return { label: t("engine.status.notInstalled"), dotClass: "bg-neutral-500" };
    case "failed":
      return { label: t("engine.status.failed", { message: status.message }), dotClass: "bg-red-500" };
    case "down":
      return { label: t("engine.status.down"), dotClass: "bg-red-500" };
    default:
      return { label: t("engine.status.unknown"), dotClass: "bg-neutral-500" };
  }
}
