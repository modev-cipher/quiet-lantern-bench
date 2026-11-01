import { t } from "./i18n";

export type DownloadErrorView = {
  kind: "connection" | "disk" | "corrupt" | "unavailable" | "other";
  message: string;
  detail: string;
  resumable: boolean;
};

export function downloadErrorView(raw: string): DownloadErrorView | null {
  const text = raw.trim();
  const lower = text.toLowerCase();
  if (lower === "download cancelled") return null;
  const status = /server returned status (\d{3})/.exec(lower);
  const code = status ? Number(status[1]) : null;
  if (lower.startsWith("insufficient disk space")) {
    return {
      kind: "disk",
      message: t("models.error.disk"),
      detail: text,
      resumable: true,
    };
  }
  if (lower.startsWith("hash mismatch")) {
    return {
      kind: "corrupt",
      message: t("models.error.corrupt"),
      detail: text,
      resumable: false,
    };
  }
  if (code === 404 || code === 403 || code === 410) {
    return {
      kind: "unavailable",
      message: t("models.error.unavailable"),
      detail: text,
      resumable: false,
    };
  }
  if (
    lower.startsWith("network error") ||
    lower.startsWith("size mismatch") ||
    (code != null && (code >= 500 || code === 429 || code === 408))
  ) {
    return {
      kind: "connection",
      message: t("models.error.connection"),
      detail: text,
      resumable: true,
    };
  }
  return {
    kind: "other",
    message: t("models.error.generic"),
    detail: text,
    resumable: true,
  };
}
