// Source gates over VoiceSessionPanel.tsx (VA6 output plane, G5). The house
// rule keeps DOM behavior in e2e and unit tests in node — so what a render
// test would pin elsewhere is pinned here as source facts: the affordances
// the audio plane earned (speaking indicator, Interrupt) exist, are gated on
// the model's word, and dispatch onto commands the Rust side actually
// registers. Every gate names the regression it exists to catch.

import { describe, expect, it } from "vitest";

import { en } from "./i18n/catalog";
import { readSource } from "./tests/readSource";

const PANEL = readSource(new URL("./VoiceSessionPanel.tsx", import.meta.url));

const STORE = readSource(new URL("./voiceSessionStore.ts", import.meta.url));

const HALF_DUPLEX = readSource(
  new URL("./voiceHalfDuplex.ts", import.meta.url),
);

const MIC_TOGGLE = readSource(new URL("./ui/MicToggle.tsx", import.meta.url));
const BUTTON = readSource(new URL("./ui/Button.tsx", import.meta.url));
const SWITCH = readSource(new URL("./ui/SegmentedSwitch.tsx", import.meta.url));
const CSS = readSource(new URL("./index.css", import.meta.url));

const LIB_RS = readSource(new URL("../src-tauri/src/lib.rs", import.meta.url));

const TUTOR_PANEL = readSource(new URL("./TutorPanel.tsx", import.meta.url));
const SHELL_BAR = readSource(new URL("./ShellRail.tsx", import.meta.url));
const APP_SHELL = readSource(new URL("./App.tsx", import.meta.url));

const JOB_MODAL = readSource(new URL("./JobModal.tsx", import.meta.url));

function generateHandlerBlock(): string {
  const start = LIB_RS.indexOf("generate_handler![");
  expect(start, "tauri command registry not found in lib.rs").toBeGreaterThan(
    -1,
  );
  // The block ends at "])"; a plain "]" stops at the first #[cfg(...)]
  // attribute inside the list (the book-agent commands are feature-gated).
  return LIB_RS.slice(start, LIB_RS.indexOf("])", start));
}

describe("every command the panel invokes exists on the Rust side", () => {
  it("finds each invoke()'d name in the tauri command registry", () => {
    const invoked = [
      ...new Set(
        [
          ...`${PANEL}${HALF_DUPLEX}`.matchAll(
            /invoke(?:<[^>]*>)?\(\s*"([a-z_]+)"/g,
          ),
        ].map((match) => match[1]),
      ),
    ].sort();
    expect(
      invoked,
      "the panel's command surface changed — re-pin it deliberately",
    ).toEqual([
      // B-4 Agent-aware entry (charter §2.2 both directions): the panel
      // probes which mode is live and, over a live Book agent, runs the
      // explicit switch ceremony (agent_session_stop) before Tutor start.
      "agent_session_stop",
      "voice_session_enter_interactive",
      "voice_session_interrupt",
      // Field session 2026-10-09: Back to typing returns a voice session to
      // the writing tutor.
      "voice_session_leave_interactive",
      "voice_session_mute",
      "voice_session_send_text",
      "voice_session_start",
      "voice_session_stop",
      // Continue shows the conversation it picks up (field 2026-10-09).
      "voice_session_transcript",
      "voice_session_unmute",
      "voice_tutor_readiness",
    ]);
    const registry = generateHandlerBlock();
    for (const name of invoked) {
      expect(
        registry,
        `invoke("${name}") dispatches into a void: not in generate_handler!`,
      ).toContain(name);
    }
  });
});

describe("the speaking indicator and Interrupt ride the model, not guesses", () => {
  it("derives `speaking` from the reducer's audio state", () => {
    expect(PANEL).toContain('session.audio === "speaking"');
  });

  it("gates the Interrupt button on speaking and wires it to the command", () => {
    // The button may only exist while the device is actually playing —
    // an always-visible Interrupt cancels responses that are not speaking.
    const guard = PANEL.match(
      /\{(\w+) && \(\s*<Button\s+variant="soft"\s+size="sm"\s+data-testid="voice-session-interrupt"/,
    );
    expect(guard, "the Interrupt button lost its conditional").not.toBeNull();
    expect((guard as RegExpMatchArray)[1]).toBe("speaking");
    expect(PANEL).toMatch(
      /data-testid="voice-session-interrupt"[^]{0,100}onClick=\{interrupt\}/,
    );
    expect(PANEL).toMatch(
      /const interrupt = useCallback[^]{0,100}invoke\("voice_session_interrupt"\)/,
    );
  });

  it("draws one now-line from the model state, reconnecting before speaking before the mic", () => {
    expect(PANEL).toContain(
      "const nowLine = nowLineOf(session.status, speaking, talking, session.mic, t);",
    );
    const order = PANEL.slice(PANEL.indexOf("function nowLineOf("));
    const renewing = order.indexOf('status === "renewing"');
    const speakingAt = order.indexOf("if (speaking)");
    const talkingGate = order.indexOf("if (!talking) return null;");
    expect(renewing).toBeGreaterThan(-1);
    expect(speakingAt).toBeGreaterThan(renewing);
    expect(talkingGate).toBeGreaterThan(speakingAt);
    expect(PANEL).toMatch(
      /data-testid="voice-session-now"[^]{0,40}role="status"|role="status"[^]{0,80}data-testid="voice-session-now"/,
    );
    for (const literal of ['"Audio on"', '"Speaking…"', '"Text only"']) {
      expect(PANEL).not.toContain(literal);
    }
  });
});

describe("the mic affordances ride the model (R2 Interactive mode)", () => {
  it("names mic states through the catalog, never as panel literals", () => {
    for (const key of [
      "voice.now.listening",
      "voice.now.muted",
      "voice.now.noMic",
    ]) {
      expect(PANEL).toContain(`t("${key}")`);
    }
    for (const literal of ['"Listening"', '"Muted"', '"No mic"']) {
      expect(PANEL).not.toContain(literal);
    }
  });

  it("offers Talk only on a live session with a usable mic, wired to its command", () => {
    expect(PANEL).toContain(
      'const micMissing = session.mic === "unavailable" || session.mic === "lost";',
    );
    expect(PANEL).toMatch(
      /const talkAvailable =\s*talking \|\| \(!micMissing && session\.activity\.state !== "connecting"\);/,
    );
    expect(PANEL).toMatch(
      /\{live && \([^]{0,120}<SegmentedSwitch\s+testId="voice-session-mode"/,
    );
    expect(PANEL).toMatch(
      /value: "voice",[^]{0,160}disabled: !talkAvailable,[^]{0,80}testId: "voice-session-interactive"/,
    );
    expect(PANEL).toMatch(
      /onChange=\{\(next\) =>\s*next === "voice" \? enterInteractive\(\) : backToTyping\(\)\s*\}/,
    );
    expect(PANEL).toMatch(
      /const enterInteractive = useCallback[^]{0,120}invoke\("voice_session_enter_interactive"\)/,
    );
  });

  it("gates Mute/Unmute on Interactive mode and wires each to its own command", () => {
    const guard = PANEL.match(
      /\{(\w+) && \(\s*<MicToggle\s+testId="voice-session-mute"/,
    );
    expect(guard, "the Mute button lost its guard").not.toBeNull();
    expect((guard as RegExpMatchArray)[1]).toBe("interactive");
    expect(PANEL).toContain(
      'const interactive = session.mic === "listening" || session.mic === "muted"',
    );
    // The toggle dispatches the state-appropriate command; the literal
    // invokes live in voiceHalfDuplex.ts (userMute/userUnmute), which the
    // registry gate above also scans.
    expect(PANEL).toMatch(
      /testId="voice-session-mute"[^]{0,200}onToggle=\{muted \? unmute : mute\}/,
    );
    expect(MIC_TOGGLE).toContain("aria-pressed={muted}");
    expect(PANEL).toMatch(/const mute = useCallback[^]{0,100}userMute\(\)/);
    expect(PANEL).toMatch(/const unmute = useCallback[^]{0,100}userUnmute\(\)/);
    expect(HALF_DUPLEX).toMatch(
      /export function userMute[^]{0,900}invoke<void>\("voice_session_mute", \{ reason: "user" \}\)/,
    );
    expect(HALF_DUPLEX).toMatch(
      /export function userUnmute[^]{0,900}invoke<void>\("voice_session_unmute", \{ reason: "user" \}\)/,
    );
    expect(PANEL).toContain('const muted = session.mic === "muted"');
  });

  it("breathes only while LISTENING: a muted mic reads as at-rest", () => {
    expect(PANEL).toContain('const listening = session.mic === "listening"');
    const body = PANEL.slice(
      PANEL.indexOf("function nowLineOf("),
      PANEL.indexOf("export default function VoiceSessionPanel"),
    );
    expect(body).toMatch(
      /if \(mic === "muted"\) \{\s*return \{\s*kind: "rest"/,
    );
    expect(body).toMatch(
      /if \(mic === "listening"\) \{\s*return \{\s*kind: "listen"/,
    );
    expect(body.indexOf('mic === "muted"')).toBeLessThan(
      body.indexOf('mic === "listening"'),
    );
    expect(PANEL).toContain("hot: talking && listening,");
    expect(CSS).toMatch(
      /\.ui-orb\[data-kind="rest"\] > \.ui-orb-aura \{\s*display: none;/,
    );
  });
});

describe("failures render instead of vanishing", () => {
  it("shows the one honest notice line under role=status", () => {
    expect(PANEL).toContain(
      "const notice = startError ?? noticeText(session);",
    );
    expect(PANEL).toMatch(
      /\{notice != null && \([^]{0,200}role="status"[^]{0,200}data-testid="voice-session-notice"/,
    );
  });

  it("the store dispatches only payloads the defensive parse accepted", () => {
    expect(STORE).toMatch(
      /voiceSessionEnvelopeFrom\(payload\)[^]{0,200}if \(envelope == null\) return/,
    );
    expect(STORE).toMatch(/reduceVoiceSession\(session, parsed\)/);
  });
});

describe("a running session is never a dead end (owner UX report)", () => {
  it("holds session state in the module store, not a per-mount reducer", () => {
    // The reported dead end: the panel remounted, forgot the live session,
    // rendered Start, and the runtime's refusal blocked every control.
    expect(PANEL).toMatch(
      /useSyncExternalStore\(\s*subscribeVoice,\s*getActiveVoiceSnapshot,?(?:\s*getActiveVoiceSnapshot,?)?\s*\)/,
    );
    expect(PANEL).not.toContain("useReducer");
  });

  it("adopts the runtime's AlreadyRunning refusal as the live session", () => {
    // …unless the live session is the BOOK AGENT: adoption would present it
    // as the Tutor (mode misattribution). The probe decides; agent-live runs
    // the explicit switch ceremony instead of adopting.
    expect(PANEL).toMatch(
      /if \(isAlreadyRunningStartError\(message\)\) \{[^]{0,900}synchronizeVoiceSession\(\);[^]{0,850}identity.bookId !== bookId \|\| identity.jobId !== jobId[^]{0,250}returnToTutor\(\);/,
    );
    expect(PANEL).toMatch(/identity\.purpose === "agent"/);
    expect(PANEL).toMatch(/setAgentSwitchNeeded\(true\);\s*return;/);
  });

  it("opens the session surface whenever the session is live", () => {
    expect(PANEL).toMatch(/if \(live\) setOpen\(true\);/);
  });

  it("keeps an end-session control whenever the session is live", () => {
    expect(PANEL).toMatch(
      /\{live \? \(\s*<Button\s+variant="danger"\s+data-testid="voice-session-stop"/,
    );
    expect(PANEL).toMatch(
      /data-testid="voice-session-stop"[^]{0,100}onClick=\{stop\}/,
    );
  });

  it("a Start press always answers: starting, adopted, or a named error", () => {
    // The banked F-C3 silent dead Start, re-pinned for F6: a null jobId no
    // longer refuses (standalone start is the feature), so the guarantee is
    // now structural — start() enters markStarting immediately, with no
    // early return that could die silently.
    expect(PANEL).toMatch(
      /const start = useCallback\(\(\) => \{[^]{0,400}clearEarlierConversation\(\);\s*markStarting\(\{ bookId, jobId \}\);/,
    );
    expect(PANEL).toContain('data-testid="voice-session-job-hint"');
    expect(PANEL).toContain('data-testid="voice-session-standalone-hint"');
  });
});

describe("the key gate names the actual blocker, not one blanket remedy", () => {
  // The owner-reported trap: a locked vault (or a dead IPC) rendered as
  // "Add a Google Gemini key…" — the one remedy that cannot fix either.
  it("uses native pool readiness and never selects a key slot in the panel", () => {
    expect(PANEL).toContain('invoke<TutorReadiness>("voice_tutor_readiness")');
    expect(PANEL).not.toContain("entries.find(");
    expect(PANEL).toContain('goToProvider("gemini")');
  });

  it("keeps three distinct remedies and an error state that is not 'missing'", () => {
    // I18N-2: the remedies are catalog keys (outcome-first, no vendor name).
    expect(PANEL).toContain('missing: "voice.hint.missing"');
    expect(PANEL).toContain('locked: "voice.hint.locked"');
    expect(PANEL).toContain('error: "voice.hint.error"');
    // An invoke failure must not masquerade as a missing key.
    expect(PANEL).toMatch(
      /\.catch\(\(\) => \{\s*if \(request === latest\) setKeyPresence\("error"\)/,
    );
  });

  it("start stays gated on presence alone — new states never enable it", () => {
    // F6 re-pin: the job gate is gone (standalone start); the key gate — the
    // one legitimate gate — remains.
    const gates = PANEL.match(
      /data-testid="voice-session-start"\s+busy=\{starting\}\s+disabled=\{keyPresence !== "present"\}/g,
    );
    expect(gates?.length).toBe(2);
    expect(BUTTON).toContain("disabled={disabled === true || busy}");
  });

  it("renders the hint for every non-startable state under one testid", () => {
    // Gated on !live: while a session runs, Start is absent, so a key
    // remedy would be noise over the session surface.
    expect(PANEL).toMatch(
      /\{!live && KEY_PRESENCE_HINT_KEYS\[keyPresence\] != null && \([^]{0,200}data-testid="voice-session-key-hint"/,
    );
  });
});

describe("F6 P4: entry gating removed — voice is reachable with no book", () => {
  it("the panel accepts a null bookId and never refuses on a missing job", () => {
    expect(PANEL).toMatch(/bookId: string \| null;/);
    expect(
      PANEL.includes("no narration job yet"),
      "the §0 refusal string is the literal defect — it must not return",
    ).toBe(false);
    expect(
      PANEL.includes('disabled={keyPresence !== "present"}'),
      "Start gates on the key only — never on a job",
    ).toBe(true);
  });

  it("the Rust start command takes optional ids (standalone start exists)", () => {
    const command = LIB_RS.slice(
      LIB_RS.indexOf("async fn voice_session_start"),
      LIB_RS.indexOf(
        "-> Result",
        LIB_RS.indexOf("async fn voice_session_start"),
      ),
    );
    expect(command).toContain("book_id: Option<String>");
    expect(command).toContain("job_id: Option<String>");
  });

  it("the shell carries the standalone tutor entry in every state, including an empty shelf", () => {
    const mounts = [
      ...TUTOR_PANEL.matchAll(
        /<VoiceSessionPanel bookId=\{null\} jobId=\{null\}/g,
      ),
    ];
    expect(mounts.length, "one standalone mount, in the tutor panel").toBe(1);
    expect(
      APP_SHELL,
      "the shell carries the tutor launcher outside any library or view branch (StudyView §0 gate #1)",
    ).toContain(
      [
        `            {overlay === "none" && (`,
        `              <TutorLauncher`,
      ].join("\n"),
    );
    expect(SHELL_BAR.includes('testId="nav-tutor"')).toBe(false);
  });

  it("the cast-dialog tutor card no longer loops back through the Library", () => {
    expect(JOB_MODAL.includes("Open Study to talk with your book")).toBe(false);
    expect(
      JOB_MODAL.includes('t("cast.talkHint")') &&
        en["cast.talkHint"].includes("No cast needed"),
      "the card decouples talking from casting",
    ).toBe(true);
  });
});

describe("F11-U3 — Continue is the start path with a source row", () => {
  it("the Rust start command takes resume_from", () => {
    const command = LIB_RS.slice(
      LIB_RS.indexOf("async fn voice_session_start"),
      LIB_RS.indexOf(
        "-> Result",
        LIB_RS.indexOf("async fn voice_session_start"),
      ),
    );
    expect(command).toContain("resume_from: Option<String>");
  });

  it("Continue while a session is live adopts it and says so (B5)", () => {
    // The refusal still adopts the live session (no second dial), and the
    // history's Continue press is answered with the §4.6 line — not silence.
    expect(PANEL).toMatch(
      /if \(isAlreadyRunningStartError\(message\) && resumeFrom != null\) \{\s*setContinueBlocked\(true\);\s*\}\s*if \(isAlreadyRunningStartError\(message\)\) \{[^]{0,900}synchronizeVoiceSession\(\);[^]{0,850}identity.bookId !== bookId \|\| identity.jobId !== jobId[^]{0,250}returnToTutor\(\);/,
    );
    expect(PANEL).toContain("{F11_STRINGS.liveContinue}");
    expect(PANEL).toMatch(/continueBlocked && \(/);
  });
});

// Field session 2026-10-09: Arabic answers with English terms rendered left
// to right (first-strong-character direction), the composer typed Arabic
// left-aligned, and a minute of waiting showed nothing.
describe("tutor text renders by block direction and the wait is visible", () => {
  it("renders every chat line through TutorRichText, never as one bdi run", () => {
    expect(PANEL).not.toContain("<bdi>{entry.text}</bdi>");
    expect(
      PANEL.match(/<TutorRichText\b/g)?.length ?? 0,
    ).toBeGreaterThanOrEqual(2);
  });

  it("types in the direction of what is typed", () => {
    expect(PANEL).toMatch(/dir="auto"\s+data-testid="voice-session-input"/);
  });

  it("mounts one persistent announcer and draws the strip from the model's message", () => {
    // Behaviour lives in TutorActivity.test.ts; this pins the wiring.
    expect(PANEL).toContain("<TutorActivityAnnouncer");
    expect(PANEL).toContain("activityMessage(session.activity)");
  });

  it("offers Back to typing only on the voice tutor, wired to its command", () => {
    expect(PANEL).toMatch(
      /value: "text",[^]{0,120}testId: "voice-session-back-to-typing"/,
    );
    expect(PANEL).toMatch(
      /const backToTyping = useCallback[^]{0,120}invoke\("voice_session_leave_interactive"\)/,
    );
    expect(SWITCH).toContain("if (!checked) onChange(option.value);");
  });
});
