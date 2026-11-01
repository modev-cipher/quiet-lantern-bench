// F11-U6 pins — docs/notes/f11-u6-mechanism.md §0 contract (C1–C3, C8, C9,
// C11) and spec §7.4 D3/D8/D9/D10. Pure model decisions + list markup; the
// catalogs are checked in both locales for every plural key this unit adds.
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

import { ar, en } from "../i18n/catalog";
import { selectForm, stripIsolates, formatMessage } from "../i18n/format";
import {
  deleteGateOf,
  displayOrder,
  queuePosition,
  queuePositionLabel,
  type JobSummary,
} from "../jobModel";
import { SessionHistoryList } from "../SessionHistory";
import {
  historyFootLine,
  INITIAL_RETENTION,
  type HistoryScopeState,
  type RetentionState,
  type SessionSummary,
} from "../sessionHistoryStore";

const ready = (rows: SessionSummary[]): HistoryScopeState => ({
  status: "ready",
  rows,
  error: null,
});
const row = (id: string): SessionSummary => ({
  id,
  book_id: "book-1",
  job_id: "job-1",
  started_at: Date.UTC(2026, 7, 25, 21, 40),
  ended_at: Date.UTC(2026, 7, 25, 22, 18),
  end_reason: "ended",
  duration_ms: 38 * 60_000,
  turn_count: 12,
  summary_excerpt: null,
  position: { kind: "ms", value: 1 },
  job_resolves: true,
  provider_changed: false,
});
const KEEP: RetentionState = { status: "ready", value: "keep" };
const NEVER: RetentionState = { status: "ready", value: "never" };
const UNREADABLE: RetentionState = { status: "error", value: null };

describe("C1–C3 / D8–D10 — the delete-cast gate decides from the preview", () => {
  it("D9: zero sessions deletes at once with the queue-removal title", () => {
    expect(deleteGateOf({ session_count: 0, live: false })).toEqual({
      kind: "immediate",
      title: en["job.deleteTitle"],
    });
  });
  it.each([1, 2, 7])("D8: %i sessions ask first and name the count", (n) => {
    const gate = deleteGateOf({ session_count: n, live: false });
    expect(gate.kind).toBe("confirm");
    if (gate.kind !== "confirm") return;
    expect(gate.question).toBe(
      n === 1
        ? "Delete this cast? This also removes 1 tutor session."
        : `Delete this cast? This also removes ${n} tutor sessions.`,
    );
    expect(gate.confirmLabel).toBe(
      n === 1 ? "Delete cast and 1 session" : `Delete cast and ${n} sessions`,
    );
    expect(gate.cancelLabel).toBe("Keep");
  });
  it("D10: a live session refuses with the spec title", () => {
    expect(deleteGateOf({ session_count: 3, live: true })).toEqual({
      kind: "refused",
      title: "End the tutor session first",
    });
  });
  it("no preview yet → pending (never deletes on a guess)", () => {
    expect(deleteGateOf(null)).toEqual({ kind: "pending" });
  });
});

const job = (id: string, state: JobSummary["state"], createdAt: number) =>
  ({
    id,
    bookId: "b",
    scope: { kind: "chapters", chapter_ids: [] },
    mode: "narrator",
    voiceProfileId: "v",
    outputFormat: "mp3",
    state,
    overnight: false,
    etaSeconds: null,
    reportJson: null,
    outputPathsJson: null,
    createdAt,
    updatedAt: createdAt,
  }) as JobSummary;

describe("C9 — queue position is creation order among QUEUED casts", () => {
  const running = job("run", "RENDERING", 1);
  const first = job("q1", "QUEUED", 10);
  const second = job("q2", "QUEUED", 20);
  const third = job("q3", "QUEUED", 30);
  const done = job("done", "DONE", 5);
  const all = [third, done, running, second, first];
  it("counts only queued casts created earlier; others are null", () => {
    expect(queuePosition(first, all)).toBe(0);
    expect(queuePosition(second, all)).toBe(1);
    expect(queuePosition(third, all)).toBe(2);
    expect(queuePosition(running, all)).toBeNull();
    expect(queuePosition(done, all)).toBeNull();
  });
  it("breaks a created_at tie by id so positions stay distinct", () => {
    const a = job("a", "QUEUED", 7);
    const b = job("b", "QUEUED", 7);
    expect([queuePosition(a, [a, b]), queuePosition(b, [a, b])]).toEqual([
      0, 1,
    ]);
  });
  it("labels: next, then N ahead (plural), null passthrough", () => {
    expect(queuePositionLabel(null)).toBeNull();
    expect(queuePositionLabel(0)).toBe("Next in queue");
    expect(queuePositionLabel(1)).toBe("1 cast ahead in the queue");
    expect(queuePositionLabel(4)).toBe("4 casts ahead in the queue");
  });
});

describe("C8 / D3 — the History foot line follows retention (§6.2 table)", () => {
  it("decides per the table", () => {
    expect(historyFootLine(ready([]), KEEP)).toBe("empty");
    expect(historyFootLine(ready([row("s")]), KEEP)).toBe("none");
    expect(historyFootLine(ready([]), NEVER)).toBe("retention");
    expect(historyFootLine(ready([row("s")]), NEVER)).toBe("retention");
    expect(historyFootLine(ready([]), UNREADABLE)).toBe("none");
    expect(historyFootLine(ready([]), INITIAL_RETENTION)).toBe("empty");
    expect(
      historyFootLine({ status: "loading", rows: [], error: null }, NEVER),
    ).toBe("none");
  });
  const render = (state: HistoryScopeState, retention: RetentionState) =>
    renderToStaticMarkup(
      createElement(SessionHistoryList, {
        scope: { bookId: "book-1" },
        state,
        showAll: false,
        onShowAll: () => {},
        onOpen: () => {},
        onContinue: () => {},
        onRetry: () => {},
        onForget: () => Promise.resolve(),
        nowMs: Date.UTC(2026, 7, 26, 9, 0),
        timeZone: "UTC",
        retention,
      }),
    );
  it("D3: Never + no rows renders the retention line and no empty line", () => {
    const markup = render(ready([]), NEVER);
    expect(markup).toContain('data-testid="session-history-retention"');
    expect(markup).toContain(en["session.retentionNever"]);
    expect(markup).toContain(en["session.retentionChange"]);
    expect(markup).not.toContain('data-testid="session-history-empty"');
  });
  it("H13: rows stay listed with the line beneath", () => {
    const markup = render(ready([row("s-kept")]), NEVER);
    expect(markup).toContain('data-testid="session-history-row-s-kept"');
    expect(markup).toContain('data-testid="session-history-retention"');
  });
  it("Keep keeps today's empty line; unreadable claims nothing", () => {
    expect(render(ready([]), KEEP)).toContain(
      'data-testid="session-history-empty"',
    );
    const silent = render(ready([]), UNREADABLE);
    expect(silent).not.toContain("session-history-empty");
    expect(silent).not.toContain("session-history-retention");
  });
});

describe("C11 / copy law — every new plural key resolves in EN and AR", () => {
  const keys = [
    "job.deleteQuestion",
    "job.deleteConfirm",
    "job.queueAhead",
    "library.deleteAllConfirm",
  ] as const;
  const counts = [0, 1, 2, 3, 11, 100];
  for (const [locale, catalog] of [
    ["en", en],
    ["ar", ar],
  ] as const) {
    it(`${locale}: no leftover placeholder, a form for every count`, () => {
      for (const key of keys) {
        const message = catalog[key];
        expect(typeof message).toBe("object");
        for (const count of counts) {
          const form = selectForm(locale, message, { count });
          expect(form, `${locale} ${key} ${count}`).toBeTruthy();
          const text = stripIsolates(formatMessage(locale, message, { count }));
          expect(text).not.toMatch(/\{count/);
          // Plain `{count}` tokens insert the digits as-is in both locales
          // (`pieces()`: String(raw); AR numerals are Latin, `ar-u-nu-latn`).
          if (count > 2 || (locale === "en" && count !== 1))
            expect(text).toContain(String(count));
        }
      }
    });
  }
  it("#31: no engine, model or stack vocabulary in the new copy", () => {
    const banned = /kokoro|whisper|sqlite|tauri|rust|gemini|openai|llm|tts/i;
    for (const catalog of [en, ar]) {
      for (const [key, value] of Object.entries(catalog)) {
        if (
          !/^(job\.delete|job\.queue|library\.|session\.retention|settings\.retention)/.test(
            key,
          )
        )
          continue;
        const texts =
          typeof value === "string" ? [value] : Object.values(value);
        for (const text of texts) expect(text, key).not.toMatch(banned);
      }
    }
  });
});

describe("C9 tie — display order and queue position share the (createdAt, id) key", () => {
  const a = job("job-a", "QUEUED", 7);
  const b = job("job-b", "QUEUED", 7);
  const c = job("job-c", "QUEUED", 9);
  it("displayOrder is the queue key reversed; positions decrease along it", () => {
    const shown = displayOrder([a, c, b]).map((j) => j.id);
    expect(shown).toEqual(["job-c", "job-b", "job-a"]);
    expect(
      displayOrder([a, c, b]).map((j) => queuePosition(j, [a, b, c])),
    ).toEqual([2, 1, 0]);
  });
});
