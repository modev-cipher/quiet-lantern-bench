// Live voice-tutor session surface (VA6 runtime slice + the owner's dead-end
// report): while a session runs the panel renders AS the session — live
// state, mic gate, tutor activity, transcript, End — never as an error
// blocking a Start button. Session state lives in voiceSessionStore so a
// remount cannot forget a running session; a runtime AlreadyRunning refusal
// is adopted as the live session instead of surfaced as a dead end.
//
// Provider selection is honest about the roster: Gemini Live is the one
// startable tutor channel today, so the picker is a labeled fact plus the
// key state (VA3's vocabulary), not a fake dropdown.

import {
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  INITIAL_VOICE_SESSION,
  activityMessage,
  isAlreadyRunningStartError,
  pagesReadIn,
  statusLabelOf,
  noticeText,
  type VoiceMicState,
  type VoiceSessionStatus,
} from "./voiceSessionModel";
import {
  synchronizeVoiceSession,
  getActiveVoiceSnapshot,
  sessionIsActive,
  matchesVoiceScope,
  getVoiceDraft,
  setVoiceDraft,
  bindVoicePanelBook,
  clearEarlierConversation,
  showEarlierConversation,
  markStarting,
  markVoiceError,
  noteTypedSend,
  noteVoiceCommand,
  subscribeVoice,
  voiceSessionLogText,
} from "./voiceSessionStore";
import { ensureHalfDuplexGate, userMute, userUnmute } from "./voiceHalfDuplex";
import SessionHistory from "./SessionHistory";
import { historyVisible, type OpenTarget } from "./sessionHistoryStore";
import { F11_STRINGS } from "./f11Strings";
import {
  VOICE_KEYS_CHANGED_EVENT,
  SETUP_CHANGED_EVENT,
  goToProvider,
  returnToTutor,
} from "./views";
import { useI18n, type MessageKey } from "./i18n";
import { providerDisplayName } from "./i18n/providerNames";
import { getSnapshot } from "./playbackStore";
import TutorRichText from "./TutorRichText";
import {
  TutorActivityAnnouncer,
  TutorActivityStrip,
  TutorSteps,
} from "./TutorActivity";
import type { SessionDetail } from "./sessionHistoryStore";
import Button from "./ui/Button";
import SegmentedSwitch from "./ui/SegmentedSwitch";
import MicToggle from "./ui/MicToggle";
import Orb, { type OrbKind } from "./ui/Orb";
import Voiceprint from "./ui/Voiceprint";
import PagePeeks from "./PagePeek";
import {
  ChevronIcon,
  KeyboardIcon,
  MicIcon,
  PowerIcon,
  SendIcon,
  StopIcon,
} from "./Icons";

/** The activity line shows its clock only once a wait is noticeable. */
/** A voice reply counts as still arriving this long after its last delta. */
const STREAM_QUIET_MS = 1500;

interface Props {
  /** null = standalone session (F6): no book attached, ever. */
  bookId: string | null;
  /** null = no narration job — session still starts (F6); only
   * playback-anchored affordances are absent. */
  jobId: string | null;
  variant?: "side" | "strip";
  onOpenTarget?: (target: OpenTarget) => void;
}

type TutorReadiness = "present" | "missing" | "locked" | "unavailable";
type KeyPresence =
  "unknown" | "present" | "missing" | "locked" | "unavailable" | "error";

/// Each non-startable key state names its own remedy — telling a user whose
/// vault is merely locked to "add a key" sends them to re-paste a key they
/// already stored (the owner-reported "Not Available" trap).
// OWNER-TUNABLE (F4): how a voice-transcribed user bubble discloses that it
// is the provider's approximation of what was said, not a quote.

const KEY_PRESENCE_HINT_KEYS: Partial<Record<KeyPresence, MessageKey>> = {
  missing: "voice.hint.missing",
  locked: "voice.hint.locked",
  error: "voice.hint.error",
  unavailable: "voice.hint.unavailable",
};

function copyViaHiddenTextarea(payload: string): boolean {
  const area = document.createElement("textarea");
  area.value = payload;
  area.setAttribute("readonly", "");
  area.style.position = "fixed";
  area.style.opacity = "0";
  document.body.appendChild(area);
  area.select();
  let copied = false;
  try {
    copied = document.execCommand("copy");
  } catch {
    copied = false;
  }
  area.remove();
  return copied;
}

function rowClass(role: "user" | "assistant"): string {
  return role === "user"
    ? "tutor-bubble ms-auto max-w-[88%] rounded-[18px] rounded-ee-md bg-ink-850 px-3.5 py-2 text-secondary leading-snug text-ink-100"
    : "tutor-voice me-auto w-full py-0.5";
}

interface NowLine {
  kind: OrbKind;
  title: string;
  sub: string;
}

function nowLineOf(
  status: VoiceSessionStatus,
  speaking: boolean,
  talking: boolean,
  mic: VoiceMicState,
  t: (key: MessageKey) => string,
): NowLine | null {
  if (status === "renewing") {
    return {
      kind: "warn",
      title: t("voice.now.reconnecting"),
      sub: t("voice.now.reconnectingSub"),
    };
  }
  if (speaking) {
    return {
      kind: "speak",
      title: t("voice.now.speaking"),
      sub: t("voice.now.speakingSub"),
    };
  }
  if (!talking) return null;
  if (mic === "muted") {
    return {
      kind: "rest",
      title: t("voice.now.muted"),
      sub: t("voice.now.mutedSub"),
    };
  }
  if (mic === "listening") {
    return {
      kind: "listen",
      title: t("voice.now.listening"),
      sub: t("voice.now.listeningSub"),
    };
  }
  if (mic === "unavailable" || mic === "lost") {
    return {
      kind: "rest",
      title: t("voice.now.noMic"),
      sub: t("voice.now.noMicSub"),
    };
  }
  return null;
}

export default function VoiceSessionPanel({
  bookId,
  jobId,
  variant = "strip",
  onOpenTarget,
}: Props) {
  const { t } = useI18n();
  const side = variant === "side";
  const [open, setOpen] = useState(side);
  const snapshot = useSyncExternalStore(
    subscribeVoice,
    getActiveVoiceSnapshot,
    getActiveVoiceSnapshot,
  );
  const scoped = matchesVoiceScope(bookId, jobId);
  const session = scoped ? snapshot.session : INITIAL_VOICE_SESSION;
  const starting = scoped ? snapshot.starting : false;
  const startError = scoped ? snapshot.startError : null;
  const elsewhere = sessionIsActive(snapshot) && !scoped;
  const [keyRevision, setKeyRevision] = useState(0);
  const [keyPresence, setKeyPresence] = useState<KeyPresence>("unknown");
  const [detailsOpen, setDetailsOpen] = useState(false);
  /** A live BOOK AGENT session blocks Tutor start until the user explicitly
   * confirms the switch (charter §2.2, the Agent->Tutor direction). */
  const [agentSwitchNeeded, setAgentSwitchNeeded] = useState(false);
  const [continueBlocked, setContinueBlocked] = useState(false);
  const draftKey = `${bookId ?? "standalone"}:${jobId ?? "direct"}:${session.sessionId ?? "new"}`;
  const draft = useSyncExternalStore(
    subscribeVoice,
    () => getVoiceDraft(draftKey),
    () => getVoiceDraft(draftKey),
  );
  const setDraft = useCallback(
    (value: string) => setVoiceDraft(draftKey, value),
    [draftKey],
  );

  // D3 book isolation (F8, extended): scope the voice store to THIS book
  // before anything renders — a mount for a different book than the store
  // last served must show zero rows from that other book's session, while
  // a same-book remount mid-session keeps its own rows. Binding during
  // render would mutate shared state (and emit) mid-render, so the effect
  // is the seam; the render-time `scoped` gate above covers the one frame
  // before it runs.
  useEffect(() => {
    bindVoicePanelBook(bookId);
  }, [bookId]);

  useEffect(() => {
    let latest = 0;
    const read = () => {
      const request = ++latest;
      setKeyPresence("unknown");
      invoke<TutorReadiness>("voice_tutor_readiness")
        .then((readiness) => {
          if (request === latest) setKeyPresence(readiness);
        })
        .catch(() => {
          if (request === latest) setKeyPresence("error");
        });
    };
    read();
    window.addEventListener(VOICE_KEYS_CHANGED_EVENT, read);
    window.addEventListener(SETUP_CHANGED_EVENT, read);
    return () => {
      latest = -1;
      window.removeEventListener(VOICE_KEYS_CHANGED_EVENT, read);
      window.removeEventListener(SETUP_CHANGED_EVENT, read);
    };
  }, [keyRevision, startError]);

  const live = session.status === "live" || session.status === "renewing";

  useEffect(() => {
    if (live) setOpen(true);
    else setContinueBlocked(false);
  }, [live]);

  const launch = useCallback(
    (resumeFrom: string | null) => {
      noteVoiceCommand(
        "start",
        resumeFrom == null ? undefined : { resumeFrom },
      );
      // Read the clock at press time — the panel needs no live subscription.
      // A book-only session (no narration job) reads this value as a
      // CHARACTER OFFSET into the book (lib.rs BookCursor), so a cast
      // playing in the background must never leak its audio clock in: the
      // fresh session starts at the book's beginning, the first page shown.
      const playbackPosMs =
        jobId == null
          ? 0
          : Math.max(0, Math.round(getSnapshot().current * 1000));
      invoke("voice_session_start", {
        bookId,
        jobId,
        playbackPosMs,
        resumeFrom,
      }).catch(async (error: unknown) => {
        const message = String(error);
        if (isAlreadyRunningStartError(message) && resumeFrom != null) {
          setContinueBlocked(true);
        }
        if (isAlreadyRunningStartError(message)) {
          try {
            const identity = await synchronizeVoiceSession();
            if (identity == null) {
              markVoiceError(message, { bookId });
              return;
            }
            if (identity.purpose === "agent") {
              setAgentSwitchNeeded(true);
              return;
            }
            if (identity.bookId !== bookId || identity.jobId !== jobId) {
              returnToTutor();
              return;
            }
            setOpen(true);
          } catch (error: unknown) {
            markVoiceError(String(error), { bookId });
          }
          return;
        }
        markVoiceError(message, { bookId });
      });
    },
    [bookId, jobId],
  );

  const start = useCallback(() => {
    const active = getActiveVoiceSnapshot();
    if (sessionIsActive(active)) {
      if (active.identity?.purpose === "agent") setAgentSwitchNeeded(true);
      else returnToTutor();
      return;
    }
    clearEarlierConversation();
    markStarting({ bookId, jobId });
    setContinueBlocked(false);
    launch(null);
  }, [launch, bookId, jobId]);

  const continueFrom = useCallback(
    (sessionId: string) => {
      markStarting({ bookId, jobId });
      setContinueBlocked(false);
      launch(sessionId);
      // Continue picks the conversation up where it stopped: show it, so
      // the panel does not open on an empty chat (field session
      // 2026-10-09). Best-effort — the runtime already carries it as the
      // tutor's recap either way.
      invoke<SessionDetail>("voice_session_transcript", { sessionId })
        .then((detail) => {
          if (detail.row.book_id === bookId)
            showEarlierConversation(bookId, sessionId, detail.turns);
        })
        .catch(() => {
          clearEarlierConversation(bookId);
        });
    },
    [bookId, jobId, launch],
  );

  const stop = useCallback(() => {
    noteVoiceCommand("stop");
    invoke("voice_session_stop").catch((error: unknown) => {
      markVoiceError(String(error), { bookId, sessionId: session.sessionId });
    });
  }, [bookId, session.sessionId]);

  const sendDraft = useCallback(() => {
    const text = draft.trim();
    if (text === "") return;
    setDraft("");
    noteTypedSend(text);
    invoke("voice_session_send_text", { text }).catch((error: unknown) => {
      markVoiceError(String(error), { bookId, sessionId: session.sessionId });
    });
  }, [draft, setDraft, bookId, session.sessionId]);

  const interrupt = useCallback(() => {
    noteVoiceCommand("interrupt");
    invoke("voice_session_interrupt").catch((error: unknown) => {
      markVoiceError(String(error), { bookId, sessionId: session.sessionId });
    });
  }, [bookId, session.sessionId]);

  const enterInteractive = useCallback(() => {
    noteVoiceCommand("enter_interactive");
    invoke("voice_session_enter_interactive").catch((error: unknown) => {
      markVoiceError(String(error), { bookId, sessionId: session.sessionId });
    });
  }, [bookId, session.sessionId]);

  const backToTyping = useCallback(() => {
    noteVoiceCommand("leave_interactive");
    invoke("voice_session_leave_interactive").catch((error: unknown) => {
      markVoiceError(String(error), { bookId, sessionId: session.sessionId });
    });
  }, [bookId, session.sessionId]);

  const muted = session.mic === "muted";

  useEffect(() => {
    ensureHalfDuplexGate();
  }, []);

  const mute = useCallback(() => {
    userMute().catch((error: unknown) => {
      markVoiceError(String(error), { bookId, sessionId: session.sessionId });
    });
  }, [bookId, session.sessionId]);

  const unmute = useCallback(() => {
    userUnmute().catch((error: unknown) => {
      markVoiceError(String(error), { bookId, sessionId: session.sessionId });
    });
  }, [bookId, session.sessionId]);

  const [logCopied, setLogCopied] = useState(false);

  const copyLog = useCallback(() => {
    const text = voiceSessionLogText();
    const payload = text === "" ? "(voice session log is empty)" : text;
    const copied = () => {
      setLogCopied(true);
      window.setTimeout(() => setLogCopied(false), 2000);
    };
    const failed = (detail: string) => {
      markVoiceError(t("voice.copyLogFailed", { detail }));
    };
    if (navigator.clipboard?.writeText != null) {
      navigator.clipboard
        .writeText(payload)
        .then(copied)
        .catch((error: unknown) => {
          if (copyViaHiddenTextarea(payload)) copied();
          else failed(String(error));
        });
      return;
    }
    if (copyViaHiddenTextarea(payload)) copied();
    else failed(t("voice.clipboardUnavailable"));
  }, [t]);

  const status = starting ? "starting" : session.status;
  const notice = startError ?? noticeText(session);
  const speaking = session.audio === "speaking";
  const listening = session.mic === "listening";
  const interactive = session.mic === "listening" || session.mic === "muted";
  const talking = session.mode === "voice";
  const micMissing = session.mic === "unavailable" || session.mic === "lost";
  const talkAvailable =
    talking || (!micMissing && session.activity.state !== "connecting");
  const textOnly = session.audio === "unavailable" || session.audio === "lost";
  const modeHintId = useId();
  const modeHint = [
    talking ? t("voice.mode.talkHint") : t("voice.mode.typeHint"),
    micMissing ? t("voice.mode.noMic") : null,
    textOnly ? t("voice.mode.textOnly") : null,
  ]
    .filter((part) => part != null)
    .join(" ");
  const nowLine = nowLineOf(session.status, speaking, talking, session.mic, t);
  const historyShown = open && historyVisible(session.status, starting);
  const earlier = scoped ? snapshot.earlier : null;
  const activity = live ? activityMessage(session.activity) : null;
  const activityKey = activity?.key ?? null;
  const transcriptLength = session.transcript.length;
  const lastEntry = session.transcript[transcriptLength - 1];
  const lastAssistantText =
    lastEntry?.role === "assistant" ? lastEntry.text : null;

  // The activity line's clock: seconds since the tutor entered its current
  // state, shown once the wait is noticeable.
  const [activitySeconds, setActivitySeconds] = useState(0);
  useEffect(() => {
    setActivitySeconds(0);
    if (activityKey == null) return;
    const started = Date.now();
    const timer = window.setInterval(() => {
      setActivitySeconds(Math.floor((Date.now() - started) / 1000));
    }, 1000);
    return () => window.clearInterval(timer);
  }, [activityKey, session.activity.seq]);

  // A reply is "still arriving" while deltas keep landing; the caret and
  // the glide stop once it goes quiet (the voice tutor sends no explicit
  // end), or at once when the runtime says the reply is done.
  const [recentDelta, setRecentDelta] = useState(false);
  useEffect(() => {
    if (lastAssistantText == null) return;
    setRecentDelta(true);
    const timer = window.setTimeout(
      () => setRecentDelta(false),
      STREAM_QUIET_MS,
    );
    return () => window.clearTimeout(timer);
  }, [lastAssistantText]);
  const streamingLast =
    live &&
    lastEntry?.role === "assistant" &&
    lastEntry.sealed !== true &&
    session.activity.state === "writing" &&
    recentDelta;

  // Follow the conversation down while the reader is at the bottom; a
  // reader who scrolled up to re-read is left where they are.
  const listRef = useRef<HTMLDivElement | null>(null);
  const pinnedRef = useRef(true);
  // A new session starts at the bottom whatever the last one's reader did
  // (review 2026-10-09, U3: a short new list fires no scroll event, so a
  // stale "scrolled up" never cleared).
  useEffect(() => {
    pinnedRef.current = true;
  }, [session.sessionId]);
  useEffect(() => {
    const list = listRef.current;
    if (list == null) return;
    const content = list.firstElementChild;
    const follow = () => {
      if (pinnedRef.current) list.scrollTop = list.scrollHeight;
    };
    follow();
    if (content == null || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(follow);
    observer.observe(content);
    return () => observer.disconnect();
  }, [open, transcriptLength, earlier]);

  return (
    <div
      data-testid="voice-session-panel"
      data-variant={variant}
      className={
        side
          ? "flex h-full w-full shrink-0 flex-col border-s border-ink-800 bg-ink-900 py-4 ps-[max(1rem,env(safe-area-inset-left))] pe-[max(1rem,env(safe-area-inset-right))]"
          : "shrink-0 border-t border-ink-800 bg-ink-900 py-3 ps-[max(1.5rem,env(safe-area-inset-left))] pe-[max(1.5rem,env(safe-area-inset-right))]"
      }
    >
      <div className="flex min-h-10 items-center gap-1.5">
        <Button
          icon
          size="sm"
          label={open ? t("voice.collapse") : t("voice.expand")}
          data-testid="voice-session-toggle"
          aria-expanded={open}
          onClick={() => setOpen((current) => !current)}
        >
          <span
            className={`inline-flex transition-transform duration-300 ease-[cubic-bezier(0.32,0.72,0,1)] ${open ? "rotate-90 rtl:-rotate-90" : ""}`}
          >
            <ChevronIcon className="h-4 w-4" />
          </span>
        </Button>
        <div className="flex min-w-0 flex-1 flex-col">
          <span className="truncate text-[14px] font-semibold leading-tight text-ink-100">
            {t(session.purpose === "agent" ? "tutor.bookAgent" : "voice.title")}
          </span>
          <span
            data-testid="voice-session-status"
            data-status={status}
            className={`inline-flex items-center gap-1.5 whitespace-nowrap text-label font-medium ${
              status === "live"
                ? "text-wash-green-ink"
                : status === "renewing"
                  ? "text-wash-yellow-ink"
                  : "text-ink-500"
            }`}
          >
            {status === "live" && (
              <span
                aria-hidden="true"
                className="ui-presence-dot"
                data-breathing="true"
              />
            )}
            {(status === "starting" || status === "renewing") && (
              <span aria-hidden="true" className="ui-spin" />
            )}
            {statusLabelOf(status)}
          </span>
        </div>
        {live ? (
          <Button
            variant="danger"
            data-testid="voice-session-stop"
            onClick={stop}
            leading={<PowerIcon />}
          >
            {t("voice.endSession")}
          </Button>
        ) : (
          !side &&
          !agentSwitchNeeded &&
          !elsewhere && (
            <Button
              variant="solid"
              size="sm"
              data-testid="voice-session-start"
              busy={starting}
              disabled={keyPresence !== "present"}
              onClick={start}
            >
              {starting ? t("voice.starting") : t("voice.start")}
            </Button>
          )
        )}
      </div>
      {elsewhere && !agentSwitchNeeded && (
        <div
          className="mt-3 flex flex-wrap items-center gap-2 text-secondary text-ink-300"
          data-testid="voice-session-elsewhere"
        >
          <span>{t("voice.return.elsewhere")}</span>
          <Button onClick={returnToTutor}>{t("voice.return.action")}</Button>
        </div>
      )}
      {agentSwitchNeeded && (
        <div
          role="alert"
          className="mt-3 flex flex-col gap-3 rounded-2xl bg-wash-yellow p-3.5 text-secondary text-wash-yellow-ink"
        >
          <span>{t("voice.switchWarning")}</span>
          <div className="flex flex-col gap-1.5">
            <Button
              variant="solid"
              block
              data-testid="voice-session-switch-from-agent"
              onClick={() => {
                setAgentSwitchNeeded(false);
                void invoke("agent_session_stop")
                  .then(() => synchronizeVoiceSession())
                  .then(() => start())
                  .catch(() => setAgentSwitchNeeded(true));
              }}
            >
              {t("voice.switchConfirm")}
            </Button>
            <Button
              block
              data-testid="voice-session-switch-cancel"
              className="text-wash-yellow-ink"
              onClick={() => setAgentSwitchNeeded(false)}
            >
              {t("voice.switchKeep")}
            </Button>
          </div>
        </div>
      )}
      {side && !live && !agentSwitchNeeded && !elsewhere && (
        <div className="mt-4 flex flex-col items-center gap-3 px-2 text-center">
          <Orb kind={starting ? "think" : "idle"} size={44} />
          <p className="font-serif text-[17px] font-medium leading-snug text-ink-100">
            {bookId == null ? t("voice.offLeadStandalone") : t("voice.offLead")}
          </p>
          <Button
            variant="solid"
            size="lg"
            block
            data-testid="voice-session-start"
            busy={starting}
            disabled={keyPresence !== "present"}
            onClick={start}
          >
            {starting ? t("voice.starting") : t("voice.startSession")}
          </Button>
        </div>
      )}
      {live && (
        <div className="mt-3 flex flex-col gap-1.5">
          <SegmentedSwitch
            testId="voice-session-mode"
            dataMode={talking ? "voice" : "text"}
            label={t("voice.mode.switch")}
            describedBy={modeHintId}
            value={talking ? "voice" : "text"}
            onChange={(next) =>
              next === "voice" ? enterInteractive() : backToTyping()
            }
            options={[
              {
                value: "text",
                label: t("voice.mode.type"),
                icon: <KeyboardIcon />,
                testId: "voice-session-back-to-typing",
              },
              {
                value: "voice",
                label: t("voice.mode.talk"),
                icon: <MicIcon />,
                disabled: !talkAvailable,
                hot: talking && listening,
                testId: "voice-session-interactive",
              },
            ]}
          />
          <p
            id={modeHintId}
            className="px-2.5 text-label leading-snug text-ink-500"
          >
            {modeHint}
          </p>
        </div>
      )}
      {!live && KEY_PRESENCE_HINT_KEYS[keyPresence] != null && (
        <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1.5">
          <p
            data-testid="voice-session-key-hint"
            className="min-w-0 text-label text-ink-500"
          >
            {t(KEY_PRESENCE_HINT_KEYS[keyPresence]!, {
              provider: providerDisplayName("gemini"),
            })}
          </p>
          {keyPresence !== "unknown" && (
            <button
              type="button"
              data-testid="voice-session-key-action"
              onClick={() => goToProvider("gemini")}
              className="shrink-0 whitespace-nowrap rounded-lg border border-ink-700 bg-ink-800/70 px-2.5 py-1 text-secondary font-medium text-ink-100 transition-colors duration-200 hover:bg-ink-800 active:scale-[0.98]"
            >
              {t(
                keyPresence === "locked"
                  ? "voice.hint.unlockGemini"
                  : "voice.hint.openGemini",
                { provider: providerDisplayName("gemini") },
              )}
            </button>
          )}
          <button
            type="button"
            data-testid="voice-session-key-retry"
            onClick={() => setKeyRevision((value) => value + 1)}
            className="rounded-lg px-2 py-1 text-secondary text-ink-300 underline"
          >
            {t("voice.hint.checkAgain")}
          </button>
        </div>
      )}
      {!live &&
        !starting &&
        bookId != null &&
        jobId == null &&
        keyPresence === "present" && (
          <p
            data-testid="voice-session-job-hint"
            className="mt-1 text-label text-ink-500"
          >
            {t("voice.noNarrationHint")}
          </p>
        )}
      {!live && !starting && bookId == null && keyPresence === "present" && (
        <p
          data-testid="voice-session-standalone-hint"
          className="mt-1 text-label text-ink-500"
        >
          {t("voice.standaloneHint")}
        </p>
      )}
      {notice != null && (
        <p
          role="status"
          data-testid="voice-session-notice"
          className="mt-1.5 text-secondary text-reading-stale"
        >
          {notice}
        </p>
      )}
      {open && (
        <div
          className={`mt-2 flex flex-col gap-2 ${side ? "min-h-0 flex-1" : ""}`}
        >
          {session.transcript.length > 0 ||
          (earlier != null && earlier.turns.length > 0) ||
          activityKey != null ? (
            <div
              ref={listRef}
              onScroll={(event) => {
                const list = event.currentTarget;
                pinnedRef.current =
                  list.scrollHeight - list.scrollTop - list.clientHeight < 48;
              }}
              className={`overflow-y-auto ${side ? "min-h-0 flex-1" : "max-h-40"}`}
            >
              <div className="flex flex-col gap-1.5">
                {earlier != null && earlier.turns.length > 0 && (
                  <>
                    <ul
                      data-testid="voice-session-earlier"
                      className="flex flex-col gap-3 opacity-70"
                    >
                      {earlier.turns.map((entry, at) => (
                        <li key={at} className={rowClass(entry.role)}>
                          <span className="sr-only">
                            {entry.role === "user"
                              ? t("tutor.you")
                              : t("tutor.tutor")}
                            :{" "}
                          </span>
                          {entry.role === "user" &&
                            entry.voiceprint != null && (
                              <Voiceprint
                                bars={entry.voiceprint}
                                label={t("voice.spoken")}
                              />
                            )}
                          <TutorRichText text={entry.text} />
                        </li>
                      ))}
                    </ul>
                    <p
                      data-testid="voice-session-earlier-divider"
                      className="flex items-center gap-2 text-label text-ink-500"
                    >
                      <span aria-hidden className="h-px flex-1 bg-ink-800" />
                      {session.transcript.length > 0 || !live
                        ? t("voice.earlier.divider")
                        : t("voice.earlier.resumed")}
                      <span aria-hidden className="h-px flex-1 bg-ink-800" />
                    </p>
                  </>
                )}
                <TutorActivityAnnouncer
                  activity={session.activity}
                  live={live}
                />
                <ul
                  data-testid="voice-session-transcript"
                  className="flex flex-col gap-3"
                >
                  {session.transcript.map((entry, at) => (
                    <li key={at} className={rowClass(entry.role)}>
                      <span className="sr-only">
                        {entry.role === "user"
                          ? t("tutor.you")
                          : t("tutor.tutor")}
                        :{" "}
                      </span>
                      {entry.role === "assistant" && entry.steps != null && (
                        <div className="mb-1">
                          <TutorSteps steps={entry.steps} />
                        </div>
                      )}
                      {entry.role === "user" && entry.typed !== true && (
                        // Provider input transcription is approximate by nature
                        // (F4, owner session 2026-08-29) — a voice bubble is what
                        // the tutor HEARD, never a quote of what was said. The
                        // marker says so instead of presenting the guess as fact.
                        // OWNER-TUNABLE presentation: glyph + tooltip copy below.
                        <span
                          className="mb-1 flex items-center gap-1 text-label text-ink-500"
                          title={t("voice.approxHint")}
                          data-testid="voice-transcript-approx"
                        >
                          {entry.voiceprint != null ? (
                            <Voiceprint
                              bars={entry.voiceprint}
                              label={t("voice.spoken")}
                            />
                          ) : (
                            <>
                              <MicIcon className="h-3.5 w-3.5" />
                              {t("voice.spoken")}
                            </>
                          )}
                        </span>
                      )}
                      <div className="flex items-start gap-1">
                        <TutorRichText
                          text={entry.text}
                          animate={
                            live &&
                            entry.role === "assistant" &&
                            at === transcriptLength - 1
                          }
                          streaming={
                            streamingLast && at === transcriptLength - 1
                          }
                        />
                      </div>
                      {entry.role === "assistant" && bookId != null && (
                        <PagePeeks
                          bookId={bookId}
                          jobId={jobId}
                          pages={pagesReadIn(entry.steps)}
                        />
                      )}
                    </li>
                  ))}
                  {activity != null && (
                    <TutorActivityStrip
                      activity={session.activity}
                      message={activity}
                      seconds={activitySeconds}
                    />
                  )}
                </ul>
              </div>
            </div>
          ) : (
            live && (
              <p
                data-testid="voice-session-transcript-empty"
                className="rounded-lg border border-dashed border-ink-800 px-3 py-2 text-label text-ink-500"
              >
                {t("voice.transcriptEmpty")}
              </p>
            )
          )}
          {continueBlocked && (
            <p
              role="status"
              data-testid="voice-session-continue-blocked"
              className="text-secondary text-ink-400"
            >
              {F11_STRINGS.liveContinue}
            </p>
          )}
          {historyShown && (
            <SessionHistory
              bookId={bookId}
              jobId={jobId}
              onContinue={continueFrom}
              onOpenTarget={onOpenTarget}
              continueDisabled={
                keyPresence !== "present" || sessionIsActive(snapshot)
              }
            />
          )}
        </div>
      )}
      {live && (
        <div className="mt-2 flex flex-col gap-2">
          {nowLine != null && (
            <div
              role="status"
              data-testid="voice-session-now"
              data-now={nowLine.kind}
              className="flex min-h-10 items-center gap-2.5 ps-1.5 pe-0.5"
            >
              <Orb kind={nowLine.kind} />
              <span className="flex min-w-0 flex-1 flex-col leading-snug">
                <span
                  className={`text-secondary font-medium ${nowLine.kind === "warn" ? "text-wash-yellow-ink" : "text-ink-200"}`}
                >
                  {nowLine.title}
                </span>
                <span className="line-clamp-2 text-label text-ink-500">
                  {nowLine.sub}
                </span>
              </span>
              {speaking && (
                <Button
                  variant="soft"
                  size="sm"
                  data-testid="voice-session-interrupt"
                  onClick={interrupt}
                  leading={<StopIcon />}
                >
                  {t("voice.interrupt")}
                </Button>
              )}
            </div>
          )}
          <form
            className="ui-capsule"
            onSubmit={(event) => {
              event.preventDefault();
              sendDraft();
            }}
          >
            {interactive && (
              <MicToggle
                testId="voice-session-mute"
                muted={muted}
                disabled={session.status === "renewing"}
                label={t("voice.mute")}
                onToggle={muted ? unmute : mute}
              />
            )}
            <input
              type="text"
              dir="auto"
              data-testid="voice-session-input"
              aria-label={t("voice.composerPlaceholder")}
              value={draft}
              placeholder={
                talking
                  ? t("voice.composerTalkPlaceholder")
                  : t("voice.composerPlaceholder")
              }
              onChange={(event) => setDraft(event.target.value)}
            />
            <Button
              type="submit"
              variant="solid"
              data-testid="voice-session-send"
              disabled={draft.trim() === ""}
              trailing={<SendIcon />}
            >
              {t("voice.send")}
            </Button>
          </form>
        </div>
      )}
      {open && (
        <div className="mt-2 flex flex-col gap-2 pb-[env(safe-area-inset-bottom)]">
          <div className="flex items-center justify-between gap-3">
            <button
              type="button"
              data-testid="voice-session-details-toggle"
              aria-expanded={detailsOpen}
              aria-controls="voice-session-details"
              onClick={() => setDetailsOpen((current) => !current)}
              className="inline-flex items-center gap-1 text-label text-ink-500 transition-colors duration-200 hover:text-ink-300"
            >
              {t("voice.howThisWorks")}
              <span
                className={`inline-flex transition-transform duration-300 ease-[cubic-bezier(0.32,0.72,0,1)] ${detailsOpen ? "rotate-90 rtl:-rotate-90" : ""}`}
              >
                <ChevronIcon className="h-3.5 w-3.5" />
              </span>
            </button>
          </div>
          {detailsOpen && (
            <div id="voice-session-details" className="flex flex-col gap-2">
              <p className="text-label leading-snug text-ink-500">
                {interactive
                  ? muted
                    ? t("voice.how.muted")
                    : t("voice.how.interactive")
                  : session.audio === "active" || session.audio === "speaking"
                    ? t("voice.how.audio")
                    : t("voice.how.text")}
              </p>

              <p
                data-testid="voice-recovery-limit"
                className="text-label leading-snug text-ink-500"
              >
                {t("voice.recovery.limit")}
              </p>
              <div className="flex justify-end">
                <button
                  type="button"
                  data-testid="voice-session-copy-log"
                  onClick={copyLog}
                  className="text-label text-ink-500 transition-colors duration-200 hover:text-ink-300"
                >
                  {logCopied ? t("voice.copied") : t("voice.copyLog")}
                </button>
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
