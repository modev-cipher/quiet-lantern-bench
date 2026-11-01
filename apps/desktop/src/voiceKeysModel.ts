import { providerDisplayName } from "./i18n/providerNames";

export type KeyState = "active" | "invalid" | "out_of_quota" | "unchecked";

export type StorageLevel = "os_keychain" | "encrypted_file" | "session_only";

export interface StorageStatus {
  level: StorageLevel;
  locked: boolean;
}

export interface VoiceKeyEntry {
  baseUrl: string;
  /** Slot number under the base URL; 0 is the pre-slot single key. */
  slot: number;
  /** Slot-qualified store account — the row's opaque identity and the
   * argument to per-slot commands. Never reconstruct it client-side. */
  account: string;
  provider: string;
  label: string | null;
  last4: string | null;
  state: KeyState;
  keyPresent: boolean;
  custom: boolean;
  addedAt: number;
  lastUsed: number | null;
}

export interface ImportRejectedLine {
  line: number;
  reason: string;
}

export interface ImportDuplicateLine {
  line: number;
  label: string;
}

/** The bulk-import receipt. By construction (backend fixed-reason strings,
 * labels are env-var names, views carry last4 at most) nothing in it is a
 * key — which is why the panel may render it verbatim. */
export interface ImportReport {
  imported: VoiceKeyEntry[];
  rejected: ImportRejectedLine[];
  duplicates: ImportDuplicateLine[];
}

export function importSummaryOf(report: ImportReport): string {
  const keys =
    report.imported.length === 1 ? "1 key" : `${report.imported.length} keys`;
  const parts = [`Imported ${keys}`];
  if (report.duplicates.length > 0) {
    parts.push(`skipped ${report.duplicates.length} already stored`);
  }
  if (report.rejected.length > 0) {
    const lines =
      report.rejected.length === 1
        ? "1 line"
        : `${report.rejected.length} lines`;
    parts.push(`could not read ${lines}`);
  }
  return `${parts.join("; ")}.`;
}

export function importDetailsOf(report: ImportReport): string[] {
  const details: string[] = [];
  if (report.imported.length > 0) {
    const names = report.imported
      .map((entry) => entry.label ?? `slot ${entry.slot}`)
      .join(", ");
    details.push(`Added: ${names}`);
  }
  for (const dup of report.duplicates) {
    details.push(`Line ${dup.line} (${dup.label}): already stored, skipped`);
  }
  for (const rejected of report.rejected) {
    details.push(`Line ${rejected.line}: ${rejected.reason}`);
  }
  return details;
}

export interface VoiceProvider {
  id: string;
  defaultBaseUrl: string | null;
  capabilities: string[];
}

export interface UrlCheck {
  canonicalUrl: string;
  consented: boolean;
}

export function providerLabelOf(id: string): string {
  return providerDisplayName(id);
}

export interface StateBadge {
  label: string;
  tone: "ok" | "error" | "warn" | "muted";
}

export function stateBadgeOf(state: KeyState): StateBadge {
  switch (state) {
    case "active":
      return { label: "Working", tone: "ok" };
    case "invalid":
      return { label: "Not working", tone: "error" };
    case "out_of_quota":
      return { label: "Rate limited", tone: "warn" };
    case "unchecked":
      return { label: "Not yet used", tone: "muted" };
  }
}

export function maskedKeyOf(entry: VoiceKeyEntry): string {
  if (!entry.keyPresent || entry.last4 === null) return "key not loaded";
  return `…${entry.last4}`;
}

export function storageNoticeOf(status: StorageStatus): string | null {
  if (status.level === "os_keychain") return null;
  if (status.level === "encrypted_file") {
    return status.locked
      ? "No OS keychain was found. Keys live in a passphrase-encrypted file that is currently locked."
      : "No OS keychain was found. Keys live in a passphrase-encrypted file on this machine.";
  }
  return "Session-only storage: keys are held in memory and forgotten when the app closes. Re-enter them next launch.";
}

export function validateCustomUrl(
  raw: string,
): { url: string } | { error: string } {
  const trimmed = raw.trim();
  if (trimmed === "") return { error: "Enter the endpoint's base URL." };
  let parsed: URL;
  try {
    parsed = new URL(trimmed);
  } catch {
    return { error: "That is not a valid URL." };
  }
  if (parsed.username !== "" || parsed.password !== "") {
    return {
      error:
        "Remove the credentials from the URL; the key is stored separately.",
    };
  }
  if (parsed.search !== "" || parsed.hash !== "") {
    return { error: "A base URL cannot carry a query string or fragment." };
  }
  if (parsed.protocol !== "https:") {
    if (parsed.protocol === "http:" && isLoopbackHost(parsed.hostname)) {
      return { url: trimmed };
    }
    return {
      error:
        "Custom endpoints must use https:// (plain http is allowed only for localhost).",
    };
  }
  return { url: trimmed };
}

function isLoopbackHost(hostname: string): boolean {
  const bare = hostname.replace(/^\[/, "").replace(/\]$/, "");
  if (bare.toLowerCase() === "localhost" || bare === "::1") return true;
  const v4 = bare.match(/^(\d+)\.(\d+)\.(\d+)\.(\d+)$/);
  return v4 !== null && v4[1] === "127";
}

export function hostnameOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

export function needsConsent(
  provider: VoiceProvider,
  check: UrlCheck | null,
): boolean {
  if (provider.defaultBaseUrl !== null) return false;
  return check === null || !check.consented;
}

export function consentCopyFor(hostname: string): string {
  return (
    `Your key for ${hostname} will be sent to that host to validate it, and voice ` +
    `sessions you start against it can send your microphone audio and book excerpts ` +
    `there. txtCaster contacts no other host for this endpoint. This is asked once per URL.`
  );
}

export function sortedEntries(entries: VoiceKeyEntry[]): VoiceKeyEntry[] {
  return [...entries].sort((a, b) => {
    if (a.provider !== b.provider) return a.provider.localeCompare(b.provider);
    if (a.baseUrl !== b.baseUrl) return a.baseUrl.localeCompare(b.baseUrl);
    return a.slot - b.slot;
  });
}
