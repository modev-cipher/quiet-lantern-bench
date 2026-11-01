import { invoke } from "@tauri-apps/api/core";

import {
  createRatedEmitter,
  type EmitterPolicy,
  type RatedEmitter,
} from "./diagnosticRatePolicy";

const NOT_AVAILABLE = "n/a";

const CRASH_POLICY: EmitterPolicy = {
  minIntervalMs: 2_000,
  heartbeatEvery: 15,
};

export interface UiCrashPayload {
  name: string;
  message: string;
  frame: string;
  detail: string;
}

export function uiCrashPayload(error: unknown): UiCrashPayload {
  if (!(error instanceof Error)) {
    const message = String(error);
    return {
      name: typeof error,
      message,
      frame: NOT_AVAILABLE,
      detail: message,
    };
  }
  const stack = error.stack ?? "";
  const lines = stack.split("\n");
  const frame = lines.find((line) => line.trimStart().startsWith("at "));
  return {
    name: error.name || "Error",
    message: error.message,
    frame: frame == null ? NOT_AVAILABLE : frame.trim(),
    detail: stack === "" ? error.message : stack,
  };
}

function collapseVolatile(text: string): string {
  return text.replace(/\d+/g, "#");
}

function crashKey(payload: UiCrashPayload): string {
  return [payload.name, collapseVolatile(payload.message), payload.frame].join(
    " | ",
  );
}

function crashLine(payload: UiCrashPayload, suppressedRun: number): string {
  const suppressed = suppressedRun > 0 ? ` suppressed=${suppressedRun}` : "";
  return `${payload.detail}${suppressed}`;
}

async function sendDiagnosticLine(line: string): Promise<void> {
  await invoke("log_ui_error", { line });
}

const CRASH_EMITTER: RatedEmitter<UiCrashPayload> =
  createRatedEmitter<UiCrashPayload>({
    policy: CRASH_POLICY,
    keyOf: crashKey,
    format: crashLine,
    send: sendDiagnosticLine,
  });

export function resetUiCrashEmitter(): void {
  CRASH_EMITTER.reset();
}

export async function reportUiCrash(error: unknown): Promise<void> {
  await CRASH_EMITTER.report(uiCrashPayload(error));
}
