import { describe, expect, it } from "vitest";

import {
  applySnapshot,
  doneMessage,
  firewallHintDue,
  initialState,
  pairAnother,
  sentCount,
  serving,
  transferActive,
  type LanCastProgress,
  type LanSendSnapshot,
  type LanSendStartResult,
} from "./lanSendModel";

function startResult(
  overrides: Partial<LanSendStartResult> = {},
): LanSendStartResult {
  return {
    qrSvg: "<svg id='a'/>",
    qrPayload: '{"v":1,"a":1}',
    fingerprint: "a".repeat(64),
    hosts: ["192.168.1.20:47317"],
    port: 47317,
    secretTtlMs: 300_000,
    ...overrides,
  };
}

function cast(overrides: Partial<LanCastProgress> = {}): LanCastProgress {
  return {
    castId: "cst_a",
    title: "Book",
    bytesSent: 0,
    bytesTotal: 1000,
    sentComplete: false,
    missing: false,
    ...overrides,
  };
}

function snapshot(overrides: Partial<LanSendSnapshot> = {}): LanSendSnapshot {
  return {
    phase: "sending",
    pairedDevices: 1,
    deviceNames: ["Phone"],
    pairLocked: false,
    acceptedConnections: 1,
    casts: [cast()],
    ...overrides,
  };
}

// A session parked in the serving step, ready to receive snapshots.
function servingState() {
  return serving(initialState(["cst_a"]), startResult(), 0);
}

describe("applySnapshot rejects snapshots outside the serving step", () => {
  it("ignores snapshots while in pick", () => {
    const state = initialState(["cst_a"]);
    const next = applySnapshot(state, snapshot());
    expect(next).toBe(state);
    expect(next.snapshot).toBeNull();
  });

  it("does not resurrect serving from the error step", () => {
    const errored = {
      ...servingState(),
      step: "error" as const,
      error: "boom",
    };
    const next = applySnapshot(errored, snapshot({ phase: "sending" }));
    expect(next.step).toBe("error");
    expect(next.snapshot).toBeNull();
  });

  it("does not overwrite a completed transfer from the done step", () => {
    const done = applySnapshot(
      servingState(),
      snapshot({ phase: "done", casts: [cast({ sentComplete: true })] }),
    );
    expect(done.step).toBe("done");
    const frozenCount = sentCount(done.snapshot);

    // A late 'paired' snapshot (phone reconnecting, poll racing the event) must
    // not roll the completed transfer backwards.
    const late = applySnapshot(
      done,
      snapshot({ phase: "paired", casts: [cast()] }),
    );
    expect(late.step).toBe("done");
    expect(sentCount(late.snapshot)).toBe(frozenCount);
    expect(late.snapshot).toBe(done.snapshot);
  });
});

describe("progress after done is frozen", () => {
  it("keeps the done count and message once complete", () => {
    const done = applySnapshot(
      servingState(),
      snapshot({
        phase: "done",
        casts: [
          cast({ sentComplete: true }),
          cast({ castId: "cst_b", sentComplete: true }),
        ],
      }),
    );
    expect(done.step).toBe("done");
    expect(sentCount(done.snapshot)).toBe(2);
    expect(doneMessage(sentCount(done.snapshot))).toEqual({
      key: "lan.done",
      params: { count: 2 },
    });

    // Further sending snapshots are dropped by the step guard.
    const after = applySnapshot(
      done,
      snapshot({ phase: "sending", casts: [cast()] }),
    );
    expect(sentCount(after.snapshot)).toBe(2);
    expect(after.snapshot).toBe(done.snapshot);
  });
});

describe("pairAnother keeps the fresh QR visible", () => {
  it("stays in done with a new QR that a stale snapshot cannot hide", () => {
    const done = applySnapshot(
      servingState(),
      snapshot({ phase: "done", casts: [cast({ sentComplete: true })] }),
    );
    expect(done.step).toBe("done");

    const fresh = startResult({
      qrSvg: "<svg id='b'/>",
      qrPayload: '{"v":1,"a":2}',
    });
    const reQr = pairAnother(done, fresh, 1000);
    expect(reQr.step).toBe("done");
    expect(reQr.reQr).toBe(true);
    expect(reQr.start).toBe(fresh);

    // The trailing 'done' snapshot from the previous phone hits the
    // step!=="serving" guard and is dropped, so the new QR is NOT hidden. This
    // is exactly why pairAnother avoids routing back through serving.
    const trailing = applySnapshot(reQr, snapshot({ phase: "done" }));
    expect(trailing.reQr).toBe(true);
    expect(trailing.start).toBe(fresh);
    expect(trailing.step).toBe("done");

    // KNOWN LIMITATION (owner): because step stays 'done', phone #2's later
    // 'paired'/'sending' snapshots are also dropped — its progress never
    // re-renders. Pin the current behavior so a future change is deliberate.
    const phoneTwo = applySnapshot(
      reQr,
      snapshot({ phase: "sending", casts: [cast()] }),
    );
    expect(phoneTwo.snapshot).toBe(reQr.snapshot);
  });
});

describe("dual-channel snapshot interleave stays stable", () => {
  it("does not flip-flop when event and poll sources reorder around done", () => {
    let state = servingState();
    const sending = snapshot({
      phase: "sending",
      casts: [cast({ bytesSent: 500 })],
    });
    const done = snapshot({
      phase: "done",
      casts: [cast({ sentComplete: true })],
    });

    // Two sources (event listener + poll interval) deliver duplicated and
    // reordered snapshots; a stale 'sending' arrives AFTER 'done'.
    const stream = [sending, done, sending, done, sending];
    for (const snap of stream) {
      state = applySnapshot(state, snap);
    }
    expect(state.step).toBe("done");
    expect(sentCount(state.snapshot)).toBe(1);
  });
});

describe("firewall hint and transfer-active heuristics", () => {
  it("fires only after 60s of serving with zero connections", () => {
    const state = {
      ...servingState(),
      snapshot: snapshot({ acceptedConnections: 0 }),
    };
    expect(firewallHintDue(state, 59_999)).toBe(false);
    expect(firewallHintDue(state, 60_000)).toBe(true);
  });

  it("stays suppressed forever once any peer has connected", () => {
    const state = {
      ...servingState(),
      snapshot: snapshot({ acceptedConnections: 1 }),
    };
    expect(firewallHintDue(state, 120_000)).toBe(false);
    expect(firewallHintDue(state, 3_600_000)).toBe(false);
  });

  it("never fires outside the serving step", () => {
    const done = {
      ...servingState(),
      step: "done" as const,
      snapshot: snapshot({ acceptedConnections: 0 }),
    };
    expect(firewallHintDue(done, 600_000)).toBe(false);
  });

  it("treats an all-missing library as no active transfer", () => {
    const missing = {
      ...servingState(),
      snapshot: snapshot({
        pairedDevices: 1,
        casts: [
          cast({ missing: true }),
          cast({ castId: "cst_b", missing: true }),
        ],
      }),
    };
    expect(transferActive(missing)).toBe(false);
  });

  it("is an active transfer while bytes are still owed", () => {
    const live = {
      ...servingState(),
      snapshot: snapshot({
        pairedDevices: 1,
        casts: [cast({ sentComplete: false })],
      }),
    };
    expect(transferActive(live)).toBe(true);
  });
});
