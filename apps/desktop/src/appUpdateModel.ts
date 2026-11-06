// UPD-1: pure view model over the updater snapshot (docs/notes/upd-1-auto-updater.md §1).
// The backend owns every transition; this file only decides what the banner and
// the Settings row show for a given snapshot.

export type UpdatePhase =
  | "disabled"
  | "idle"
  | "checking"
  | "up_to_date"
  | "available"
  | "downloading"
  | "verifying"
  | "installing"
  | "paused"
  | "failed";

export interface UpdateSnapshot {
  phase: UpdatePhase;
  enabled: boolean;
  current_version: string;
  available_version: string | null;
  notes: string | null;
  release_url: string | null;
  install_kind: "restart" | "open" | null;
  received: number;
  total: number;
  error_code: string | null;
  error_message: string | null;
  last_checked_unix: number | null;
  last_attempt_offline: boolean;
  dismissed: boolean;
}

export type UpdateBanner =
  | {
      kind: "available";
      version: string;
      notes: string | null;
      action: "restart" | "open";
      whatsNew: boolean;
      castsContinue: boolean;
    }
  | {
      kind: "progress";
      phase: "downloading" | "verifying" | "installing";
      percent: number | null;
    }
  | { kind: "failed"; currentVersion: string };

export function percentOf(received: number, total: number): number | null {
  if (!Number.isFinite(total) || total <= 0) return null;
  return Math.max(0, Math.min(100, Math.floor((received / total) * 100)));
}

/** Non-modal banner content, or null when nothing should be shown (idle,
 *  checking, up to date, paused, disabled, or "Later" was clicked). Casts
 *  interrupted by the restart resume on launch from their checkpoints. */
export function bannerFor(
  s: UpdateSnapshot | null,
  castsActive = false,
): UpdateBanner | null {
  if (!s || !s.enabled) return null;
  switch (s.phase) {
    case "available": {
      if (s.dismissed || !s.available_version) return null;
      const action = s.install_kind === "open" ? "open" : "restart";
      return {
        kind: "available",
        version: s.available_version,
        notes: s.notes,
        action,
        whatsNew: action === "restart" && s.release_url !== null,
        castsContinue: action === "restart" && castsActive,
      };
    }
    case "downloading":
      return {
        kind: "progress",
        phase: "downloading",
        percent: percentOf(s.received, s.total),
      };
    case "verifying":
    case "installing":
      return { kind: "progress", phase: s.phase, percent: null };
    case "failed":
      return { kind: "failed", currentVersion: s.current_version };
    default:
      return null;
  }
}

export type UpdateStatusLine =
  | { kind: "unavailable" }
  | { kind: "checking" }
  | { kind: "never" }
  | { kind: "offline" }
  | {
      kind: "checked";
      at: number;
      detail: "up_to_date" | "available" | "offline" | null;
      version: string | null;
    };

/** Settings → Updates row. A paused publisher shows only the time (no claim). */
export function statusLineFor(s: UpdateSnapshot | null): UpdateStatusLine {
  if (!s || !s.enabled) return { kind: "unavailable" };
  if (s.phase === "checking") return { kind: "checking" };
  if (s.last_checked_unix === null) {
    return s.last_attempt_offline ? { kind: "offline" } : { kind: "never" };
  }
  let detail: "up_to_date" | "available" | "offline" | null = null;
  if (s.last_attempt_offline) detail = "offline";
  else if (s.available_version) detail = "available";
  else if (s.phase === "up_to_date") detail = "up_to_date";
  return {
    kind: "checked",
    at: s.last_checked_unix,
    detail,
    version: s.available_version,
  };
}

/** Time only when it was today, date + time otherwise; locale-native digits. */
export function formatCheckedAt(
  unixSeconds: number,
  locale: string,
  now: number = Date.now(),
): string {
  const at = new Date(unixSeconds * 1000);
  const sameDay = new Date(now).toDateString() === at.toDateString();
  const options: Intl.DateTimeFormatOptions = sameDay
    ? { hour: "2-digit", minute: "2-digit" }
    : { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" };
  return new Intl.DateTimeFormat(locale, options).format(at);
}
