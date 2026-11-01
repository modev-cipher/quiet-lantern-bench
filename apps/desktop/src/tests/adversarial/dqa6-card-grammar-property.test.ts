// D-QA6 — card grammar + discoverability, exhaustive over card states.
//
// INVARIANT: every book card renders the IDENTICAL action-slot structure
// (#266 contract, libraryCardGrammar.test.ts: one primary slot, one fixed
// secondary row — Talk, demoted Cast, Delete, kebab) INCLUDING the F7 Talk
// slot, across ALL states (playable × has-jobs × pageCount); AND a Cast
// affordance is reachable (visible or one interaction away) in EVERY state.
//   Spec citations:
//   - #266 contract: libraryCardGrammar.test.ts (the ratified grammar).
//   - F7 §3 E1 / F7 P5: "Library card gains an always-rendered Talk button
//     (peer of Cast, ungated)" — any imported book.
//   - F7 §3: "Library card: Cast always" (libraryAvailability cited) —
//     Cast discoverability is unconditional.
//   - F8 §2.2 P9: the Talk entry routes by tutorEntryRoute — the route must
//     be TOTAL over every card state (never throw, never dead-end).
//   - SPEC-GAP (filed): no spec defines any has-jobs-dependent card
//     structure; the #266 grammar is availability-keyed only. The jobs
//     dimension is therefore exercised only through route/availability
//     totality, with no structural assertion to cite.
//
// ATTACK STRATEGY: (1) property sweep — enumerate the full state cube
// (shelf shapes × pageCount × jobs flags) through the PRODUCTION
// bookAvailability + tutorEntryRoute models: totality, closed route set,
// and the F7 P5 promise that Talk has a route in every single state.
// (2) source-grammar attack on Library.tsx (the #266 mechanism itself):
// the fixed secondary row's Delete/kebab must sit OUTSIDE the playable
// gate (a gate swallowing them would vary the structure per state and the
// existing order-only test would still pass — the hole this suite closes),
// Talk must sit outside it too, and BOTH primary branches must expose a
// Cast affordance so Cast is reachable whatever the branch.
//
// WHY A SHALLOW MOCK CANNOT FAKE A PASS: the sweep drives the shipped
// models over 100+ real state combinations (no doubles anywhere); the
// grammar tests read the shipped Library.tsx source — only the production
// card can satisfy them.

import { describe, expect, it } from "vitest";

import type { AudioShelfItem } from "../../audioShelfModel";
import { bookAvailability } from "../../libraryAvailability";
import type { BookSummary } from "../../libraryModel";
import { tutorEntryRoute } from "../../tutorEntryModel";
import { readSource } from "../readSource";

const SOURCE = readSource(new URL("../../Library.tsx", import.meta.url));
const CARD = SOURCE.slice(
  SOURCE.indexOf("function ShelfSlot"),
  SOURCE.indexOf("function ShelfSection"),
);

function book(overrides: Partial<BookSummary> = {}): BookSummary {
  return {
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
    ...overrides,
  };
}

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

// The state cube. Shelf dimension covers: nothing, playable fresh, stale,
// non-followable format, jobless export, foreign-book export, and mixes.
const SHELVES: { label: string; shelf: AudioShelfItem[] }[] = [
  { label: "empty", shelf: [] },
  { label: "fresh mp3", shelf: [cast()] },
  { label: "stale", shelf: [cast({ stale: true })] },
  { label: "wav", shelf: [cast({ format: "wav", path: "/x/e1.wav" })] },
  { label: "jobless", shelf: [cast({ jobId: null })] },
  { label: "other book", shelf: [cast({ bookId: "b2" })] },
  {
    label: "mixed stale+fresh",
    shelf: [cast({ id: "s", stale: true, createdAt: 5 }), cast()],
  },
  {
    label: "all unusable",
    shelf: [
      cast({ stale: true }),
      cast({ id: "e2", format: "wav" }),
      cast({ id: "e3", jobId: null }),
    ],
  },
];
const PAGE_COUNTS: (number | null)[] = [null, 0, 1, 40];
// has-jobs dimension: the card models expose no job list input, so jobs
// vary through the shelf's jobId field (jobless export ↔ job-backed) and
// the book flags that job activity toggles (blocksStale). SPEC-GAP: no
// spec text keys card STRUCTURE on jobs.
const JOB_FLAVORS: Partial<BookSummary>[] = [
  {},
  { blocksStale: true },
  { hasPseudoChapters: true },
];

describe("D-QA6 property sweep: route + availability are total over the state cube", () => {
  it("every state yields a closed availability and exactly one route kind — no throw, no dead end", () => {
    for (const { label, shelf } of SHELVES) {
      for (const pageCount of PAGE_COUNTS) {
        for (const flavor of JOB_FLAVORS) {
          const state = `${label} / pages=${pageCount} / ${JSON.stringify(flavor)}`;
          const b = book({ pageCount, ...flavor });
          const availability = bookAvailability(b.id, shelf);
          expect(
            availability.playable === null ||
              typeof availability.playable === "object",
            state,
          ).toBe(true);
          const route = tutorEntryRoute(b, shelf);
          expect(
            ["reader", "bookSurface", "study"].includes(route.kind),
            `${state}: route kind ${route.kind} is outside the F8 P9 closed set`,
          ).toBe(true);
        }
      }
    }
  });

  it("F7 P5: Talk has a live route in EVERY state (ungated — any imported book)", () => {
    for (const { label, shelf } of SHELVES) {
      for (const pageCount of PAGE_COUNTS) {
        const route = tutorEntryRoute(book({ pageCount }), shelf);
        expect(
          route,
          `${label} / pages=${pageCount}: Talk dead-ends`,
        ).not.toBeNull();
      }
    }
  });

  it("route honesty across the cube: pages win; reader only over a followable cast without pages", () => {
    for (const { label, shelf } of SHELVES) {
      for (const pageCount of PAGE_COUNTS) {
        const route = tutorEntryRoute(book({ pageCount }), shelf);
        const followable = shelf.some(
          (item) =>
            item.bookId === "b1" &&
            !item.stale &&
            item.format === "mp3" &&
            item.jobId != null,
        );
        const state = `${label} / pages=${pageCount}`;
        // Owner decision 2026-10-09: Talk opens the book itself, so
        // rendered pages take precedence over any cast.
        if (pageCount != null && pageCount > 0) {
          expect(
            route.kind,
            `${state}: rendered pages must open BookSurface`,
          ).toBe("bookSurface");
        } else if (followable) {
          expect(
            route.kind,
            `${state}: a readable cast without pages must open the reader`,
          ).toBe("reader");
        } else {
          expect(
            route.kind,
            `${state}: nothing renderable must fall back to Study`,
          ).toBe("study");
        }
      }
    }
  });
});

describe("D-QA6 grammar: identical action-slot structure, Cast reachable in every state", () => {
  const secondary = CARD.slice(
    CARD.indexOf('data-testid="book-card-secondary"'),
  );
  const gateToken = "availability.playable != null && (";

  it("the slots exist exactly once each, with the F7 Talk slot present", () => {
    expect(CARD.match(/data-testid="book-card-primary"/g)).toHaveLength(1);
    expect(CARD.match(/data-testid="book-card-secondary"/g)).toHaveLength(1);
    expect(secondary).toContain("talk-book-");
  });

  it("Talk and kebab sit OUTSIDE the playable gate, and Delete is an ungated menu item — structure identical per state", () => {
    const gate = secondary.indexOf(gateToken);
    expect(gate, "the playable gate itself moved — re-derive").toBeGreaterThan(
      -1,
    );
    const talk = secondary.indexOf("talk-book-");
    expect(talk).toBeGreaterThan(-1);
    expect(talk, "Talk slid inside the playable gate").toBeLessThan(gate);
    const open = gate + gateToken.length - 1;
    let depth = 0;
    let close = -1;
    for (let at = open; at < secondary.length; at += 1) {
      if (secondary[at] === "(") depth += 1;
      else if (secondary[at] === ")") {
        depth -= 1;
        if (depth === 0) {
          close = at;
          break;
        }
      }
    }
    expect(close, "unbalanced playable gate").toBeGreaterThan(-1);
    const menu = secondary.indexOf("<BookMenu");
    expect(
      menu,
      "BookMenu is INSIDE the playable gate — non-playable cards lose the kebab",
    ).toBeGreaterThan(close);
    const items = CARD.slice(
      CARD.indexOf("const menuItems"),
      CARD.indexOf("const title = bookDisplayTitle"),
    );
    const improveEnd = items.indexOf(": []),");
    expect(improveEnd).toBeGreaterThan(-1);
    expect(
      items.indexOf("delete-book-"),
      "Delete became conditional — some states would lose it",
    ).toBeGreaterThan(improveEnd);
  });

  it("Cast affordance is reachable in EVERY state: primary on the non-playable branch, demoted in the secondary row on the playable branch", () => {
    const primarySlot = CARD.slice(
      CARD.indexOf('data-testid="book-card-primary"'),
      CARD.indexOf('data-testid="book-card-secondary"'),
    );
    expect(
      primarySlot,
      "the non-playable primary branch lost its Cast affordance",
    ).toContain("create-audio-");
    expect(
      secondary,
      "the playable branch lost its demoted Cast — Cast becomes unreachable on playable cards",
    ).toContain("create-audio-");
    const gate = secondary.indexOf(gateToken);
    expect(secondary.indexOf("create-audio-")).toBeGreaterThan(gate);
  });

  it("the Talk slot routes through the F8 D2 router in the shipped card", () => {
    expect(secondary).toContain("requestBookTutor(book.id)");
  });

  it("no action row wraps: whole affordances only, in one fixed-height line", () => {
    expect(CARD).not.toContain("flex-wrap");
    expect(CARD).not.toContain("flex-col-reverse");
  });
});
