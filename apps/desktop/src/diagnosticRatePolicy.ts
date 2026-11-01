export interface EmitterPolicy {
  minIntervalMs: number;
  heartbeatEvery: number;
}

export interface EmitterState {
  lastKey: string | null;
  lastEmitAtMs: number;
  suppressed: number;
}

export const INITIAL_EMITTER_STATE: EmitterState = {
  lastKey: null,
  lastEmitAtMs: Number.NEGATIVE_INFINITY,
  suppressed: 0,
};

export interface RateDecision {
  emit: boolean;
  heartbeat: boolean;
  suppressedRun: number;
  next: EmitterState;
}

export function decideRated(
  policy: EmitterPolicy,
  state: EmitterState,
  key: string,
  suppressibleRepeat: boolean,
  nowMs: number,
): RateDecision {
  const repeat = key === state.lastKey && suppressibleRepeat;
  const windowOpen = nowMs - state.lastEmitAtMs < policy.minIntervalMs;
  if (!repeat || !windowOpen) {
    return {
      emit: true,
      heartbeat: false,
      suppressedRun: state.suppressed,
      next: { lastKey: key, lastEmitAtMs: nowMs, suppressed: 0 },
    };
  }
  const suppressed = state.suppressed + 1;
  if (suppressed % policy.heartbeatEvery === 0) {
    return {
      emit: true,
      heartbeat: true,
      suppressedRun: suppressed,
      next: { lastKey: key, lastEmitAtMs: nowMs, suppressed },
    };
  }
  return {
    emit: false,
    heartbeat: false,
    suppressedRun: suppressed,
    next: {
      lastKey: state.lastKey,
      lastEmitAtMs: state.lastEmitAtMs,
      suppressed,
    },
  };
}

export interface RatedEmitter<P> {
  report(payload: P): Promise<void>;
  reset(): void;
}

export function createRatedEmitter<P>(config: {
  policy: EmitterPolicy;
  keyOf: (payload: P) => string;
  suppressibleRepeat?: (payload: P) => boolean;
  format: (payload: P, suppressedRun: number) => string;
  send: (line: string) => Promise<void>;
  now?: () => number;
}): RatedEmitter<P> {
  let state = INITIAL_EMITTER_STATE;
  const now = config.now ?? (() => Date.now());
  const suppressible = config.suppressibleRepeat ?? (() => true);
  return {
    async report(payload: P): Promise<void> {
      const decision = decideRated(
        config.policy,
        state,
        config.keyOf(payload),
        suppressible(payload),
        now(),
      );
      state = decision.next;
      if (!decision.emit) return;
      await config.send(config.format(payload, decision.suppressedRun));
    },
    reset(): void {
      state = INITIAL_EMITTER_STATE;
    },
  };
}
