// Structure pins for the reading-session surface (round-2 slice, H17 +
// its side-panel addendum, v3 redesign). The chapter outline flanks the
// reading surface at inline-start and the tutor companion at inline-end:
// the outline is the row's first flex child and the companion its last, so
// the document's stable UI direction (LTR → tutor right, RTL → tutor left)
// places them — never per-turn tutor language, never an overlay.

import { describe, expect, it } from "vitest";
import { readSource } from "./tests/readSource";

const VIEW = readSource(new URL("./ReadingView.tsx", import.meta.url));
const PANEL = readSource(new URL("./VoiceSessionPanel.tsx", import.meta.url));
const HEADER = readSource(new URL("./ReadingHeader.tsx", import.meta.url));
const STRIP = readSource(new URL("./MiniPlayer.tsx", import.meta.url));
const APP = readSource(new URL("./App.tsx", import.meta.url));

describe("the study zones flank the reading surface: outline at inline-start, tutor at inline-end", () => {
  it("renders the outline, then the center column, then the companion-wrapped tutor", () => {
    const row = VIEW.indexOf(
      '"relative flex h-full w-full bg-ink-950 outline-none"',
    );
    const outline = VIEW.indexOf("<ReadingOutline");
    const companion = VIEW.indexOf("<StudyCompanion");
    const header = VIEW.indexOf("<ReadingHeader");
    expect(row, "the full-bleed dialog lost its row layout").toBeGreaterThan(
      -1,
    );
    expect(outline, "the chapter outline left the session").toBeGreaterThan(
      row,
    );
    expect(
      header,
      "the outline must precede the center column (inline-start flank)",
    ).toBeGreaterThan(outline);
    expect(
      companion,
      "the companion zone must follow the center column (inline-end flank)",
    ).toBeGreaterThan(header);
    expect(
      VIEW.indexOf("<VoiceSessionPanel", companion),
      "the tutor panel must live inside the companion zone",
    ).toBeGreaterThan(companion);
  });

  it("guards the study zones on fullBleed and keeps the strip everywhere else", () => {
    expect(VIEW).toMatch(
      /\{fullBleed && wideShell && !focusMode && \(\s*<StudyCompanion/,
    );
    expect(VIEW).toMatch(
      /\{fullBleed && wideShell && !focusMode && \(\s*<ReadingOutline/,
    );
    expect(VIEW).toMatch(
      /\{fullBleed && !wideShell && !focusMode && \(\s*<VoiceSessionPanel\s/,
    );
    expect(VIEW).toMatch(/\{!fullBleed && \(\s*<VoiceSessionPanel /);
  });

  it("gives the outline its own toggle and the pinned [ shortcut", () => {
    expect(VIEW).toContain('data-testid="outline-toggle"');
    expect(VIEW).toContain('if (event.key === "[")');
    expect(VIEW).toMatch(
      /tag === "INPUT" \|\| tag === "TEXTAREA" \|\| target\?\.isContentEditable/,
    );
  });

  it("lifts the companion tab above the Focus mount boundary", () => {
    expect(VIEW).toContain(
      'initialCompanionTab ?? checkpoint?.companionTab ?? "tutor"',
    );
    expect(VIEW).toMatch(
      /tab=\{companionTab\}\s*onSelectTab=\{setCompanionTab\}/,
    );
    const companion = readSource(
      new URL("./StudyCompanion.tsx", import.meta.url),
    );
    expect(
      companion,
      "companion state must live above the mount so Focus exit restores it",
    ).not.toContain("useState");
  });

  it("keeps focus mode and the [ shortcut from fighting", () => {
    const handler = VIEW.slice(
      VIEW.indexOf('if (event.key === "[")'),
      VIEW.indexOf("window.addEventListener"),
    );
    expect(
      handler,
      "[ during focus must exit focus and reveal the outline",
    ).toMatch(
      /if \(focusModeRef\.current\) \{\s*setFocusMode\(false\);\s*setOutlineOpen\(true\);/,
    );
    expect(VIEW).toContain('data-testid="focus-toggle"');
    expect(VIEW).toContain('data-testid="focus-edge-handle"');
  });

  it("places by document direction with logical properties, not per-turn language", () => {
    const sideClass = PANEL.match(/side\s*\?\s*"([^"]+)"/);
    expect(sideClass, "the side variant lost its class branch").not.toBeNull();
    const classes = (sideClass as RegExpMatchArray)[1];
    expect(classes).toContain("border-s");
    expect(classes).toContain("shrink-0");
    for (const banned of ["absolute", "fixed", "border-l ", "border-r "]) {
      expect(
        classes,
        `side panel must displace via flex flow, found "${banned}"`,
      ).not.toContain(banned);
    }
    expect(PANEL).not.toContain("session.language");
    // Field session 2026-10-09: CONTENT takes its own direction — the
    // composer types in the direction of what is typed (dir="auto") and
    // TutorRichText sets it per block — but the panel's placement never
    // takes a per-turn dir.
    expect(
      PANEL.replace(/dir="auto"\s+data-testid="voice-session-input"/, ""),
    ).not.toContain("dir=");
  });

  it("adds no text-highlight vocabulary to the tutor surface (J19)", () => {
    expect(PANEL).not.toContain("reading-highlight");
  });
});

describe("the live tutor suppresses production highlights (J19, regression-class)", () => {
  it("consults the real voice session store, not a prop", () => {
    expect(VIEW).toContain("getActiveVoiceSnapshot");
    expect(VIEW).toContain("matchesVoiceScope(item.bookId, itemJobId)");
    expect(VIEW).toMatch(
      /voiceSession\.status === "live" \|\| voiceSession\.status === "renewing"/,
    );
  });

  it("pauses playback and clears both appliers when the tutor goes live", () => {
    expect(VIEW).toMatch(
      /if \(tutorSessionLive\) \{[^]{0,80}dispatchPlayback\(\{ type: "pause" \}\);/,
    );
    expect(VIEW).toMatch(
      /applier\.apply\(\s*\{ container: scriptRef\.current[^]{0,120}null,\s*0,\s*\)/,
    );
    expect(VIEW).toMatch(/overlayApplier\.apply\([^]{0,160}NO_OVERLAY,\s*\)/);
  });

  it("gates every cursor-driven highlight on the live tutor", () => {
    expect(VIEW).toContain(
      "stale || tutorLiveRef.current ? null : cursor.heldWordIdx",
    );
    expect(VIEW).toMatch(
      /inputs\.mode === "page"\s*&&\s*!tutorLiveRef\.current/,
    );
    expect(VIEW).toContain('mode === "page" && !tutorSessionLive');
  });

  it("applies script-mode word highlighting without suppression in teacher mode", () => {
    expect(VIEW).toMatch(
      /applier\.apply\(\s*\{\s*container:\s*scriptRef\.current,\s*underline:\s*underlineRef\.current\s*\},\s*scriptWordIdx,\s*cursor\.fill,\s*\)/,
    );
  });

  it("keeps the H17 entry alive inside Focus via the transport", () => {
    expect(VIEW).toMatch(
      /\{focusMode && \(\s*<button\s+type="button"\s+data-testid="ask-tutor"/,
    );
  });
});

describe("the reading mode selector is first-class and two-way", () => {
  it("offers Script and Page as peer segments", () => {
    expect(HEADER).toMatch(
      /data-testid="reading-mode-script"[^]{0,200}onClick=\{\(\) => onSelectMode\("script"\)\}/,
    );
    expect(HEADER).toMatch(
      /data-testid="reading-mode-toggle"[^]{0,120}aria-label=\{t\("reader.page"\)\}/,
    );
    expect(HEADER).toContain('aria-pressed={mode === "script"}');
    expect(HEADER).toContain('aria-pressed={mode === "page"}');
  });

  it("selecting a mode routes through the follow-model transition", () => {
    expect(VIEW).toMatch(
      /const selectMode = useCallback\(\s*\(next: ReadingMode\) => \{\s*if \(direct\) return; \/\/[^\n]*\n\s*if \(next === mode\) return;\s*dispatchFollow\(\{ type: "readingModeChanged" \}\);\s*storeReadingMode\(next\);\s*setMode\(next\);/,
    );
  });
});

describe("the tutor opens in context through the Book the strip is playing (H17)", () => {
  it("offers the way into the Book only when a playable context exists", () => {
    expect(STRIP).toMatch(
      /\{followAlong && \([^]{0,300}data-testid="mini-player-expand"/,
    );
    expect(STRIP).toMatch(
      /\{followAlong && \([^]{0,300}data-testid="mini-player-follow-along"/,
    );
  });

  it("App gates the entry on the playable-context rule", () => {
    expect(APP).toMatch(
      /onFollowAlong=\{\s*canFollowAlong\(playbackItem\)\s*\?\s*\(\) => applyOverlay\(\{ type: "openReading" \}\)\s*:\s*null\s*\}/,
    );
  });
});
