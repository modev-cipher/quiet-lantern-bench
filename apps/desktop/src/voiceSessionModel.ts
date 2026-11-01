import { END_REASONS, type EndReason } from "./sessionHistoryStore";
import { t, type MessageKey } from "./i18n";

/** The notice as text: catalog key resolved now, else the raw message. */
export function noticeText(state: {
  notice: string | null;
  noticeKey: MessageKey | null;
}): string | null {
  return state.noticeKey != null ? t(state.noticeKey) : state.notice;
}

// Pure state for the live voice-tutor session UI (VA6 runtime slice).
//
// The Rust runtime emits `voice-session-event` (SessionUiEvent in
// voice_session.rs, serde tag "kind"); this module is the defensive parse +
// reducer the panel renders from. Honest by design: failures become visible
// notices, never retries or silence, and when the session runs text-first
// (no usable output device, or the device died) that is stated, not hidden.

export type VoiceSessionStatus =
  "idle" | "starting" | "live" | "renewing" | "ended";

/**
 * The audio-output plane as the runtime reports it (SessionUiEvent::
 * AudioState): `unknown` until the runtime says anything (also what an older
 * runtime leaves us at — indistinguishable from text-first, rendered as
 * neither promise), `speaking` while the device actually plays tutor audio.
 */
export type VoiceAudioState =
  "unknown" | "active" | "speaking" | "unavailable" | "lost";

/**
 * The mic plane as the runtime reports it (SessionUiEvent::MicState):
 * `idle` until Interactive mode is entered (also the honest initial —
 * nothing is captured before the runtime says otherwise), `listening`
 * while capture flows, `muted` while the capture gate is closed,
 * `unavailable`/`lost` when there is no usable device (the session
 * continues without a mic and says so).
 */
export type VoiceMicState =
  "idle" | "listening" | "muted" | "unavailable" | "lost";

/**
 * Which tutor answers (SessionUiEvent::Mode): `text` = the writing tutor
 * (typed questions, detailed written answers); `voice` = the live voice
 * tutor (Go Interactive). Null until the runtime says.
 */
export type VoiceTutorMode = "text" | "voice";

/**
 * What the tutor is doing (SessionUiEvent::Activity), plus `writing`,
 * which the reducer derives from the reply's own deltas.
 */
export type VoiceActivityState =
  | "idle"
  | "connecting"
  | "reconnecting"
  | "thinking"
  | "tool"
  | "retrying"
  | "fallback"
  | "writing";

/**
 * One tool step the tutor took for the reply being prepared — recorded
 * from the runtime's own `tool` activity events, never inferred. `failed`
 * is set when the runtime reported that step's navigation as failed.
 */
export interface ActivityStep {
  navigationId?: string;
  status?: "pending" | "held";
  tool: string;
  /** What it acted on (`23` for a page, a chapter title), or "". */
  target: string;
  failed: boolean;
}

export interface VoiceActivity {
  state: VoiceActivityState;
  /**
   * The tool's name (`tool`), the retry number (`retrying`), `after_tool`
   * when a tool answered and the reply is being prepared, else "".
   */
  detail: string;
  /** What the running tool acts on (see [`ActivityStep.target`]). */
  target: string;
  /** This reply's tool steps so far, oldest first (capped). */
  steps: ActivityStep[];
  /** Bumps on every change, so the panel can restart its elapsed clock. */
  seq: number;
}

/** How many steps one reply keeps (a runaway tool loop stays readable). */
export const MAX_ACTIVITY_STEPS = 6;

const ACTIVITY_STATES: readonly VoiceActivityState[] = [
  "idle",
  "connecting",
  "reconnecting",
  "thinking",
  "tool",
  "retrying",
  "fallback",
  "writing",
];

export const IDLE_ACTIVITY: VoiceActivity = {
  state: "idle",
  detail: "",
  target: "",
  steps: [],
  seq: 0,
};

export interface TranscriptEntry {
  role: "user" | "assistant";
  text: string;
  sealed?: boolean;
  /**
   * Present (true) only on typed sends. A user entry without it came from
   * provider input transcription, which is approximate by nature — the
   * panel labels it so instead of presenting the guess as a quote.
   */
  typed?: boolean;
  /**
   * The tool steps the tutor took before this reply (assistant entries
   * only): what it really did — "Turned to page 23 · Looked at the page" —
   * kept with the reply it led to.
   */
  steps?: ActivityStep[];
  voiceprint?: number[];
}

export const VOICEPRINT_BARS = 20;

export function voiceprintFrom(value: unknown): number[] | null {
  if (!Array.isArray(value) || value.length !== VOICEPRINT_BARS) return null;
  return value.every(
    (bar) =>
      Number.isInteger(bar) && (bar as number) >= 0 && (bar as number) <= 255,
  )
    ? (value as number[])
    : null;
}

const PAGE_TOOLS = new Set(["jump_to_page", "look_at_page"]);
const MAX_PAGE_PILLS = 3;

export function pagesReadIn(
  steps: readonly ActivityStep[] | undefined,
): number[] {
  const pages: number[] = [];
  for (const step of steps ?? []) {
    if (
      step.failed ||
      step.status != null ||
      !PAGE_TOOLS.has(step.tool) ||
      !/^\d+$/.test(step.target)
    )
      continue;
    const page = Number(step.target);
    if (page > 0 && !pages.includes(page)) pages.push(page);
    if (pages.length === MAX_PAGE_PILLS) break;
  }
  return pages;
}

export function foldVoiceprints(first: number[], second: number[]): number[] {
  const levels = [...first, ...second];
  return Array.from({ length: VOICEPRINT_BARS }, (_, bar) => {
    const start = Math.floor((bar * levels.length) / VOICEPRINT_BARS);
    const end = Math.min(
      levels.length,
      Math.max(
        start + 1,
        Math.floor(((bar + 1) * levels.length) / VOICEPRINT_BARS),
      ),
    );
    return Math.max(...levels.slice(start, end));
  });
}

/**
 * O3 display join (owner ruling, span-join investigation): the runtime marks
 * each COMPLETE per-span final from the transcription stream with
 * `span_final: true`. Consecutive span finals with no real turn boundary
 * between them — no assistant reply, no typed send — are one spoken
 * utterance the client VAD split at intra-utterance pauses, so the display
 * joins them into one cohesive bubble. Display only: the runtime's
 * accounting, attribution, and journal rows stay strictly per-span; this
 * reducer merely renders them together.
 */

export interface VoiceSessionState {
  purpose?: "tutor" | "agent";
  status: VoiceSessionStatus;
  audio: VoiceAudioState;
  mic: VoiceMicState;
  transcript: TranscriptEntry[];
  /** One honest status/failure line, or null when nothing needs saying. */
  /** Raw text (backend/event messages). */
  notice: string | null;
  /** Catalog notice, resolved at RENDER so a locale switch re-translates it (round-2 F3). */
  noticeKey: MessageKey | null;
  /**
   * The runtime session this state belongs to (F8 §2.3, D3): the id every
   * wire envelope carries. Null only before the first event of the process
   * (or after a test reset) — the store's id guard keys off it, so rows
   * from any other session are unrenderable, not merely uncleared.
   */
  sessionId: string | null;
  /**
   * The latest SUCCESSFUL navigation the runtime surfaced
   * (SessionUiEvent::Navigation, F8 §2.4 D4), or null before the first one.
   * `nonce` increments on every event so a repeat jump to the same page is
   * still an edge the reader can apply; the §11.1 follow law (manual-scroll
   * override) is the consumer's job, not this state's.
   */
  navigation: VoiceNavigation | null;
  navigationOutcome?: {
    callId: string;
    applied: boolean;
    held: boolean;
  } | null;
  mode: VoiceTutorMode | null;
  activity: VoiceActivity;
}

export interface VoiceNavigation {
  callId?: string;
  generation?: number;
  nonce: number;
  tool: string;
  posMs: number;
  pageNo: number | null;
  chapterIdx: number | null;
}

export const INITIAL_VOICE_SESSION: VoiceSessionState = {
  status: "idle",
  audio: "unknown",
  mic: "idle",
  transcript: [],
  notice: null,
  noticeKey: null,
  sessionId: null,
  navigation: null,
  mode: null,
  activity: IDLE_ACTIVITY,
};

export type VoiceSessionEvent =
  | {
      kind: "navigation_settled";
      callId: string;
      tool: string;
      applied: boolean;
      held: boolean;
    }
  | { kind: "adopted" }
  | { kind: "lifecycle"; state: string }
  | {
      kind: "transcript";
      role: "user" | "assistant";
      text: string;
      typed?: boolean;
      /** True on a complete per-span stream final (O3 display join). */
      spanFinal?: boolean;
      /**
       * True when this user final REPLACES the last surfaced user row's
       * text (the stream's late final correcting a dialog-leg fallback
       * row) — the UI twin of the journal's update_turn_text reconcile
       * (4a, 2026-09-28).
       */
      correctsLast?: boolean;
    }
  | { kind: "turn_boundary" }
  | { kind: "voiceprint"; bars: number[] }
  | {
      kind: "navigation";
      callId?: string;
      generation?: number;
      tool: string;
      posMs: number;
      pageNo: number | null;
      chapterIdx: number | null;
    }
  | { kind: "ended"; reason: string; endReason?: EndReason }
  | { kind: "failed"; code: string; message: string }
  | { kind: "step_failed"; tool: string }
  | { kind: "audio_state"; state: string; detail: string }
  | { kind: "mic_state"; state: string; detail: string }
  | { kind: "activity"; state: string; detail: string; target?: string }
  | { kind: "mode"; mode: VoiceTutorMode; model: string };

/**
 * The wire shape crossing the Tauri boundary (SessionUiEnvelope in
 * voice_session.rs, F8 §2.3 D3): `{ session_id, event }`. The id is what
 * makes cross-session transcript leakage unrenderable — the store discards
 * stale-id events and replaces state wholesale on a fresh id.
 */
export interface VoiceSessionEnvelope {
  jobId?: string | null;
  purpose?: "tutor" | "agent";
  sessionId: string;
  /**
   * The session's book scope (D3 extension, SessionUiWireEnvelope): the
   * book the runtime bound the session to, or null for a standalone
   * session (F6). The store's book guard keys off it so a panel mounted
   * for one book never renders another book's session — even before any
   * new session id exists.
   */
  bookId: string | null;
  event: VoiceSessionEvent;
}

/** Defensive parse of a wire envelope. An id-less payload is dropped. */
export function voiceSessionEnvelopeFrom(
  payload: unknown,
): VoiceSessionEnvelope | null {
  if (typeof payload !== "object" || payload == null) return null;
  const record = payload as Record<string, unknown>;
  if (typeof record.session_id !== "string" || record.session_id === "") {
    return null;
  }
  if (
    record.purpose !== undefined &&
    record.purpose !== "tutor" &&
    record.purpose !== "agent"
  )
    return null;
  if (
    record.job_id !== undefined &&
    record.job_id !== null &&
    typeof record.job_id !== "string"
  )
    return null;
  const event = voiceSessionEventFrom(record.event);
  if (event == null) return null;
  return {
    ...(record.job_id === null || typeof record.job_id === "string"
      ? { jobId: record.job_id }
      : {}),
    ...(record.purpose === "tutor" || record.purpose === "agent"
      ? { purpose: record.purpose }
      : {}),
    sessionId: record.session_id,
    bookId: typeof record.book_id === "string" ? record.book_id : null,
    event,
  };
}

/** Defensive parse of an event payload crossing the Tauri boundary. */
export function voiceSessionEventFrom(
  payload: unknown,
): VoiceSessionEvent | null {
  if (typeof payload !== "object" || payload == null) return null;
  const record = payload as Record<string, unknown>;
  switch (record.kind) {
    case "lifecycle":
      return typeof record.state === "string"
        ? { kind: "lifecycle", state: record.state }
        : null;
    case "transcript":
      return (record.role === "user" || record.role === "assistant") &&
        typeof record.text === "string"
        ? {
            kind: "transcript",
            role: record.role,
            text: record.text,
            ...(record.span_final === true ? { spanFinal: true } : {}),
            ...(record.corrects_last === true ? { correctsLast: true } : {}),
          }
        : null;
    case "turn_boundary":
      return { kind: "turn_boundary" };
    case "step_failed":
      return typeof record.tool === "string"
        ? { kind: "step_failed", tool: record.tool }
        : null;
    case "voiceprint": {
      const bars = voiceprintFrom(record.bars);
      return bars == null ? null : { kind: "voiceprint", bars };
    }
    case "navigation_settled":
      return typeof record.call_id === "string" &&
        typeof record.tool === "string" &&
        typeof record.applied === "boolean" &&
        typeof record.held === "boolean"
        ? {
            kind: "navigation_settled",
            callId: record.call_id,
            tool: record.tool,
            applied: record.applied,
            held: record.held,
          }
        : null;
    case "navigation_requested":
      if (
        typeof record.call_id !== "string" ||
        record.call_id.length === 0 ||
        typeof record.generation !== "number" ||
        !Number.isSafeInteger(record.generation) ||
        record.generation < 0 ||
        typeof record.page_no !== "number" ||
        !Number.isSafeInteger(record.page_no) ||
        record.page_no < 1
      )
        return null;
      return typeof record.tool === "string" &&
        typeof record.pos_ms === "number" &&
        Number.isFinite(record.pos_ms)
        ? {
            kind: "navigation",
            callId: record.call_id,
            generation: record.generation,
            tool: record.tool,
            posMs: record.pos_ms,
            pageNo: record.page_no,
            chapterIdx: null,
          }
        : null;
    case "navigation":
      return typeof record.tool === "string" &&
        typeof record.pos_ms === "number"
        ? {
            kind: "navigation",
            tool: record.tool,
            posMs: record.pos_ms,
            pageNo: typeof record.page_no === "number" ? record.page_no : null,
            chapterIdx:
              typeof record.chapter_idx === "number"
                ? record.chapter_idx
                : null,
          }
        : null;
    case "ended":
      if (typeof record.reason !== "string") return null;
      return typeof record.end_reason === "string" &&
        (END_REASONS as readonly string[]).includes(record.end_reason)
        ? {
            kind: "ended",
            reason: record.reason,
            endReason: record.end_reason as EndReason,
          }
        : { kind: "ended", reason: record.reason };
    case "failed":
      return typeof record.code === "string" &&
        typeof record.message === "string"
        ? { kind: "failed", code: record.code, message: record.message }
        : null;
    case "audio_state":
      return typeof record.state === "string" &&
        typeof record.detail === "string"
        ? { kind: "audio_state", state: record.state, detail: record.detail }
        : null;
    case "mic_state":
      return typeof record.state === "string" &&
        typeof record.detail === "string"
        ? { kind: "mic_state", state: record.state, detail: record.detail }
        : null;
    case "activity":
      return typeof record.state === "string" &&
        typeof record.detail === "string"
        ? {
            kind: "activity",
            state: record.state,
            detail: record.detail,
            target: typeof record.target === "string" ? record.target : "",
          }
        : null;
    case "mode":
      return (record.mode === "text" || record.mode === "voice") &&
        typeof record.model === "string"
        ? { kind: "mode", mode: record.mode, model: record.model }
        : null;
    default:
      return null;
  }
}

/**
 * Notice shown when this window rejoins a session the runtime already runs
 * (the UI state was lost to a remount or reload while the Rust side kept the
 * session alive). Honest about the gap: the runtime does not replay
 * transcript history, so earlier turns are not shown.
 */
export function adoptedSessionNotice(): string {
  return t("voice.notice.adopted");
}

/**
 * Recognizes the runtime's AlreadyRunning start refusal
 * (SessionStartError::AlreadyRunning in voice_session.rs). This refusal is
 * not a dead end: it proves a live session exists, so the UI adopts it
 * instead of rendering an error that blocks every control.
 */
export function isAlreadyRunningStartError(message: string): boolean {
  return message.includes("already running");
}

/** Transcript cap: a long session must not grow the UI state unbounded. */
const MAX_TRANSCRIPT_ENTRIES = 200;

/**
 * A turn must carry at least one letter or digit (any script) to be a
 * bubble. Provider input transcription streams punctuation-only frames —
 * the trailing "." of an utterance routinely lands after the assistant has
 * started speaking, and rendering it opens a lone-dot user bubble
 * (owner session 2026-08-28).
 */
export function hasLexicalContent(text: string): boolean {
  return /[\p{L}\p{N}]/u.test(text);
}

function sealed(entry: TranscriptEntry): TranscriptEntry {
  return { ...entry, sealed: true };
}

/** Seal `last` onto `head`, dropping it when nothing lexical accumulated. */
function sealOnto(
  head: TranscriptEntry[],
  last: TranscriptEntry,
): TranscriptEntry[] {
  return hasLexicalContent(last.text) ? [...head, sealed(last)] : head;
}

function appendTranscript(
  transcript: TranscriptEntry[],
  role: "user" | "assistant",
  text: string,
  typed: boolean,
  spanFinal: boolean,
): TranscriptEntry[] {
  const last = transcript[transcript.length - 1];
  // O3 display join: a per-span stream final extends the previous SPOKEN
  // user bubble — sealed or not — because nothing but a speaking edge
  // intervened. A real turn boundary (an assistant entry, a typed send)
  // makes `last` fail this guard and the final opens its own bubble. The
  // separator keeps the joined utterance readable; the underlying events
  // and journal rows remain one per span.
  if (
    spanFinal &&
    !typed &&
    role === "user" &&
    last != null &&
    last.role === "user" &&
    last.typed !== true
  ) {
    const separator = /\s$/.test(last.text) ? "" : " ";
    return [
      ...transcript.slice(0, -1),
      { ...last, text: `${last.text}${separator}${text}` },
    ];
  }
  if (!typed && last != null && last.role === role && last.sealed !== true) {
    return [
      ...transcript.slice(0, -1),
      { ...last, text: `${last.text}${text}` },
    ];
  }
  // A USER voice delta with no lexical content never OPENS a bubble — it
  // may only extend one (the coalescing branch above). Assistant deltas
  // are exempt: a reply legitimately streams an opening ellipsis or dash
  // first, and dropping it would diverge the bubble from the journal.
  if (!typed && role === "user" && !hasLexicalContent(text)) {
    return transcript;
  }
  const entry: TranscriptEntry = typed
    ? { role, text, sealed: true, typed: true }
    : { role, text };
  // Opening a new bubble closes the previous one, whichever side spoke it:
  // the role flip (or a typed send) IS the utterance boundary. Audio-plane
  // edges are NOT — playback drains between chunks, and sealing there
  // shredded replies into one-bubble-per-chunk (owner session 2026-08-29).
  const head =
    last != null && last.sealed !== true
      ? sealOnto(transcript.slice(0, -1), last)
      : transcript;
  const next = [...head, entry];
  return next.length > MAX_TRANSCRIPT_ENTRIES
    ? next.slice(next.length - MAX_TRANSCRIPT_ENTRIES)
    : next;
}

function sealLastEntry(
  transcript: TranscriptEntry[],
  role: "user" | "assistant",
): TranscriptEntry[] {
  const last = transcript[transcript.length - 1];
  if (last == null || last.role !== role || last.sealed === true) {
    return transcript;
  }
  return sealOnto(transcript.slice(0, -1), last);
}

function withActivity(
  state: VoiceSessionState,
  next: VoiceActivityState,
  detail = "",
  target = "",
): VoiceSessionState {
  const current = state.activity;
  const steps =
    next === "tool"
      ? [...current.steps, { tool: detail, target, failed: false }].slice(
          -MAX_ACTIVITY_STEPS,
        )
      : current.steps;
  if (
    next !== "tool" &&
    current.state === next &&
    current.detail === detail &&
    current.target === target
  ) {
    return state;
  }
  return {
    ...state,
    activity: { state: next, detail, target, steps, seq: current.seq + 1 },
  };
}

/** The runtime said the newest step's navigation failed. */
function withFailedStep(state: VoiceSessionState): VoiceSessionState {
  const steps = state.activity.steps;
  if (steps.length === 0) return state;
  const last = steps[steps.length - 1];
  return {
    ...state,
    activity: {
      ...state.activity,
      steps: [...steps.slice(0, -1), { ...last, failed: true }],
    },
  };
}

function markNewestStep(
  steps: readonly ActivityStep[],
  tool: string,
): ActivityStep[] | null {
  let at = steps.length - 1;
  while (at >= 0 && (steps[at].tool !== tool || steps[at].failed)) at -= 1;
  if (at === -1) return null;
  return steps.map((step, index) =>
    index === at ? { ...step, failed: true } : step,
  );
}

function withStepFailed(
  state: VoiceSessionState,
  tool: string,
): VoiceSessionState {
  const live = markNewestStep(state.activity.steps, tool);
  if (live != null)
    return { ...state, activity: { ...state.activity, steps: live } };
  const last = state.transcript.length - 1;
  const entry = last === -1 ? null : state.transcript[last];
  if (entry == null || entry.role !== "assistant" || entry.steps == null)
    return state;
  const settled = markNewestStep(entry.steps, tool);
  if (settled == null) return state;
  return {
    ...state,
    transcript: state.transcript.map((row, index) =>
      index === last ? { ...row, steps: settled } : row,
    ),
  };
}

/**
 * A reply's first words opened a new assistant bubble: the steps that led
 * to it move onto that bubble, and the next reply starts a fresh record.
 * A user turn clears an unattached record (a question that was answered
 * with no reply text keeps no orphan steps).
 */
function attachSteps(
  before: VoiceSessionState,
  after: VoiceSessionState,
  role: "user" | "assistant",
): VoiceSessionState {
  const steps = after.activity.steps;
  if (steps.length === 0) return after;
  if (role === "user") {
    return { ...after, activity: { ...after.activity, steps: [] } };
  }
  const prior = before.transcript[before.transcript.length - 1];
  const now = after.transcript[after.transcript.length - 1];
  // Opened = the reply now ends the transcript and did not before (an
  // extension of the same unsealed bubble is not an opening).
  const opened =
    now != null &&
    now.role === "assistant" &&
    (prior == null || prior.role !== "assistant" || prior.sealed === true);
  if (!opened) return after;
  const transcript = [...after.transcript];
  const at = transcript.length - 1;
  transcript[at] = { ...transcript[at], steps };
  return { ...after, transcript, activity: { ...after.activity, steps: [] } };
}

/** Runtime notices that have a catalog line (resolved at render). */
const FAILED_NOTICE_KEYS: Record<string, MessageKey> = {
  mic_unavailable_typing: "voice.notice.micTyping",
  typing_unavailable: "voice.notice.typingUnavailable",
  reply_failed: "voice.notice.replyFailed",
};

export function reduceVoiceSession(
  state: VoiceSessionState,
  event: VoiceSessionEvent,
): VoiceSessionState {
  switch (event.kind) {
    case "adopted":
      return {
        ...state,
        status: "live",
        notice: null,
        noticeKey: "voice.notice.adopted",
      };
    case "lifecycle":
      switch (event.state) {
        case "connected":
        case "reconnected":
          return { ...state, status: "live", notice: null, noticeKey: null };
        case "renewing":
          return {
            ...state,
            status: "renewing",
            notice: null,
            noticeKey: "voice.notice.reconnecting",
          };
        case "expired":
          // The runtime ends the session next; say why in the meantime.
          return { ...state, notice: null, noticeKey: "voice.notice.expired" };
        default:
          return state;
      }
    case "transcript":
      if (
        event.correctsLast === true &&
        event.role === "user" &&
        state.transcript.length > 0
      ) {
        const lastUser = [...state.transcript]
          .reverse()
          .findIndex((entry) => entry.role === "user");
        if (lastUser !== -1) {
          const at = state.transcript.length - 1 - lastUser;
          const transcript = [...state.transcript];
          transcript[at] = { ...transcript[at], text: event.text };
          return { ...state, transcript };
        }
      }
      {
        const next = {
          ...state,
          transcript: appendTranscript(
            state.transcript,
            event.role,
            event.text,
            event.typed === true,
            event.spanFinal === true,
          ),
        };
        const attached = attachSteps(state, next, event.role);
        return event.role === "assistant" && event.text.trim() !== ""
          ? withActivity(attached, "writing")
          : attached;
      }
    case "voiceprint": {
      let at = state.transcript.length - 1;
      while (at >= 0 && state.transcript[at].role !== "user") at -= 1;
      const target = at === -1 ? null : state.transcript[at];
      if (target == null || target.typed === true) return state;
      const transcript = [...state.transcript];
      transcript[at] = {
        ...target,
        voiceprint:
          target.voiceprint == null
            ? event.bars
            : foldVoiceprints(target.voiceprint, event.bars),
      };
      return { ...state, transcript };
    }
    case "turn_boundary":
      // The runtime's seal signal (2026-09-28 D1): the journal just sealed
      // its open turn at a suppressed user flip or a non-anchor tool
      // seam — seal the open ASSISTANT bubble NOW so reply deltas arriving
      // during the stream-final lag window open their own bubble instead
      // of coalescing into the question.
      return {
        ...state,
        transcript: sealLastEntry(state.transcript, "assistant"),
      };
    case "ended": {
      // The runtime drops the mic plane with the session: an ended session
      // must not keep a hot Listening/Muted badge (or its Mute button)
      // promising a capture state that no longer exists.
      //
      // Kritias round 5 finding 3: the runtime surfaces teardown turn loss
      // (voice_turn_uncertain / voice_turn_lost) immediately BEFORE Ended.
      // With a single notice slot the terminal line would overwrite the
      // loss warning — the one fact the user must leave the session with —
      // so a standing loss notice folds INTO the terminal line instead of
      // vanishing under it.
      const lossMatch =
        state.notice == null
          ? null
          : /^voice_turn_(?:uncertain|lost): (.*)$/s.exec(state.notice);
      const ended = `Session ended: ${event.reason}`;
      return {
        ...state,
        status: "ended",
        mic: "idle",
        activity: { ...IDLE_ACTIVITY, seq: state.activity.seq + 1 },
        transcript: sealLastEntry(
          sealLastEntry(state.transcript, "assistant"),
          "user",
        ),
        notice: lossMatch == null ? ended : `${ended} — ${lossMatch[1]}`,
        noticeKey: null,
      };
    }
    case "step_failed":
      return withStepFailed(state, event.tool);
    case "failed": {
      // A failed navigation is the newest step's own outcome: the step
      // trail says so instead of claiming the page turned.
      const marked =
        event.code.startsWith("navigation") || event.code === "unknown_tool"
          ? withFailedStep(state)
          : state;
      // Non-fatal by contract (a fatal failure is followed by "ended"):
      // the session keeps running, the user sees what went wrong.
      return withActivity(
        FAILED_NOTICE_KEYS[event.code] != null
          ? {
              ...marked,
              notice: null,
              noticeKey: FAILED_NOTICE_KEYS[event.code],
            }
          : {
              ...marked,
              notice: `${event.code}: ${event.message}`,
              noticeKey: null,
            },
        "idle",
      );
    }
    case "navigation_settled": {
      const settle = (steps: ActivityStep[] | undefined, fallback = false) =>
        steps?.map((step, at) => {
          if (
            step.navigationId !== event.callId &&
            !(
              fallback &&
              step.navigationId == null &&
              step.tool === event.tool &&
              at === steps.length - 1
            )
          )
            return step;
          const rest = { ...step, status: undefined };
          return {
            ...rest,
            failed: !event.applied && !event.held,
            ...(event.held ? { status: "held" as const } : {}),
          };
        });
      return {
        ...state,
        ...(!event.applied &&
        !event.held &&
        state.navigation?.callId === event.callId
          ? { navigation: null }
          : {}),
        ...(event.applied &&
        state.noticeKey === "voice.notice.navigationNotApplied"
          ? { noticeKey: null, notice: null }
          : {}),
        navigationOutcome: {
          callId: event.callId,
          applied: event.applied,
          held: event.held,
        },
        ...(!event.applied &&
        !event.held &&
        state.navigation?.callId === event.callId
          ? {
              noticeKey: "voice.notice.navigationNotApplied" as MessageKey,
              notice: null,
            }
          : {}),
        activity: {
          ...state.activity,
          steps: settle(state.activity.steps, true) ?? [],
        },
        transcript: state.transcript.map((row) =>
          row.steps == null ? row : { ...row, steps: settle(row.steps) },
        ),
      };
    }
    case "navigation":
      return {
        ...state,
        ...(event.callId == null
          ? {}
          : {
              activity: {
                ...state.activity,
                steps: state.activity.steps.map((step, at, all) =>
                  at === all.length - 1 && step.tool === event.tool
                    ? {
                        ...step,
                        navigationId: event.callId,
                        status: "pending" as const,
                      }
                    : step,
                ),
              },
            }),
        navigation: {
          ...(event.callId == null
            ? {}
            : { callId: event.callId, generation: event.generation }),
          nonce: (state.navigation?.nonce ?? 0) + 1,
          tool: event.tool,
          posMs: event.posMs,
          pageNo: event.pageNo,
          chapterIdx: event.chapterIdx,
        },
        // F10-D2: a move the reader cannot turn to must still be visible —
        // otherwise the tutor's narration is the only witness it happened.
        ...(event.pageNo == null
          ? {
              noticeKey: "voice.notice.navigatedNoPage" as MessageKey,
              notice: null,
            }
          : {}),
      };
    case "activity":
      return (ACTIVITY_STATES as readonly string[]).includes(event.state)
        ? withActivity(
            state,
            event.state as VoiceActivityState,
            event.detail,
            event.target ?? "",
          )
        : state;
    case "mode": {
      // Back on the writing tutor there is no mic and no audio plane: the
      // badges the voice tutor left must not keep promising either. The
      // reply the switch cut is sealed: the other tutor's first words open
      // their own bubble (review 2026-10-09, J3).
      const sealed = {
        ...state,
        transcript: sealLastEntry(state.transcript, "assistant"),
      };
      return event.mode === "text"
        ? { ...sealed, mode: "text", mic: "idle", audio: "unknown" }
        : { ...sealed, mode: "voice" };
    }
    case "audio_state":
      switch (event.state) {
        case "active":
        case "idle": // the speaking edge falling back to a live device
          // Deliberately NOT a transcript seal: the plane reports idle
          // whenever playback drains, including between chunks of one
          // reply. The assistant bubble closes on the next role flip or
          // session end (appendTranscript / "ended"), never on audio
          // timing.
          return { ...state, audio: "active" };
        case "speaking":
          return {
            ...state,
            audio: "speaking",
            transcript: sealLastEntry(state.transcript, "user"),
          };
        case "unavailable":
          return {
            ...state,
            audio: "unavailable",
            notice: null,
            noticeKey: "voice.notice.noAudioOut",
          };
        case "lost":
          return {
            ...state,
            audio: "lost",
            notice: null,
            noticeKey: "voice.notice.audioLost",
          };
        default:
          // Forward-compatible: an unknown audio state changes nothing.
          return state;
      }
    case "mic_state":
      switch (event.state) {
        case "idle":
          return { ...state, mic: "idle" };
        case "listening":
          return { ...state, mic: "listening" };
        case "muted":
          return { ...state, mic: "muted" };
        case "unavailable":
          return {
            ...state,
            mic: "unavailable",
            notice: null,
            noticeKey: "voice.notice.noMic",
          };
        case "lost":
          return {
            ...state,
            mic: "lost",
            notice: null,
            noticeKey: "voice.notice.micLost",
          };
        default:
          // Forward-compatible: an unknown mic state changes nothing.
          return state;
      }
  }
}

/** Tools grouped by what the user sees the tutor doing. */
const TOOL_ACTIVITY_KEYS: Record<string, MessageKey> = {
  jump_to_page: "voice.activity.page",
  jump_to_chapter: "voice.activity.page",
  replay_previous_page: "voice.activity.page",
  go_to_passage: "voice.activity.page",
  open_passage: "voice.activity.page",
  look_at_page: "voice.activity.look",
  get_context_around_position: "voice.activity.read",
  get_context_for_range: "voice.activity.read",
  summarize_section: "voice.activity.summarize",
  update_learning_profile: "voice.activity.profile",
  search_book: "voice.activity.search",
  pause_playback: "voice.activity.playback",
  resume_playback: "voice.activity.playback",
  seek_relative: "voice.activity.playback",
  set_playback_speed: "voice.activity.playback",
  replay_previous_paragraph: "voice.activity.playback",
  replay_last_minute: "voice.activity.playback",
};

/**
 * The activity line's text, or null when there is nothing to say: idle,
 * and `writing` (the reply itself is on screen, streaming).
 */
export function activityLabelKey(activity: VoiceActivity): MessageKey | null {
  return activityMessage(activity)?.key ?? null;
}

export interface ActivityMessage {
  key: MessageKey;
  params?: Record<string, string | number>;
}

/** A page number the runtime named, or null (never guessed). */
function pageOf(target: string): number | null {
  const page = Number.parseInt(target, 10);
  return Number.isFinite(page) && page > 0 && String(page) === target.trim()
    ? page
    : null;
}

/**
 * The activity line, with what the step acts on when the runtime named it
 * ("Turning to page 23…"). An unknown wait reads only "Thinking…": the
 * panel never claims a specific action the runtime did not report.
 */
export function activityMessage(
  activity: VoiceActivity,
): ActivityMessage | null {
  switch (activity.state) {
    case "idle":
    case "writing":
      return null;
    case "connecting":
      return {
        key:
          activity.detail === "typing"
            ? "voice.activity.typingBack"
            : "voice.activity.connecting",
      };
    case "reconnecting":
      return { key: "voice.activity.reconnecting" };
    case "thinking":
      return {
        key:
          activity.detail === "after_tool"
            ? "voice.activity.prepare"
            : "voice.activity.thinking",
      };
    case "tool": {
      const page = pageOf(activity.target);
      switch (activity.detail) {
        case "jump_to_page":
          if (page != null)
            return { key: "voice.activity.pageTo", params: { page } };
          break;
        case "look_at_page":
          if (page != null)
            return { key: "voice.activity.lookAt", params: { page } };
          break;
        case "jump_to_chapter":
          if (activity.target.trim() !== "")
            return {
              key: "voice.activity.chapterTo",
              params: { title: activity.target.trim() },
            };
          break;
      }
      return {
        key: TOOL_ACTIVITY_KEYS[activity.detail] ?? "voice.activity.tool",
      };
    }
    case "retrying": {
      const attempt = Number.parseInt(activity.detail, 10);
      return Number.isFinite(attempt) && attempt > 0
        ? { key: "voice.activity.retryingN", params: { attempt } }
        : { key: "voice.activity.retrying" };
    }
    case "fallback":
      return { key: "voice.activity.fallback" };
  }
}

const TOOL_STEP_KEYS: Record<string, MessageKey> = {
  jump_to_page: "voice.step.pageAny",
  jump_to_chapter: "voice.step.pageAny",
  replay_previous_page: "voice.step.pageAny",
  go_to_passage: "voice.step.pageAny",
  open_passage: "voice.step.pageAny",
  look_at_page: "voice.step.lookAny",
  get_context_around_position: "voice.step.read",
  get_context_for_range: "voice.step.read",
  summarize_section: "voice.step.summarize",
  update_learning_profile: "voice.step.profile",
  search_book: "voice.step.search",
  pause_playback: "voice.step.playback",
  resume_playback: "voice.step.playback",
  seek_relative: "voice.step.playback",
  set_playback_speed: "voice.step.playback",
  replay_previous_paragraph: "voice.step.playback",
  replay_last_minute: "voice.step.playback",
};

/** One finished step, in the past tense ("Turned to page 23"). */
export function stepMessage(step: ActivityStep): ActivityMessage {
  if (step.status === "pending") return { key: "voice.step.navigationPending" };
  if (step.status === "held") return { key: "voice.step.navigationHeld" };
  const page = pageOf(step.target);
  if (step.tool === "jump_to_page" && page != null)
    return { key: "voice.step.page", params: { page } };
  if (step.tool === "look_at_page" && page != null)
    return { key: "voice.step.look", params: { page } };
  if (step.tool === "jump_to_chapter" && step.target.trim() !== "")
    return { key: "voice.step.chapter", params: { title: step.target.trim() } };
  return { key: TOOL_STEP_KEYS[step.tool] ?? "voice.step.tool" };
}

/**
 * Badge text for the audio-output state, or null when there is nothing
 * honest to claim (`unknown`: the runtime never reported — promise neither
 * audio nor text-first).
 */
export function audioBadgeOf(audio: VoiceAudioState): string | null {
  switch (audio) {
    case "unknown":
      return null;
    case "active":
      return t("voice.badge.audioOn");
    case "speaking":
      return t("voice.badge.speaking");
    case "unavailable":
    case "lost":
      return t("voice.badge.textOnly");
  }
}

/**
 * Badge text for the mic state, or null when there is nothing to claim
 * (`idle`: Interactive mode not entered — no mic promise either way).
 * "Listening" is deliberately a different word from the audio badge's
 * "Speaking…": the two planes are independent and both can be true.
 */
export function micBadgeOf(mic: VoiceMicState): string | null {
  switch (mic) {
    case "idle":
      return null;
    case "listening":
      return t("voice.badge.listening");
    case "muted":
      return t("voice.badge.muted");
    case "unavailable":
    case "lost":
      return t("voice.badge.noMic");
  }
}

export function statusLabelOf(status: VoiceSessionStatus): string {
  switch (status) {
    case "idle":
      return t("voice.status.off");
    case "starting":
      return t("voice.status.starting");
    case "live":
      return t("voice.status.live");
    case "renewing":
      return t("voice.status.reconnecting");
    case "ended":
      return t("voice.status.ended");
  }
}
