// D-QA4 (CRITICAL) — Talk auto-play.
//
// INVARIANT: entering Talk on a book mounts a live tutor session and NOTHING
// else — no play command fires at entry. Playback starts only on an explicit
// user play, or a user-REQUESTED tutor navigation.
//   Spec citations:
//   - F7 §3 ("Voice is a *live session*", entry model E1-E4) and F7 §8.1:
//     Voice is a peer ACTION, never a render/playback trigger.
//   - F8 §2.2 (D2): the entries "open the ReadingView overlay … instead of
//     StudyView" — the remediation re-ROUTES the surface; nowhere does it
//     authorize starting narration audio at entry.
//   - F8 §2.4.4: outbound session→viewport sync only; playback state changes
//     ride ONLY the VA6 effect channel (F8 §1.4), i.e. tutor tool calls the
//     USER asked for — never the mount.
//
// ATTACK STRATEGY: the playback singleton (playbackStore) is the only
// playback authority in the TS layer, and its pinned contract makes `load`
// auto-play (playbackStore.test.ts: "loads a fresh item and starts playing
// from the top"). The overlay historically opens only over a playing item
// (F8 §1.2: `overlay === "reading" && playbackItem`, App.tsx:368-370), so
// the tempting implementation of "Talk → reader" is to dispatch
// `load(cast)` — which by contract STARTS NARRATION: exactly the owner's
// device defect. We therefore attack the one seam every Talk entry funnels
// through (views.ts `requestBookTutor` → BOOK_TUTOR_EVENT handler, pinned
// by libraryCardGrammar.test.ts), on both routes (reader / BookSurface).
//
// WHY A SHALLOW MOCK CANNOT FAKE A PASS: test 1 is the real reducer — the
// auto-play semantics of `load` are demonstrated on the production store,
// not a stub. Tests 2-3 are source gates over the production App.tsx
// handler region (the house idiom, voiceSessionPanel.test.ts): they scan
// the shipped source text, so no test double exists to satisfy them — only
// the production entry path itself can make them pass.

import { afterEach, describe, expect, it } from "vitest";

import type { AudioShelfItem } from "../../audioShelfModel";
import {
  getSnapshot,
  initialPlaybackState,
  playbackReducer,
  resetPlaybackStore,
} from "../../playbackStore";
import { BOOK_TUTOR_EVENT } from "../../views";
import { readSource } from "../readSource";

const APP = readSource(new URL("../../App.tsx", import.meta.url));

function cast(overrides: Partial<AudioShelfItem> = {}): AudioShelfItem {
  return {
    id: "e1",
    bookId: "b1",
    title: "The Book",
    path: "/x/e1.mp3",
    mode: "narrate",
    format: "mp3",
    bytes: 1,
    stale: false,
    isScanned: false,
    jobId: "j1",
    createdAt: 10,
    ...overrides,
  };
}

afterEach(() => resetPlaybackStore());

const BOOK_TUTOR_EVENT_NAME = "BOOK_TUTOR_EVENT";

// Brace-matched extraction of a function body starting at the first `{`
// at/after `from` — mechanical, no window guessing.
function bodyFrom(source: string, from: number): string {
  const open = source.indexOf("{", from);
  if (open === -1) return "";
  let depth = 0;
  for (let at = open; at < source.length; at += 1) {
    if (source[at] === "{") depth += 1;
    else if (source[at] === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(open, at + 1);
    }
  }
  return source.slice(open);
}

// The BOOK_TUTOR_EVENT handler in App.tsx: every Talk entry (Library card,
// JobModal card — libraryCardGrammar.test.ts pins `requestBookTutor`)
// dispatches this event; App is its only consumer (grep: src/App.tsx,
// src/views.ts). Extract exactly the listener callback — named or inline —
// plus, for a named handler, its definition body.
function tutorHandlerRegions(): string[] {
  const regions: string[] = [];
  const listeners = [
    ...APP.matchAll(
      /addEventListener\(\s*BOOK_TUTOR_EVENT\s*(?:as\s+\w+\s*)?,\s*([A-Za-z_$][\w$]*)?/g,
    ),
  ];
  for (const match of listeners) {
    const name = match[1];
    if (name != null && name !== "") {
      // Named handler: extract its `const <name> = …` body.
      const def = APP.indexOf(`const ${name} =`);
      if (def !== -1) regions.push(bodyFrom(APP, def));
      const fn = APP.indexOf(`function ${name}`);
      if (fn !== -1) regions.push(bodyFrom(APP, fn));
    } else {
      // Inline arrow: extract the callback body right at the call site.
      regions.push(bodyFrom(APP, (match.index ?? 0) + match[0].length));
    }
  }
  return regions.filter((region) => region.length > 0);
}

// One level of call-following: a handler that delegates to a helper which
// dispatches `load` (e.g. the legitimate user-action openNowPlaying) is
// still an auto-play at Talk entry. Pull the bodies of every function the
// handler invokes and audit them under the same law.
const IGNORED_CALLS = new Set([
  "useCallback",
  "useEffect",
  "useState",
  "useRef",
  "useMemo",
  "setOverlay",
  "console",
  "removeEventListener",
  "addEventListener",
]);

function calledHelperBodies(region: string): string[] {
  const bodies: string[] = [];
  for (const call of region.matchAll(/([A-Za-z_$][\w$]*)\(/g)) {
    const name = call[1];
    if (IGNORED_CALLS.has(name)) continue;
    const def = APP.indexOf(`const ${name} = `);
    if (def !== -1) bodies.push(bodyFrom(APP, def));
    const fn = APP.indexOf(`function ${name}(`);
    if (fn !== -1) bodies.push(bodyFrom(APP, fn));
  }
  return bodies;
}

describe("D-QA4: Talk entry fires no play command", () => {
  it("premise (production reducer, not a mock): `load` IS an auto-play — so any load in the entry path starts narration", () => {
    const next = playbackReducer(initialPlaybackState, {
      type: "load",
      item: cast(),
    });
    // This is the pinned store contract. If this ever flips to false the
    // gates below are auditing a threat that no longer exists — re-derive
    // the attack, do not silently keep the gates.
    expect(next.playing).toBe(true);
  });

  it("App.tsx consumes the Talk event (the F8 D2 route exists at all)", () => {
    expect(
      APP.includes(BOOK_TUTOR_EVENT_NAME),
      "App.tsx no longer handles BOOK_TUTOR_EVENT — the Talk entry seam moved; re-anchor this suite",
    ).toBe(true);
    // F8 §2.2: the entry resolves through the route model, not ad-hoc.
    expect(
      APP.includes("tutorEntryRoute"),
      "the Talk entry must resolve through tutorEntryRoute (F8 §2.2 P9)",
    ).toBe(true);
  });

  it("reader route (followable cast): the Talk handler never dispatches load/play/toggle", () => {
    const regions = tutorHandlerRegions();
    expect(regions.length).toBeGreaterThan(0);
    for (const region of regions) {
      // `load` auto-plays by contract (premise test above); `play`/`toggle`
      // are play commands outright. NONE may appear in the entry path.
      expect(
        region,
        "Talk entry dispatches a `load` — by the store contract that AUTO-STARTS narration (owner defect D-QA4)",
      ).not.toMatch(/type:\s*"load"/);
      expect(region, "Talk entry dispatches an explicit play").not.toMatch(
        /type:\s*"play"/,
      );
      expect(region, "Talk entry dispatches a toggle").not.toMatch(
        /type:\s*"toggle"/,
      );
      expect(
        region,
        "Talk entry applies a resume playback-effect at mount",
      ).not.toMatch(/applyPlaybackEffect\(\s*\{\s*op:\s*"resume"/);
    }
  });

  it("one call deep: no helper the Talk handler delegates to dispatches load/play/toggle", () => {
    const regions = tutorHandlerRegions();
    expect(regions.length).toBeGreaterThan(0);
    for (const region of regions) {
      for (const body of calledHelperBodies(region)) {
        expect(
          body,
          "a helper invoked from the Talk entry dispatches a play command — auto-play one call deep (owner defect D-QA4)",
        ).not.toMatch(/dispatch\(\s*\{\s*type:\s*"(play|toggle)"/);
        // Arbitration r1: play-intent is an explicit reducer input; naked load remains forbidden.
        for (const load of body.matchAll(
          /dispatch\(\s*\{\s*type:\s*"load"[^)]*/g,
        )) {
          expect(
            load[0],
            "a helper invoked from the Talk entry dispatches a naked `load` (no explicit autoplay intent) — auto-play one call deep (owner defect D-QA4)",
          ).toMatch(/autoplay:/);
        }
      }
    }
  });

  it("no-cast route (BookSurface): the handler region touches no playback surface at all", () => {
    for (const region of tutorHandlerRegions()) {
      // The BookSurface reader has no narration track (F8 §2.2.2: word
      // overlay and cursor DISABLED); the Talk handler must not seed
      // playback for it through any dispatch shape.
      expect(region).not.toMatch(
        /dispatch\(\s*\{\s*type:\s*"(load|play|toggle)"/,
      );
    }
  });

  it("behavioral floor: nothing in module init or route modeling starts playback", async () => {
    // Importing the entry models (what a mount executes at module level)
    // and resolving both routes must leave the production playback
    // singleton untouched and idle.
    const { tutorEntryRoute } = await import("../../tutorEntryModel");
    const before = getSnapshot();
    tutorEntryRoute(
      {
        id: "b1",
        title: "The Book",
        author: null,
        pageCount: 40,
        chapterCount: 3,
        language: null,
        isScanned: false,
        coverPath: null,
        createdAt: 1,
        hasPseudoChapters: false,
        layoutAnalysed: true,
        reflowed: false,
        blocksStale: false,
      },
      [cast()],
    );
    tutorEntryRoute(
      {
        id: "b1",
        title: "The Book",
        author: null,
        pageCount: 40,
        chapterCount: 3,
        language: null,
        isScanned: false,
        coverPath: null,
        createdAt: 1,
        hasPseudoChapters: false,
        layoutAnalysed: true,
        reflowed: false,
        blocksStale: false,
      },
      [],
    );
    expect(getSnapshot()).toBe(before);
    expect(getSnapshot().playing).toBe(false);
    expect(getSnapshot().item).toBeNull();
  });

  it("BOOK_TUTOR_EVENT is exported (this suite anchors on the production seam, not a copy)", () => {
    expect(BOOK_TUTOR_EVENT).toBe("txtcaster:book-tutor");
  });
});
