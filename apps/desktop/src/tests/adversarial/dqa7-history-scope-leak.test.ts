import { beforeEach, describe, expect, it, vi } from "vitest";

type Pending = {
  resolve: (rows: unknown) => void;
  reject: (error: unknown) => void;
  args: Record<string, unknown>;
};

const pending: Pending[] = [];

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(
    (_cmd: string, args: Record<string, unknown>) =>
      new Promise((resolve, reject) => {
        pending.push({ resolve, reject, args });
      }),
  ),
}));

import {
  ensureHistory,
  getHistorySnapshot,
  loadHistory,
  resetSessionHistoryStore,
  subscribeHistory,
  type SessionSummary,
} from "../../sessionHistoryStore";

const row = (id: string, bookId: string): SessionSummary => ({
  id,
  book_id: bookId,
  job_id: null,
  started_at: 1_756_000_000_000,
  ended_at: 1_756_000_600_000,
  end_reason: "ended",
  duration_ms: 600_000,
  turn_count: 4,
  summary_excerpt: `about ${bookId}`,
  position: { kind: "offset", value: 120 },
  job_resolves: false,
  provider_changed: false,
});

const A = { bookId: "book-A" };
const B = { bookId: "book-B" };

function mountFor(scope: { bookId: string | null }) {
  const unsubscribe = subscribeHistory(() => {});
  ensureHistory(scope);
  return {
    rendered: () => getHistorySnapshot(scope).rows,
    status: () => getHistorySnapshot(scope).status,
    unmount: unsubscribe,
  };
}

async function flush(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

beforeEach(() => {
  pending.length = 0;
  resetSessionHistoryStore();
});

describe("DQA-H1: a different scope's mount renders zero cached rows", () => {
  it("scope A cached, scope B mounts before its fetch resolves — B renders no rows", async () => {
    const a = mountFor(A);
    pending.shift()?.resolve([row("s-a1", "book-A"), row("s-a2", "book-A")]);
    await flush();
    expect(a.rendered()).toHaveLength(2);
    a.unmount();

    const b = mountFor(B);
    expect(pending, "B must fetch its own scope").toHaveLength(1);
    expect(pending[0].args.bookId).toBe("book-B");
    expect(b.status()).toBe("loading");
    expect(
      b.rendered(),
      "book B's history mount rendered book A's cached rows before B's fetch resolved",
    ).toEqual([]);
    b.unmount();
  });

  it("a late response for scope A never lands in scope B, and B's own empty answer stays empty", async () => {
    void loadHistory(A);
    const lateA = pending.shift();
    const b = mountFor(B);
    const forB = pending.shift();
    lateA?.resolve([row("s-a1", "book-A")]);
    await flush();
    expect(b.rendered()).toEqual([]);
    forB?.resolve([]);
    await flush();
    expect(b.status()).toBe("ready");
    expect(b.rendered()).toEqual([]);
    expect(getHistorySnapshot(A).rows).toHaveLength(1);
  });

  it("a superseded request for the same scope is discarded (latest request wins)", async () => {
    void loadHistory(A);
    const first = pending.shift();
    void loadHistory(A);
    const second = pending.shift();
    second?.resolve([row("s-a2", "book-A")]);
    await flush();
    first?.resolve([row("s-a1", "book-A"), row("s-stale", "book-A")]);
    await flush();
    expect(getHistorySnapshot(A).rows.map((r) => r.id)).toEqual(["s-a2"]);
  });

  it("standalone and a book scope are disjoint entries", async () => {
    void loadHistory({ bookId: null });
    pending.shift()?.resolve([row("s-solo", "")]);
    await flush();
    expect(getHistorySnapshot({ bookId: null }).rows).toHaveLength(1);
    expect(getHistorySnapshot(B).rows).toEqual([]);
    expect(getHistorySnapshot(B)).not.toBe(
      getHistorySnapshot({ bookId: null }),
    );
  });
});
