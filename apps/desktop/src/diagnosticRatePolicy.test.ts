import { describe, expect, it } from "vitest";
import {
  createRatedEmitter,
  decideRated,
  INITIAL_EMITTER_STATE,
  type EmitterPolicy,
  type EmitterState,
} from "./diagnosticRatePolicy";

const POLICY: EmitterPolicy = { minIntervalMs: 2_000, heartbeatEvery: 15 };

describe("decideRated", () => {
  it("emits the first event and a key change immediately", () => {
    const first = decideRated(POLICY, INITIAL_EMITTER_STATE, "a", true, 0);
    expect(first.emit).toBe(true);
    const changed = decideRated(POLICY, first.next, "b", true, 100);
    expect(changed.emit).toBe(true);
  });

  it("suppresses an exact repeat inside the window and carries the count out", () => {
    let state: EmitterState = INITIAL_EMITTER_STATE;
    state = decideRated(POLICY, state, "a", true, 0).next;
    for (let step = 1; step <= 5; step += 1) {
      const decision = decideRated(POLICY, state, "a", true, step * 100);
      expect(decision.emit).toBe(false);
      state = decision.next;
    }
    const flush = decideRated(POLICY, state, "a", true, 3_000);
    expect(flush.emit).toBe(true);
    expect(flush.suppressedRun).toBe(5);
    expect(flush.next.suppressed).toBe(0);
  });

  it("a non-suppressible repeat emits even inside the window", () => {
    let state: EmitterState = INITIAL_EMITTER_STATE;
    state = decideRated(POLICY, state, "a", true, 0).next;
    const decision = decideRated(POLICY, state, "a", false, 100);
    expect(decision.emit).toBe(true);
  });

  it("a long run heartbeats every N suppressed instead of going silent", () => {
    let state: EmitterState = INITIAL_EMITTER_STATE;
    state = decideRated(POLICY, state, "a", true, 0).next;
    let heartbeats = 0;
    for (let step = 1; step <= 30; step += 1) {
      const decision = decideRated(POLICY, state, "a", true, step);
      if (decision.emit) {
        heartbeats += 1;
        expect(decision.heartbeat).toBe(true);
        expect(decision.suppressedRun % POLICY.heartbeatEvery).toBe(0);
      }
      state = decision.next;
    }
    expect(heartbeats).toBe(2);
  });
});

describe("scrub-gesture replay at the reported log cadence", () => {
  it("collapses a 5-second 240-event pause flood to a bounded handful of lines", async () => {
    const sent: string[] = [];
    let clock = 0;
    const emitter = createRatedEmitter<{ at: number }>({
      policy: POLICY,
      keyOf: () => "pause",
      format: (payload, run) => `pause@${payload.at} suppressed=${run}`,
      send: async (line) => {
        sent.push(line);
      },
      now: () => clock,
    });
    for (let step = 0; step < 240; step += 1) {
      clock = step * 21;
      await emitter.report({ at: clock });
    }
    expect(
      sent.length,
      "the reported flood was one line per pointer step; the policy must bound a 240-step gesture",
    ).toBeLessThanOrEqual(20);
    expect(sent.length).toBeGreaterThan(0);
    const runs = sent.map((line) =>
      Number(/suppressed=(\d+)/.exec(line)?.[1] ?? "0"),
    );
    expect(
      Math.max(...runs),
      "the suppressed count must surface so a reader can see the size of the collapsed burst",
    ).toBeGreaterThan(0);
  });
});
