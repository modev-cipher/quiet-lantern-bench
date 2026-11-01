// Book Agent composer state (charter §7, B-4): the pure model behind the
// Study companion's Book tab. Every visible state is backed by runtime truth
// from `agent_session_probe` / session events — no state claims a search,
// citation, or session that did not happen.

import { t } from "./i18n";

export type BookAgentState =
  | { kind: "loading" }
  | { kind: "unsupported" }
  | {
      kind: "consentRequired";
      connectionId: string;
      hosts: string[];
      dataCategories: string[];
      retentionUrl: string | null;
    }
  | { kind: "ready" }
  | { kind: "starting" }
  | { kind: "live" }
  | { kind: "liveAgent" }
  | { kind: "liveTutor" }
  | { kind: "notInBuild" }
  | { kind: "failed"; message: string };

/** Parse one `agent_session_probe` answer; malformed answers fail closed. */
export function bookAgentStateFrom(payload: unknown): BookAgentState {
  if (typeof payload !== "object" || payload == null) {
    return { kind: "failed", message: "probe unavailable" };
  }
  const record = payload as Record<string, unknown>;
  switch (record.state) {
    case "live":
      return record.mode === "agent"
        ? { kind: "liveAgent" }
        : { kind: "liveTutor" };
    case "ready":
      return { kind: "ready" };
    case "unsupported":
      return { kind: "unsupported" };
    case "consent_required": {
      const strings = (value: unknown): string[] =>
        Array.isArray(value)
          ? value.filter((v): v is string => typeof v === "string")
          : [];
      return {
        kind: "consentRequired",
        connectionId:
          typeof record.connection_id === "string" ? record.connection_id : "",
        hosts: strings(record.hosts),
        dataCategories: strings(record.data_categories),
        retentionUrl:
          typeof record.retention_url === "string"
            ? record.retention_url
            : null,
      };
    }
    default:
      return { kind: "failed", message: "probe unavailable" };
  }
}

/** The user-visible names for the canonical consent categories (§6.4/A5 —
 * `source_text` reads "book text and book metadata" so the displayed words
 * match what egresses). */
export function categoryLabel(id: string): string {
  switch (id) {
    case "source_text":
      return t("tutor.category.sourceText");
    case "voice_audio":
      return t("tutor.category.voiceAudio");
    case "voice_transcript":
      return t("tutor.category.voiceTranscript");
    case "reading_position":
      return t("tutor.category.readingPosition");
    case "page_images":
      return t("tutor.category.pageImages");
    default:
      return id;
  }
}

/** Switch confirmation copy (§2.2/§7.2): shown when the Tutor is live. */
