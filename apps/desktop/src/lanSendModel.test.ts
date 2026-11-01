import { describe, expect, it } from "vitest";

import {
  applySnapshot,
  canShowQr,
  castRowNote as castRowNoteMessage,
  doneMessage as doneMessageMessage,
  failed,
  firewallHintDue,
  initialState,
  keepSending,
  needsStopConfirm,
  pairAnother,
  plainError as plainErrorMessage,
  progressPercent,
  qrExpiryDue,
  qrVisible,
  requestStop,
  safetyCode,
  sentCount,
  serving,
  starting,
  statusLabel as statusLabelMessage,
  toggleSelected,
  transferActive,
  type LanCastProgress,
  type LanSendSnapshot,
  type LanSendStartResult,
} from "./lanSendModel";
import { readSource } from "./tests/readSource";

import { i18nFor, type MessageDescriptor } from "./i18n";
import { stripIsolates } from "./i18n/format";

function english(message: MessageDescriptor): string {
  return stripIsolates(i18nFor("en").t(message.key, message.params));
}
const castRowNote = (...args: Parameters<typeof castRowNoteMessage>) => {
  const message = castRowNoteMessage(...args);
  return message ? english(message) : null;
};
const doneMessage = (...args: Parameters<typeof doneMessageMessage>) =>
  english(doneMessageMessage(...args));
const plainError = (...args: Parameters<typeof plainErrorMessage>) =>
  english(plainErrorMessage(...args));
const statusLabel = (...args: Parameters<typeof statusLabelMessage>) =>
  english(statusLabelMessage(...args));

function startResult(): LanSendStartResult {
  return {
    qrSvg: "<svg/>",
    qrPayload: '{"v":1}',
    fingerprint: "a".repeat(64),
    hosts: ["192.168.1.20:47317"],
    port: 47317,
    secretTtlMs: 300_000,
  };
}

function cast(overrides: Partial<LanCastProgress> = {}): LanCastProgress {
  return {
    castId: "export-1",
    title: "A Book",
    bytesSent: 0,
    bytesTotal: 1000,
    sentComplete: false,
    missing: false,
    ...overrides,
  };
}

function snapshot(overrides: Partial<LanSendSnapshot> = {}): LanSendSnapshot {
  return {
    phase: "waiting",
    pairedDevices: 0,
    deviceNames: [],
    pairLocked: false,
    acceptedConnections: 0,
    casts: [cast()],
    ...overrides,
  };
}

describe("lan send state machine", () => {
  it("starts on the pick step with every audio selected", () => {
    const state = initialState(["a", "b"]);
    expect(state.step).toBe("pick");
    expect(state.selected).toEqual(["a", "b"]);
    expect(canShowQr(state)).toBe(true);
  });

  it("toggling every audio off disables the QR button", () => {
    let state = initialState(["a"]);
    state = toggleSelected(state, "a");
    expect(state.selected).toEqual([]);
    expect(canShowQr(state)).toBe(false);
    state = toggleSelected(state, "a");
    expect(canShowQr(state)).toBe(true);
  });

  it("moves pick → serving → done as snapshots arrive", () => {
    let state = starting(initialState(["a"]));
    expect(state.busy).toBe(true);
    state = serving(state, startResult(), 1_000);
    expect(state.step).toBe("serving");
    expect(state.servingSince).toBe(1_000);
    state = applySnapshot(
      state,
      snapshot({ phase: "sending", pairedDevices: 1 }),
    );
    expect(state.step).toBe("serving");
    state = applySnapshot(
      state,
      snapshot({
        phase: "done",
        pairedDevices: 1,
        casts: [cast({ sentComplete: true, bytesSent: 1000 })],
      }),
    );
    expect(state.step).toBe("done");
    expect(sentCount(state.snapshot)).toBe(1);
    expect(doneMessage(sentCount(state.snapshot))).toBe(
      "Sent 1 audiobook to your phone.",
    );
  });

  it("ignores snapshots outside the serving step", () => {
    const state = initialState(["a"]);
    expect(applySnapshot(state, snapshot())).toBe(state);
  });

  it("maps backend errors to plain words", () => {
    expect(plainError("no lan address")).toContain("same Wi-Fi");
    expect(plainError("could not open a port: EACCES")).toContain(
      "Another program may be blocking it",
    );
    expect(plainError("something odd")).toContain("something odd");
    const state = failed(initialState(["a"]), "no lan address");
    expect(state.step).toBe("error");
    expect(state.error).toBe("no lan address");
    expect(plainError(state.error!)).toContain("same Wi-Fi");
  });

  it("arms the firewall hint after 60 s with zero accepted connections", () => {
    let state = serving(initialState(["a"]), startResult(), 0);
    expect(firewallHintDue(state, 59_999)).toBe(false);
    expect(firewallHintDue(state, 60_000)).toBe(true);
    state = applySnapshot(state, snapshot({ acceptedConnections: 1 }));
    expect(firewallHintDue(state, 120_000)).toBe(false);
  });

  it("asks for confirmation only while a transfer is active", () => {
    let state = serving(initialState(["a"]), startResult(), 0);
    expect(transferActive(state)).toBe(false);
    expect(needsStopConfirm(state)).toBe(false);
    state = applySnapshot(
      state,
      snapshot({
        phase: "sending",
        pairedDevices: 1,
        casts: [cast({ bytesSent: 10 })],
      }),
    );
    expect(transferActive(state)).toBe(true);
    expect(needsStopConfirm(state)).toBe(true);
    state = requestStop(state);
    expect(state.confirmingStop).toBe(true);
    expect(needsStopConfirm(state)).toBe(false);
    state = keepSending(state);
    expect(state.confirmingStop).toBe(false);
    const done = applySnapshot(
      state,
      snapshot({
        phase: "done",
        pairedDevices: 1,
        casts: [cast({ sentComplete: true })],
      }),
    );
    expect(transferActive(done)).toBe(false);
  });

  it("pair another stays on the done step so the fresh QR is not hidden", () => {
    let state = serving(initialState(["a"]), startResult(), 0);
    state = applySnapshot(
      state,
      snapshot({
        phase: "done",
        pairedDevices: 1,
        casts: [cast({ sentComplete: true })],
      }),
    );
    expect(state.step).toBe("done");
    const fresh = { ...startResult(), qrPayload: '{"v":1,"s":"fresh"}' };
    state = pairAnother(state, fresh, 5_000);
    expect(state.step).toBe("done");
    expect(state.reQr).toBe(true);
    expect(state.start).toBe(fresh);
    expect(state.snapshot?.pairedDevices).toBe(1);
    expect(qrExpiryDue(state, 5_000 + 300_000)).toBe(true);
  });

  it("labels rows and progress honestly", () => {
    expect(statusLabel(null)).toBe("Waiting for your phone…");
    expect(statusLabel(snapshot({ phase: "paired" }))).toBe("Phone connected");
    expect(statusLabel(snapshot({ phase: "sending" }))).toBe("Sending…");
    expect(castRowNote(cast({ missing: true, title: "Lost" }))).toBe(
      "'Lost' is missing on disk — it was skipped.",
    );
    expect(castRowNote(cast({ sentComplete: true }))).toBe("Sent ✓");
    expect(castRowNote(cast())).toBeNull();
    expect(progressPercent(cast({ bytesSent: 500 }))).toBe(50);
    expect(progressPercent(cast({ bytesSent: 5000 }))).toBe(100);
    expect(progressPercent(cast({ bytesTotal: 0 }))).toBe(0);
    expect(safetyCode("abcdef0123456789")).toBe("abcdef01");
  });

  it("dismisses the QR once its code has been consumed by a pairing", () => {
    let state = serving(initialState(["export-1"]), startResult(), 1000);
    expect(qrVisible(state)).toBe(true);
    state = applySnapshot(state, snapshot({ phase: "waiting" }));
    expect(qrVisible(state)).toBe(true);
    state = applySnapshot(
      state,
      snapshot({ phase: "paired", pairedDevices: 1, deviceNames: ["Phone"] }),
    );
    expect(qrVisible(state)).toBe(false);
    state = applySnapshot(
      state,
      snapshot({ phase: "sending", pairedDevices: 1, deviceNames: ["Phone"] }),
    );
    expect(qrVisible(state)).toBe(false);
  });

  it("re-mints the QR when the desktop secret TTL elapses, only while a QR is showing", () => {
    let state = serving(initialState(["export-1"]), startResult(), 1000);
    expect(qrExpiryDue(state, 1000 + 299_999)).toBe(false);
    expect(qrExpiryDue(state, 1000 + 300_000)).toBe(true);
    // Consumed QR (a phone paired): the consumed notice shows, no auto re-mint.
    state = applySnapshot(
      state,
      snapshot({ phase: "paired", pairedDevices: 1, deviceNames: ["Phone"] }),
    );
    expect(qrExpiryDue(state, 1000 + 300_000)).toBe(false);
    // Pair-another restarts the window from its own mint time.
    state = pairAnother(state, startResult(), 5000);
    expect(qrExpiryDue(state, 5000 + 299_999)).toBe(false);
    expect(qrExpiryDue(state, 5000 + 300_000)).toBe(true);
    // A re-mint already in flight must not fire twice.
    expect(qrExpiryDue(starting(state), 5000 + 300_000)).toBe(false);
  });

  it("shows a freshly minted QR again after pair-another", () => {
    let state = serving(initialState(["export-1"]), startResult(), 1000);
    state = applySnapshot(
      state,
      snapshot({ phase: "paired", pairedDevices: 1, deviceNames: ["Phone"] }),
    );
    expect(qrVisible(state)).toBe(false);
    state = pairAnother(state, startResult(), 2000);
    expect(qrVisible(state)).toBe(true);
    state = applySnapshot(
      state,
      snapshot({
        phase: "paired",
        pairedDevices: 2,
        deviceNames: ["Phone", "Tablet"],
      }),
    );
    expect(qrVisible(state)).toBe(false);
  });

  it("keeps the QR hidden before a start result exists", () => {
    expect(qrVisible(initialState(["export-1"]))).toBe(false);
  });
});

describe("boundary gate: the lan send surface stays feature-neutral", () => {
  const sources = [
    "./lanSendModel.ts",
    "./SendToPhoneModal.tsx",
    "../src-tauri/src/lan_sender/proto.rs",
    "../src-tauri/src/lan_sender/server.rs",
    "../src-tauri/src/lan_sender/session.rs",
    "../src-tauri/src/lan_sender/mod.rs",
  ].map((path) => ({
    path,
    text: readSource(new URL(path, import.meta.url)),
  }));

  const forbiddenSubstrings = [
    "gateway",
    "pricing",
    "paywall",
    "premium",
    "subscription",
    "upgrade",
    "managed",
  ];

  it("contains no gateway, tier, pricing, managed, or Pro language", () => {
    for (const { path, text } of sources) {
      const lower = text.toLowerCase();
      for (const token of forbiddenSubstrings) {
        expect(lower.includes(token), `${path} contains '${token}'`).toBe(
          false,
        );
      }
      expect(/\btiers?\b/i.test(text), `${path} mentions tiers`).toBe(false);
      expect(/\bpro\b/i.test(text), `${path} mentions Pro`).toBe(false);
    }
  });
});
