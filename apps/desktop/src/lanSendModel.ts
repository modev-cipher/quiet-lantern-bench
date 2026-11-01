import type { MessageDescriptor } from "./i18n";

export interface LanCastProgress {
  castId: string;
  title: string;
  bytesSent: number;
  bytesTotal: number;
  sentComplete: boolean;
  missing: boolean;
}

export interface LanSendSnapshot {
  phase: string;
  pairedDevices: number;
  deviceNames: string[];
  pairLocked: boolean;
  acceptedConnections: number;
  casts: LanCastProgress[];
}

export interface LanSendStartResult {
  qrSvg: string;
  qrPayload: string;
  fingerprint: string;
  hosts: string[];
  port: number;
  /** Desktop-enforced lifetime of the secret inside this QR (ms). */
  secretTtlMs: number;
}

export type LanSendStep = "pick" | "serving" | "done" | "error";

export interface LanSendState {
  step: LanSendStep;
  selected: string[];
  busy: boolean;
  start: LanSendStartResult | null;
  snapshot: LanSendSnapshot | null;
  error: string | null;
  confirmingStop: boolean;
  servingSince: number | null;
  reQr: boolean;
  pairedAtQr: number;
}

const FIREWALL_HINT_AFTER_MS = 60_000;

export function initialState(audioIds: string[]): LanSendState {
  return {
    step: "pick",
    selected: [...audioIds],
    busy: false,
    start: null,
    snapshot: null,
    error: null,
    confirmingStop: false,
    servingSince: null,
    reQr: false,
    pairedAtQr: 0,
  };
}

export function toggleSelected(state: LanSendState, id: string): LanSendState {
  const selected = state.selected.includes(id)
    ? state.selected.filter((entry) => entry !== id)
    : [...state.selected, id];
  return { ...state, selected };
}

export function canShowQr(state: LanSendState): boolean {
  return state.step === "pick" && state.selected.length > 0 && !state.busy;
}

export function starting(state: LanSendState): LanSendState {
  return { ...state, busy: true, error: null };
}

export function serving(
  state: LanSendState,
  start: LanSendStartResult,
  now: number,
): LanSendState {
  return {
    ...state,
    step: "serving",
    start,
    busy: false,
    error: null,
    snapshot: null,
    confirmingStop: false,
    servingSince: now,
    reQr: false,
    pairedAtQr: 0,
  };
}

// Keeps the current step: pairing another phone from the done step must not
// bounce through "serving", where the next done-phased snapshot would hide
// the freshly minted QR before anyone could scan it.
export function pairAnother(
  state: LanSendState,
  start: LanSendStartResult,
  now: number,
): LanSendState {
  return {
    ...state,
    start,
    busy: false,
    error: null,
    confirmingStop: false,
    servingSince: now,
    reQr: true,
    pairedAtQr: state.snapshot?.pairedDevices ?? 0,
  };
}

export function qrVisible(state: LanSendState): boolean {
  if (state.start == null) return false;
  return (state.snapshot?.pairedDevices ?? 0) <= state.pairedAtQr;
}

// The desktop refuses the secret once PAIR_SECRET_TTL has elapsed on its own
// clock; the phone is never the authority. While a scannable QR is on screen
// we re-mint at that moment so the screen never shows a dead code. Consumed
// QRs (a phone already paired) keep the manual "New QR code" path instead.
export function qrExpiryDue(state: LanSendState, now: number): boolean {
  if (state.busy || state.start == null) return false;
  if (!qrVisible(state) || state.servingSince == null) return false;
  const ttl = state.start.secretTtlMs;
  return ttl > 0 && now - state.servingSince >= ttl;
}

export function failed(state: LanSendState, raw: string): LanSendState {
  return { ...state, step: "error", busy: false, error: raw };
}

export function plainError(raw: string): MessageDescriptor {
  if (raw.includes("no lan address")) return { key: "lan.noNetwork" };
  if (raw.includes("could not open a port")) return { key: "lan.portBlocked" };
  return { key: "lan.failed", params: { reason: raw } };
}

export function applySnapshot(
  state: LanSendState,
  snapshot: LanSendSnapshot,
): LanSendState {
  if (state.step !== "serving") return state;
  if (snapshot.phase === "done") {
    return { ...state, snapshot, step: "done", confirmingStop: false };
  }
  return { ...state, snapshot };
}

export function statusLabel(
  snapshot: LanSendSnapshot | null,
): MessageDescriptor {
  if (snapshot == null || snapshot.phase === "waiting")
    return { key: "lan.waiting" };
  if (snapshot.phase === "paired") return { key: "lan.paired" };
  if (snapshot.phase === "sending") return { key: "lan.sending" };
  return { key: "lan.allSent" };
}

export function firewallHintDue(state: LanSendState, now: number): boolean {
  return (
    state.step === "serving" &&
    state.servingSince != null &&
    now - state.servingSince >= FIREWALL_HINT_AFTER_MS &&
    (state.snapshot?.acceptedConnections ?? 0) === 0
  );
}

function allSent(snapshot: LanSendSnapshot): boolean {
  return snapshot.casts.every((cast) => cast.sentComplete || cast.missing);
}

export function transferActive(state: LanSendState): boolean {
  return (
    state.step === "serving" &&
    state.snapshot != null &&
    state.snapshot.pairedDevices > 0 &&
    !allSent(state.snapshot)
  );
}

export function needsStopConfirm(state: LanSendState): boolean {
  return transferActive(state) && !state.confirmingStop;
}

export function requestStop(state: LanSendState): LanSendState {
  return { ...state, confirmingStop: true };
}

export function keepSending(state: LanSendState): LanSendState {
  return { ...state, confirmingStop: false };
}

export function sentCount(snapshot: LanSendSnapshot | null): number {
  if (snapshot == null) return 0;
  return snapshot.casts.filter((cast) => cast.sentComplete).length;
}

export function doneMessage(count: number): MessageDescriptor {
  return { key: "lan.done", params: { count } };
}

export function castRowNote(cast: LanCastProgress): MessageDescriptor | null {
  if (cast.missing)
    return { key: "lan.missing", params: { title: cast.title } };
  if (cast.sentComplete) return { key: "lan.sent" };
  return null;
}

export function progressPercent(cast: LanCastProgress): number {
  if (cast.bytesTotal <= 0) return cast.sentComplete ? 100 : 0;
  return Math.min(100, Math.round((cast.bytesSent / cast.bytesTotal) * 100));
}

export function safetyCode(fingerprint: string): string {
  return fingerprint.slice(0, 8);
}
