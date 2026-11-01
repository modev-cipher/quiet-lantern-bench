// Module-level home for the voice-session UI state (playbackStore pattern).
// The Rust runtime owns the session; this store makes the UI's view of it
// outlive any one mount of VoiceSessionPanel, so navigating away and back
// (or the panel remounting for any reason) no longer forgets a live session
// and strands the user on a Start button the runtime refuses.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  INITIAL_VOICE_SESSION,
  reduceVoiceSession,
  voiceSessionEnvelopeFrom,
  voiceprintFrom,
  type TranscriptEntry,
  type VoiceSessionState,
} from "./voiceSessionModel";

/**
 * The conversation a Continue picked up (field session 2026-10-09: Continue
 * opened an empty chat, so it looked like a fresh session). Shown above
 * the new session's turns, read-only; it lives beside the session state
 * because a new session id replaces that state wholesale.
 */
export interface EarlierConversation {
  bookId: string | null;
  sourceId: string;
  turns: TranscriptEntry[];
}

export interface VoiceUiState {
  session: VoiceSessionState;
  starting: boolean;
  startError: string | null;
  earlier: EarlierConversation | null;
  identity: ActiveVoiceSession | null;
  pendingTarget: { bookId: string | null; jobId: string | null } | null;
}

const INITIAL_UI: VoiceUiState = {
  session: INITIAL_VOICE_SESSION,
  starting: false,
  startError: null,
  earlier: null,
  identity: null,
  pendingTarget: null,
};

let state = INITIAL_UI;
const listeners = new Set<() => void>();
const drafts = new Map<string, string>();

export function getVoiceDraft(scope: string): string {
  return drafts.get(scope) ?? "";
}
export function setVoiceDraft(scope: string, value: string): void {
  drafts.delete(scope);
  if (value !== "") drafts.set(scope, value);
  if (drafts.size > 20) drafts.delete(drafts.keys().next().value!);
  emit();
}

let bookScope: { bookId: string | null } | null = null;
let stateBook: { bookId: string | null } | null = null;
let runtimeRevision = 0;
let probeRevision = 0;

export interface ActiveVoiceSession {
  sessionId: string;
  bookId: string | null;
  jobId: string | null;
  purpose: "tutor" | "agent";
}

export function activeSessionFrom(value: unknown): ActiveVoiceSession | null {
  if (value === null) return null;
  if (value == null || typeof value !== "object")
    throw new Error("Invalid session identity");
  const row = value as Record<string, unknown>;
  if (
    typeof row.sessionId !== "string" ||
    row.sessionId === "" ||
    !(row.bookId === null || typeof row.bookId === "string") ||
    !(row.jobId === null || typeof row.jobId === "string") ||
    !(row.purpose === "tutor" || row.purpose === "agent")
  )
    throw new Error("Invalid session identity");
  return {
    sessionId: row.sessionId,
    bookId: row.bookId,
    jobId: row.jobId,
    purpose: row.purpose,
  };
}

export function sessionIsActive(snapshot: VoiceUiState): boolean {
  return (
    snapshot.starting ||
    snapshot.session.status === "starting" ||
    snapshot.session.status === "live" ||
    snapshot.session.status === "renewing"
  );
}

export function matchesVoiceScope(
  bookId: string | null,
  jobId?: string | null,
): boolean {
  return (
    stateBook != null &&
    stateBook.bookId === bookId &&
    (jobId === undefined ||
      state.identity == null ||
      state.identity.jobId === jobId)
  );
}

function emit(): void {
  for (const listener of listeners) listener();
}

export function getVoiceSnapshot(): VoiceUiState {
  return bookScope == null || matchesVoiceScope(bookScope.bookId)
    ? state
    : INITIAL_UI;
}

export function getActiveVoiceSnapshot(): VoiceUiState {
  return state;
}

/** The bound book scope, for render-time gating (null = never bound). */
export function getVoiceBookScope(): { bookId: string | null } | null {
  return bookScope;
}

export function subscribeVoice(listener: () => void): () => void {
  listeners.add(listener);
  void ensureRuntimeListener().catch(() => {});
  return () => {
    listeners.delete(listener);
  };
}

export function bindVoicePanelBook(bookId: string | null): void {
  if (bookScope != null && bookScope.bookId === bookId) return;
  bookScope = { bookId };
  if (stateBook == null && state.session.sessionId == null)
    stateBook = bookScope;
  emit();
}

/**
 * Talk entry (owner decision 2026-10-09): a Talk press opens a FRESH tutor
 * for the book, so the previous, finished conversation's rows — which a
 * same-book bind would otherwise keep — are dropped and the store is bound
 * to this book. A session that is starting or still running is real and is
 * left untouched; the panel adopts it as before.
 */
export function freshVoicePanelForBook(bookId: string | null): void {
  const status = state.session.status;
  if (
    state.starting ||
    status === "starting" ||
    status === "live" ||
    status === "renewing"
  ) {
    return;
  }
  state = INITIAL_UI;
  pendingTypedSends = [];
  bookScope = { bookId };
  stateBook = { bookId };
  emit();
}

export function markStarting(origin?: {
  bookId: string | null;
  jobId: string | null;
}): void {
  if (sessionIsActive(state)) return;
  runtimeRevision += 1;
  // D3 belt: a fresh start clears residual rows optimistically. The id
  // guard in dispatchVoiceEvent is the invariant (suspenders) — a missed
  // clearing edge can never leak, because stale-id events are unrenderable.
  state = {
    ...state,
    starting: true,
    earlier:
      state.earlier?.bookId ===
      (origin == null ? bookScope?.bookId : origin.bookId)
        ? state.earlier
        : null,
    startError: null,
    identity: null,
    pendingTarget: origin ?? { bookId: bookScope?.bookId ?? null, jobId: null },
    session: { ...state.session, transcript: [] },
  };
  stateBook = origin == null ? bookScope : { bookId: origin.bookId };
  emit();
}

export function markVoiceError(
  message: string,
  origin?: { bookId: string | null; sessionId?: string | null },
): void {
  if (
    origin != null &&
    (!matchesVoiceScope(origin.bookId) ||
      (origin.sessionId != null &&
        origin.sessionId !== state.session.sessionId))
  )
    return;
  state = {
    ...state,
    starting: false,
    startError: message,
    pendingTarget: null,
  };
  if (stateBook == null) stateBook = bookScope;
  emit();
}

/** Attach a Continue's history only to its own active or pending book. */
export function showEarlierConversation(
  bookId: string | null,
  sourceId: string,
  turns: readonly { role: string; text: string; voiceprint?: unknown }[],
): void {
  if (!matchesVoiceScope(bookId)) return;
  const entries: TranscriptEntry[] = turns
    .filter(
      (
        turn,
      ): turn is {
        role: "user" | "assistant";
        text: string;
        voiceprint?: unknown;
      } =>
        (turn.role === "user" || turn.role === "assistant") &&
        turn.text.trim() !== "",
    )
    .map((turn) => {
      const voiceprint =
        turn.role === "user" ? voiceprintFrom(turn.voiceprint) : null;
      return {
        role: turn.role,
        text: turn.text,
        sealed: true,
        typed: true,
        ...(voiceprint == null ? {} : { voiceprint }),
      };
    });
  state = { ...state, earlier: { bookId, sourceId, turns: entries } };
  emit();
}

/** A fresh Start shows no earlier conversation. */
export function clearEarlierConversation(bookId?: string | null): void {
  if (bookId !== undefined && !matchesVoiceScope(bookId)) return;
  if (state.earlier == null) return;
  state = { ...state, earlier: null };
  emit();
}

export function adoptRunningSession(identity: ActiveVoiceSession): void {
  const same = state.session.sessionId === identity.sessionId;
  state = {
    ...(same ? state : INITIAL_UI),
    identity,
    starting: false,
    pendingTarget: null,
    startError: null,
    session: same
      ? state.session
      : {
          ...reduceVoiceSession(INITIAL_VOICE_SESSION, { kind: "adopted" }),
          sessionId: identity.sessionId,
          purpose: identity.purpose,
        },
  };
  stateBook = { bookId: identity.bookId };
  emit();
}

export async function synchronizeVoiceSession(): Promise<ActiveVoiceSession | null> {
  await ensureRuntimeListener();
  const revision = runtimeRevision;
  const probe = ++probeRevision;
  const value = await invoke<unknown>("voice_session_active");
  if (revision !== runtimeRevision || probe !== probeRevision)
    return sessionIsActive(state) ? state.identity : null;
  const identity = activeSessionFrom(value);
  if (identity != null) adoptRunningSession(identity);
  else if (sessionIsActive(state) && !state.starting) {
    state = {
      ...state,
      identity: null,
      session: { ...state.session, status: "ended" },
    };
    emit();
  }
  return identity;
}

const MAX_JOURNAL_ENTRIES = 500;
const MAX_PENDING_TYPED = 20;

interface JournalEntry {
  at: string;
  source: "event" | "command";
  detail: unknown;
}

let journal: JournalEntry[] = [];
let pendingTypedSends: string[] = [];

function journalRecord(source: "event" | "command", detail: unknown): void {
  journal.push({ at: new Date().toISOString(), source, detail });
  if (journal.length > MAX_JOURNAL_ENTRIES) {
    journal = journal.slice(journal.length - MAX_JOURNAL_ENTRIES);
  }
}

export function noteVoiceCommand(name: string, detail?: unknown): void {
  journalRecord("command", detail === undefined ? { name } : { name, detail });
}

export function noteTypedSend(text: string): void {
  pendingTypedSends.push(text);
  if (pendingTypedSends.length > MAX_PENDING_TYPED) {
    pendingTypedSends = pendingTypedSends.slice(
      pendingTypedSends.length - MAX_PENDING_TYPED,
    );
  }
  journalRecord("command", { name: "send_text", length: text.length });
}

export function voiceSessionLogText(): string {
  return journal.map((entry) => JSON.stringify(entry)).join("\n");
}

let lastAssistantDeltaAt = 0;

export function assistantDeltaAgeMs(): number {
  return lastAssistantDeltaAt === 0
    ? Number.POSITIVE_INFINITY
    : Date.now() - lastAssistantDeltaAt;
}

export function dispatchVoiceEvent(payload: unknown): void {
  journalRecord("event", payload);
  const envelope = voiceSessionEnvelopeFrom(payload);
  if (envelope == null) return;
  // D3 id guard (F8 §2.3): UUIDv7 session ids are time-ordered, so
  // lexicographic comparison is the stale-response guard — an event from
  // an OLDER session than the one on screen is discarded before it can
  // render or coalesce; a NEWER id replaces the state wholesale below.
  const current = state.session.sessionId;
  if (current != null && envelope.sessionId < current) return;
  if (
    current === envelope.sessionId &&
    stateBook != null &&
    stateBook.bookId !== envelope.bookId
  )
    return;
  if (
    current === envelope.sessionId &&
    state.identity != null &&
    ((envelope.jobId !== undefined &&
      envelope.jobId !== state.identity.jobId) ||
      (envelope.purpose !== undefined &&
        envelope.purpose !== state.identity.purpose))
  )
    return;
  runtimeRevision += 1;

  let session = state.session;
  if (current == null) {
    // First event this store has ever seen (or post-adoption): key the
    // existing state — an adopted session's notice must survive.
    session = { ...session, sessionId: envelope.sessionId };
  } else if (envelope.sessionId > current) {
    // A fresh session's first event: nothing from the previous session
    // may survive — state, rows, or a pending typed-send match.
    session = { ...INITIAL_VOICE_SESSION, sessionId: envelope.sessionId };
    pendingTypedSends = [];
  }
  if (envelope.purpose != null)
    session = { ...session, purpose: envelope.purpose };
  let parsed = envelope.event;
  if (parsed.kind === "transcript" && parsed.role === "assistant") {
    lastAssistantDeltaAt = Date.now();
  }
  if (parsed.kind === "transcript" && parsed.role === "user") {
    const at = pendingTypedSends.indexOf(parsed.text);
    if (at !== -1) {
      pendingTypedSends.splice(at, 1);
      parsed = { ...parsed, typed: true };
    }
  }
  state = {
    ...state,
    // A stale start/command error must not outlive the session moving on:
    // lifecycle progress supersedes it, everything else keeps it visible.
    startError: parsed.kind === "lifecycle" ? null : state.startError,
    starting: false,
    session: reduceVoiceSession(session, parsed),
    earlier: state.earlier?.bookId === envelope.bookId ? state.earlier : null,
    pendingTarget: null,
    identity:
      envelope.purpose != null && envelope.jobId !== undefined
        ? {
            sessionId: envelope.sessionId,
            bookId: envelope.bookId,
            jobId: envelope.jobId,
            purpose: envelope.purpose,
          }
        : current === envelope.sessionId
          ? state.identity
          : null,
  };
  // Native provenance survives changes to the displayed panel.
  stateBook = { bookId: envelope.bookId };
  emit();
}

let runtimeReady: Promise<void> | null = null;

function ensureRuntimeListener(): Promise<void> {
  if (runtimeReady != null) return runtimeReady;
  runtimeReady = listen("voice-session-event", (event) => {
    dispatchVoiceEvent(event.payload);
  }).then(
    () => {},
    (error: unknown) => {
      runtimeReady = null;
      markVoiceError(`Voice events unavailable: ${String(error)}`);
      throw error;
    },
  );
  return runtimeReady;
}

export function resetVoiceSessionStore(): void {
  runtimeRevision += 1;
  probeRevision += 1;
  state = INITIAL_UI;
  drafts.clear();
  journal = [];
  pendingTypedSends = [];
  lastAssistantDeltaAt = 0;
  bookScope = null;
  stateBook = null;
  emit();
}
