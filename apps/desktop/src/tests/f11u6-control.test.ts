// F11-U6 C5 pin: a confirmed cast delete invalidates
// the book's history scope; other controls and refused deletes do not.
import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

import type { JobSummary } from "../jobModel";
import { runJobControl } from "../jobsPanelControl";
import { getHistorySnapshot, loadHistory } from "../sessionHistoryStore";

const job = {
  id: "job-1",
  bookId: "book-1",
  scope: { kind: "chapters", chapter_ids: [] },
  mode: "narrator",
  voiceProfileId: "v",
  outputFormat: "mp3",
  state: "DONE",
  overnight: false,
  etaSeconds: null,
  reportJson: null,
  outputPathsJson: null,
  createdAt: 1,
  updatedAt: 1,
} as JobSummary;
const SCOPE = { bookId: "book-1" };

async function readyScope(): Promise<void> {
  invoke.mockImplementation((cmd) =>
    cmd === "voice_session_list" ? Promise.resolve([]) : Promise.resolve(null),
  );
  await loadHistory(SCOPE);
  expect(getHistorySnapshot(SCOPE).status).toBe("ready");
}

describe("C5 — a confirmed delete_job invalidates the book's history scope", () => {
  beforeEach(() => invoke.mockReset());

  it("delete_job → scope back to idle (a mounted rail re-lists)", async () => {
    await readyScope();
    await runJobControl("delete_job", "job-1", [job]);
    expect(invoke).toHaveBeenLastCalledWith("delete_job", { jobId: "job-1" });
    expect(getHistorySnapshot(SCOPE).status).toBe("idle");
  });

  it("cancel_job / pause_job leave the scope alone", async () => {
    await readyScope();
    await runJobControl("cancel_job", "job-1", [job]);
    await runJobControl("pause_job", "job-1", [job]);
    expect(getHistorySnapshot(SCOPE).status).toBe("ready");
  });

  it("a refused delete (live cast) invalidates nothing and rejects", async () => {
    await readyScope();
    invoke.mockImplementation((cmd) =>
      cmd === "delete_job"
        ? Promise.reject(
            new Error("cannot delete a cast with a live tutor session"),
          )
        : Promise.resolve([]),
    );
    await expect(runJobControl("delete_job", "job-1", [job])).rejects.toThrow(
      /live tutor session/,
    );
    expect(getHistorySnapshot(SCOPE).status).toBe("ready");
  });

  it("an unknown job id deletes nothing in history (no scope to invalidate)", async () => {
    await readyScope();
    await runJobControl("delete_job", "job-zzz", [job]);
    expect(getHistorySnapshot(SCOPE).status).toBe("ready");
  });
});
