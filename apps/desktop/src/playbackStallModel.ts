export const STALL_LIMIT_TICKS = 15;

export interface StallState {
  lastTime: number;
  heldTicks: number;
}

export const NO_STALL: StallState = { lastTime: Number.NaN, heldTicks: 0 };

export function nextStall(state: StallState, time: number): StallState {
  return time === state.lastTime
    ? { lastTime: time, heldTicks: state.heldTicks + 1 }
    : { lastTime: time, heldTicks: 0 };
}

export function stalled(state: StallState): boolean {
  return state.heldTicks >= STALL_LIMIT_TICKS;
}
