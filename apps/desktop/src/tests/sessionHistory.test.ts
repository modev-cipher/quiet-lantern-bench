import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

import { F11_STRINGS, type F11StringKey } from "../f11Strings";
import { SessionHistoryList } from "../SessionHistory";
import SessionView from "../SessionView";
import VoiceSessionPanel from "../VoiceSessionPanel";
import {
  END_REASONS,
  HISTORY_PREVIEW_ROWS,
  chaptersLabel,
  endReasonLine,
  formatDuration,
  formatSessionDate,
  historyVisible,
  openTargetOf,
  showAllAvailable,
  visibleRows,
  type HistoryScopeState,
  type SessionSummary,
} from "../sessionHistoryStore";
import {
  bindVoicePanelBook,
  dispatchVoiceEvent,
  getVoiceSnapshot,
  markStarting,
  resetVoiceSessionStore,
} from "../voiceSessionStore";
import { stripIsolates } from "../i18n/format";
import { readSource } from "./readSource";

const NOW = Date.UTC(2026, 7, 26, 9, 0);
const TZ = "UTC";

function row(overrides: Partial<SessionSummary> = {}): SessionSummary {
  return {
    id: "s-1",
    book_id: "book-1",
    job_id: "job-1",
    started_at: Date.UTC(2026, 7, 25, 21, 40),
    ended_at: Date.UTC(2026, 7, 25, 22, 18),
    end_reason: "ended",
    duration_ms: 38 * 60_000,
    turn_count: 12,
    summary_excerpt: "We proved Lemma 3.2 and started on measure spaces.",
    position: { kind: "ms", value: 1_234_000 },
    job_resolves: true,
    provider_changed: false,
    ...overrides,
  };
}

function rows(count: number): SessionSummary[] {
  return Array.from({ length: count }, (_, at) =>
    row({
      id: `s-${at}`,
      started_at: Date.UTC(2026, 7, 25 - at, 21, 40),
      ended_at: Date.UTC(2026, 7, 25 - at, 22, 0),
    }),
  );
}

function ready(list: SessionSummary[]): HistoryScopeState {
  return { status: "ready", rows: list, error: null };
}

const noop = () => {};

function renderList(
  state: HistoryScopeState,
  extra: Partial<Parameters<typeof SessionHistoryList>[0]> = {},
): string {
  return renderToStaticMarkup(
    createElement(SessionHistoryList, {
      scope: { bookId: "book-1" },
      state,
      showAll: false,
      onShowAll: noop,
      onOpen: noop,
      onContinue: noop,
      onRetry: noop,
      onForget: () => Promise.resolve(),
      nowMs: NOW,
      timeZone: TZ,
      ...extra,
    }),
  );
}

const count = (markup: string, needle: string): number =>
  markup.split(needle).length - 1;

const escaped = (text: string): string => text.replace(/'/g, "&#x27;");

describe("A1 — an ended session is listed with date, duration and excerpt", () => {
  it("renders the three lines of the newest row and its Continue affordance", () => {
    const markup = renderList(ready([row()]));
    expect(markup).toContain(F11_STRINGS.pastSessions);
    expect(markup).toContain('data-testid="session-history-row-s-1"');
    expect(markup).toContain("Yesterday, 21:40");
    expect(markup).toContain("38 min");
    expect(markup).toContain(
      "We proved Lemma 3.2 and started on measure spaces.",
    );
    expect(count(markup, 'data-testid="session-history-continue"')).toBe(1);
  });

  it("an H4 row carries its reason; a normal row carries none", () => {
    const orphan = renderList(ready([row({ end_reason: "unexpected" })]));
    expect(orphan).toContain(F11_STRINGS.reasonUnexpected);
    const normal = renderList(ready([row()]));
    expect(normal).not.toContain(F11_STRINGS.reasonUnexpected);
  });

  it("an H2 row (no summary) still renders date and duration, with no excerpt node", () => {
    const markup = renderList(ready([row({ summary_excerpt: null })]));
    expect(markup).toContain("Yesterday, 21:40");
    expect(markup).toContain('data-testid="session-history-duration"');
    expect(markup).not.toContain('data-testid="session-history-excerpt"');
  });
});

const textOf = (markup: string): string => markup.replace(/<[^>]+>/g, "");

const tagsOf = (markup: string): string[] =>
  [...markup.matchAll(/<([a-z0-9]+)[\s>]/g)].map((m) => m[1]);

describe("A2 — zero rows shows exactly the empty line and no list chrome", () => {
  it("book scope: the label, then the one calm line, and nothing else user-visible", () => {
    const markup = renderList(ready([]));
    expect(textOf(markup)).toBe(
      `${F11_STRINGS.pastSessions}${F11_STRINGS.historyEmptyBook}`,
    );
    expect(tagsOf(markup)).toEqual(["section", "p", "p"]);
    expect(markup).not.toContain("<img");
    expect(markup).not.toContain("<svg");
  });

  it("book scope: the empty line only", () => {
    const markup = renderList(ready([]));
    expect(markup).toContain(F11_STRINGS.historyEmptyBook);
    expect(markup).not.toContain("<ul");
    expect(markup).not.toContain('data-testid="session-history-row-');
    expect(markup).not.toContain(F11_STRINGS.showAll);
    expect(markup).not.toContain(F11_STRINGS.forgetBook);
    expect(markup).not.toContain(F11_STRINGS.continueSession);
  });

  it("standalone scope: its own empty line, no book wording", () => {
    const markup = renderList(ready([]), { scope: { bookId: null } });
    expect(textOf(markup)).toBe(
      `${F11_STRINGS.pastSessions}${F11_STRINGS.historyEmptyStandalone}`,
    );
    expect(tagsOf(markup)).toEqual(["section", "p", "p"]);
    expect(markup).not.toContain("for this book");
  });

  it("loading and error states are honest lines, never an empty-list claim", () => {
    const loading = renderList({ status: "loading", rows: [], error: null });
    expect(loading).toContain(F11_STRINGS.historyLoading);
    expect(loading).not.toContain(F11_STRINGS.historyEmptyBook);
    const failed = renderList({ status: "error", rows: [], error: "boom" });
    expect(failed).toContain(escaped(F11_STRINGS.historyError));
    expect(failed).toContain(F11_STRINGS.retry);
    expect(failed).not.toContain(F11_STRINGS.historyEmptyBook);
  });
});

describe("A9 — Show all appears only past five rows", () => {
  it("five rows: all listed, no Show all", () => {
    const markup = renderList(ready(rows(5)));
    expect(count(markup, 'data-testid="session-history-row-')).toBe(5);
    expect(markup).not.toContain('data-testid="session-history-show-all"');
    expect(showAllAvailable(rows(5))).toBe(false);
  });

  it("six rows: five listed plus Show all; expanded lists all six", () => {
    const collapsed = renderList(ready(rows(6)));
    expect(count(collapsed, 'data-testid="session-history-row-')).toBe(5);
    expect(collapsed).toContain('data-testid="session-history-show-all"');
    const expanded = renderList(ready(rows(6)), { showAll: true });
    expect(count(expanded, 'data-testid="session-history-row-')).toBe(6);
    expect(expanded).not.toContain('data-testid="session-history-show-all"');
    expect(HISTORY_PREVIEW_ROWS).toBe(5);
    expect(visibleRows(rows(6), false)).toHaveLength(5);
  });

  it("Continue rides the newest row only", () => {
    const markup = renderList(ready(rows(3)));
    expect(count(markup, 'data-testid="session-history-continue"')).toBe(1);
    const first = markup.indexOf('data-testid="session-history-row-s-0"');
    const second = markup.indexOf('data-testid="session-history-row-s-1"');
    const cont = markup.indexOf('data-testid="session-history-continue"');
    expect(cont).toBeGreaterThan(first);
    expect(cont).toBeLessThan(second);
  });
});

describe("A6 — the session view renders every turn in order as text", () => {
  const turns = [
    { turn_idx: 2, role: "assistant", text: "Second reply", created_at: 3 },
    {
      turn_idx: 0,
      role: "user",
      text: "<script>alert(1)</script>",
      created_at: 1,
    },
    { turn_idx: 1, role: "assistant", text: "First reply", created_at: 2 },
  ];
  const renderView = (extra: Record<string, unknown> = {}) =>
    renderToStaticMarkup(
      createElement(SessionView, {
        row: row(),
        detail: {
          status: "ready",
          detail: {
            row: row(),
            turns,
          },
          error: null,
        },
        notice: null,
        onBack: noop,
        onContinue: noop,
        onDelete: () => Promise.resolve(),
        deleteError: null,
        nowMs: NOW,
        timeZone: TZ,
        ...extra,
      }),
    );

  it("turns appear in turn_idx order inside role=log", () => {
    const markup = renderView();
    expect(markup).toContain('role="log"');
    const first = markup.indexOf("First reply");
    const second = markup.indexOf("Second reply");
    const script = markup.indexOf("&lt;script&gt;alert(1)&lt;/script&gt;");
    expect(script).toBeGreaterThan(-1);
    expect(script).toBeLessThan(first);
    expect(first).toBeLessThan(second);
  });

  it("a stored <script> turn renders literally, never as markup", () => {
    const markup = renderView();
    expect(markup).not.toContain("<script>");
  });

  it("header carries date, duration and the H9 notice when given", () => {
    const markup = renderView({ notice: F11_STRINGS.h9Notice });
    expect(markup).toContain("Yesterday, 21:40");
    expect(markup).toContain("38 min");
    expect(markup).toContain(F11_STRINGS.h9Notice);
    expect(markup).toContain(F11_STRINGS.summary);
    expect(count(markup, 'data-testid="session-view-continue"')).toBe(1);
    expect(markup).toContain('data-testid="delete-session-view-s-1"');
  });
});

describe("A7 — Open turns the page only when the row's cast is the reader's", () => {
  it("H7 on the current cast yields the stored ms", () => {
    expect(openTargetOf(row(), "job-1")).toEqual({
      kind: "ms",
      ms: 1_234_000,
      notice: null,
    });
  });

  it("H9 (another cast) yields no page turn and the H9 notice", () => {
    expect(openTargetOf(row(), "job-2")).toEqual({
      kind: "none",
      notice: F11_STRINGS.h9Notice,
    });
    expect(openTargetOf(row(), null).notice).toBe(F11_STRINGS.h9Notice);
  });

  it("H5 and H6 rows never turn the page and carry no notice", () => {
    const standalone = row({
      book_id: null,
      job_id: null,
      position: { kind: "none", value: null },
    });
    expect(openTargetOf(standalone, "job-1")).toEqual({
      kind: "none",
      notice: null,
    });
    const direct = row({
      job_id: null,
      position: { kind: "offset", value: 512 },
    });
    expect(openTargetOf(direct, null)).toEqual({ kind: "none", notice: null });
  });
});

describe("A8 — the list never renders while a session is live", () => {
  it("visibility follows the session status and the starting flag", () => {
    expect(historyVisible("idle", false)).toBe(true);
    expect(historyVisible("ended", false)).toBe(true);
    expect(historyVisible("starting", false)).toBe(false);
    expect(historyVisible("live", false)).toBe(false);
    expect(historyVisible("renewing", false)).toBe(false);
    expect(historyVisible("idle", true)).toBe(false);
  });

  it("store transition: shown idle, gone from Start through live and renewing, back after Ended", () => {
    resetVoiceSessionStore();
    bindVoicePanelBook("book-1");
    const shown = (): boolean => {
      const snapshot = getVoiceSnapshot();
      const markup = renderToStaticMarkup(
        createElement(VoiceSessionPanel, {
          bookId: "book-1",
          jobId: "job-1",
          variant: "side",
        }),
      );
      const rendered = markup.includes('data-testid="session-history"');
      expect(rendered).toBe(
        historyVisible(snapshot.session.status, snapshot.starting),
      );
      return rendered;
    };
    const event = (event: Record<string, unknown>) =>
      dispatchVoiceEvent({ session_id: "s-live", book_id: "book-1", event });

    expect(shown()).toBe(true);
    markStarting();
    expect(shown()).toBe(false);
    event({ kind: "lifecycle", state: "connected" });
    expect(getVoiceSnapshot().session.status).toBe("live");
    expect(shown()).toBe(false);
    event({ kind: "lifecycle", state: "renewing" });
    expect(shown()).toBe(false);
    event({ kind: "lifecycle", state: "reconnected" });
    expect(shown()).toBe(false);
    event({ kind: "ended", reason: "user" });
    expect(getVoiceSnapshot().session.status).toBe("ended");
    expect(shown()).toBe(true);
    resetVoiceSessionStore();
  });

  it("the panel mounts the history only under historyVisible and refetches on remount", () => {
    const panel = readSource(
      new URL("../VoiceSessionPanel.tsx", import.meta.url),
    );
    expect(panel).toMatch(/historyVisible\(session\.status, starting\)/);
    expect(panel).toMatch(/\{historyShown && \([^]{0,400}<SessionHistory/);
    const history = readSource(
      new URL("../SessionHistory.tsx", import.meta.url),
    );
    expect(history).toMatch(
      /useEffect\(\(\) => \{\s*void loadHistory\(scope\)/,
    );
  });
});

describe("formatting — plain voice, en-GB shapes, no invented precision", () => {
  it("dates: today and yesterday by calendar day, else weekday + day + month", () => {
    expect(
      stripIsolates(formatSessionDate(Date.UTC(2026, 7, 26, 7, 5), NOW, TZ)),
    ).toBe("Today, 07:05");
    expect(
      stripIsolates(formatSessionDate(Date.UTC(2026, 7, 25, 21, 40), NOW, TZ)),
    ).toBe("Yesterday, 21:40");
    expect(
      stripIsolates(formatSessionDate(Date.UTC(2026, 7, 24, 9, 0), NOW, TZ)),
    ).toBe("Mon 24 Aug");
    expect(
      stripIsolates(formatSessionDate(Date.UTC(2025, 11, 31, 9, 0), NOW, TZ)),
    ).toBe("Wed, 31 Dec 2025");
  });

  it("durations round to minutes and never print seconds", () => {
    expect(stripIsolates(formatDuration(20_000))).toBe("<1 min");
    expect(stripIsolates(formatDuration(38 * 60_000))).toBe("38 min");
    expect(stripIsolates(formatDuration(72 * 60_000))).toBe("1 h 12 min");
    expect(stripIsolates(formatDuration(120 * 60_000))).toBe("2 h");
  });
});

describe("copy discipline — every new string goes through f11Strings", () => {
  const files = ["SessionHistory.tsx", "SessionView.tsx"].map((name) =>
    readSource(new URL(`../${name}`, import.meta.url)),
  );

  it("components import the table and carry none of the §4.6 copy literally", () => {
    for (const source of files) {
      // I18N-2: SessionView reads the catalog directly; the rail still reads the table.
      expect(
        source.includes('from "./f11Strings"') ||
          source.includes('from "./i18n"'),
      ).toBe(true);
      expect(source).not.toContain("No past sessions");
      expect(source).not.toContain("cannot be undone");
      expect(source).not.toContain("Ended when");
    }
  });

  it("the §4.6 rows exist verbatim in the table", () => {
    expect(F11_STRINGS.pastSessions).toBe("Past sessions");
    expect(F11_STRINGS.historyEmptyBook).toBe(
      "No past sessions for this book yet.",
    );
    expect(F11_STRINGS.reasonUnexpected).toBe("Ended unexpectedly");
    expect(F11_STRINGS.deleteConfirmQuestion).toBe(
      "Delete this session's transcript and summary? This cannot be undone.",
    );
    expect(F11_STRINGS.forgetConfirmQuestion).toBe(
      "Forget every saved session for this book? This cannot be undone.",
    );
    expect(F11_STRINGS.liveContinue).toBe(
      "A session is live. End it to continue this one.",
    );
  });

  it("every end-reason line in the table is reachable from the DTO's end_reason values", () => {
    const reasonLines = Object.keys(F11_STRINGS)
      .filter((key) => key.startsWith("reason"))
      .map((key) => F11_STRINGS[key as F11StringKey]);
    const reachable = END_REASONS.map((end_reason) =>
      endReasonLine(row({ end_reason })),
    ).filter((line) => line != null);
    expect(reasonLines.sort()).toEqual([...reachable].sort());
  });

  it("chapter labels: every visible character comes from the table", () => {
    expect(chaptersLabel(row({ chapters_touched: [3] }))).toBe(
      F11_STRINGS.chapterSingle(3),
    );
    expect(chaptersLabel(row({ chapters_touched: [4, 3] }))).toBe(
      F11_STRINGS.chapterRange(3, 4),
    );
    expect(chaptersLabel(row({ chapters_touched: [5, 1, 5] }))).toBe(
      F11_STRINGS.chapterList([1, 5]),
    );
    expect(chaptersLabel(row())).toBeNull();
    expect(stripIsolates(F11_STRINGS.chapterRange(3, 4))).toBe("Ch. 3–4");
    const store = readSource(
      new URL("../sessionHistoryStore.ts", import.meta.url),
    );
    expect(store).not.toMatch(/Ch\./);
  });

  it("no provider name reaches the history copy or the Start label", () => {
    const panel = readSource(
      new URL("../VoiceSessionPanel.tsx", import.meta.url),
    );
    expect(panel).not.toContain("Start (Gemini Live)");
    for (const source of [...files, JSON.stringify(F11_STRINGS)]) {
      expect(source).not.toMatch(/Gemini|Deepgram|OpenAI/);
    }
  });
});

describe("every command the history store invokes exists on the Rust side", () => {
  it("finds each invoke()'d name in the tauri command registry", () => {
    const store = readSource(
      new URL("../sessionHistoryStore.ts", import.meta.url),
    );
    const lib = readSource(
      new URL("../../src-tauri/src/lib.rs", import.meta.url),
    );
    const start = lib.indexOf("generate_handler![");
    const registry = lib.slice(start, lib.indexOf("])", start));
    const invoked = [
      ...new Set(
        [...store.matchAll(/invoke(?:<[^>]*>)?\(\s*"([a-z_]+)"/g)].map(
          (m) => m[1],
        ),
      ),
    ].sort();
    expect(invoked).toEqual([
      // F11-U6: the retention pair joins the store (spec §5.1).
      "voice_retention_get",
      "voice_retention_set",
      "voice_session_delete",
      "voice_session_forget_scope",
      "voice_session_list",
      "voice_session_transcript",
    ]);
    for (const name of invoked) expect(registry).toContain(name);
  });
});

describe("F11-U3 C1 — the end reason is typed and every variant has a line", () => {
  it("names the four reasons the runtime and the derive can produce", () => {
    expect([...END_REASONS]).toEqual([
      "ended",
      "unexpected",
      "expired",
      "connection_closed",
    ]);
  });

  it("the S6/S7 lines are the §4.6 copy, reachable from their reasons", () => {
    expect(F11_STRINGS.reasonExpired).toBe(
      "Ended when the provider session expired",
    );
    expect(F11_STRINGS.reasonConnectionClosed).toBe(
      "Ended when the connection closed",
    );
    expect(endReasonLine(row({ end_reason: "expired" }))).toBe(
      F11_STRINGS.reasonExpired,
    );
    expect(endReasonLine(row({ end_reason: "connection_closed" }))).toBe(
      F11_STRINGS.reasonConnectionClosed,
    );
    expect(endReasonLine(row({ end_reason: "ended" }))).toBeNull();
  });
});

describe("F11-U3 C3 — an H6 row opens to its derived page", () => {
  const direct = () =>
    row({
      job_id: null,
      position: { kind: "offset", value: 4_200 },
      location: { chapter: 2, page: 7 },
    });

  it("turns to the page the Rust derivation resolved", () => {
    expect(openTargetOf(direct(), null)).toEqual({
      kind: "page",
      page: 7,
      notice: null,
    });
    expect(openTargetOf(direct(), "job-1")).toEqual({
      kind: "page",
      page: 7,
      notice: null,
    });
  });

  it("does nothing when no page resolved — never a guessed page", () => {
    expect(
      openTargetOf(
        row({ ...direct(), location: { chapter: 2, page: null } }),
        null,
      ),
    ).toEqual({ kind: "none", notice: null });
    expect(openTargetOf(row({ ...direct(), location: null }), null)).toEqual({
      kind: "none",
      notice: null,
    });
  });

  it("an H7 row keeps the ms target (the reader owns that mapping)", () => {
    expect(
      openTargetOf(row({ location: { chapter: 1, page: 3 } }), "job-1"),
    ).toEqual({
      kind: "ms",
      ms: 1_234_000,
      notice: null,
    });
  });
});

describe("F11-U3 C2 — the session view shows the whole summary paragraph", () => {
  it("prefers the detail's summary_text over the row excerpt", () => {
    const markup = renderToStaticMarkup(
      createElement(SessionView, {
        row: row({ summary_excerpt: "We proved Lemma 3.2…" }),
        detail: {
          status: "ready",
          detail: {
            row: row(),
            turns: [],
            summary_text:
              "We proved Lemma 3.2 and started on measure spaces. Sigma algebras came next.",
          },
          error: null,
        },
        notice: null,
        onBack: noop,
        onContinue: noop,
        onDelete: () => Promise.resolve(),
        deleteError: null,
        nowMs: NOW,
        timeZone: TZ,
      }),
    );
    expect(markup).toContain("Sigma algebras came next.");
    expect(markup).not.toContain("Lemma 3.2…");
  });
});
