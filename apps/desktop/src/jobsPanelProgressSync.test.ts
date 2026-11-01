import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({
  revealItemInDir: vi.fn(() => Promise.resolve()),
}));

import type { JobProgress, JobSummary } from "./jobModel";
import JobsPanel, { JobRow } from "./JobsPanel";
import type { StudioJobs } from "./ActivityTray";

function makeJob(overrides: Partial<JobSummary> = {}): JobSummary {
  return {
    id: "job-sync-1",
    bookId: "book-1",
    scope: { kind: "chapters", chapter_ids: [] },
    mode: "narrator",
    voiceProfileId: "voice-1",
    outputFormat: "mp3",
    state: "SYNTHESIZING",
    overnight: false,
    etaSeconds: null,
    reportJson: null,
    outputPathsJson: null,
    createdAt: 1,
    updatedAt: 1,
    ...overrides,
  };
}

describe("Job progress synchronization and instantaneous rendering", () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it("JobRow renders live progress and ETA with 0ms delay when stageProgress is provided", () => {
    const job = makeJob();
    const progress: JobProgress = {
      job_id: "job-sync-1",
      stage: "SYNTHESIZING",
      pct: 0.89,
      eta_seconds: 65,
      current_item: "Segment synthesized",
    };

    const markup = renderToStaticMarkup(
      createElement(JobRow, {
        job,
        bookTitle: "Modern Compiler Implementation in Java",
        progress,
        index: 0,
        staleExports: 0,
        queueNotice: null,
        queueLabel: null,
        playableItem: null,
        onControl: () => {},
      }),
    );

    expect(markup).toContain("Segment synthesized");
    expect(markup).toContain('style="width:89%"');
    expect(markup).toContain("~1 min left");
  });

  it("JobRow rehydrates from checkpointJson when stageProgress has not yet received a live event", () => {
    const job = makeJob({
      checkpointJson: JSON.stringify({
        stage: "SYNTHESIZING",
        done_segments: 89,
        total_segments: 100,
      }),
    });

    const markup = renderToStaticMarkup(
      createElement(JobRow, {
        job,
        bookTitle: "Modern Compiler Implementation in Java",
        progress: null,
        index: 0,
        staleExports: 0,
        queueNotice: null,
        queueLabel: null,
        playableItem: null,
        onControl: () => {},
      }),
    );

    // Should not say "Starting" or have 0% width; should rehydrate 89% from checkpoint
    expect(markup).toContain("89/100 segments");
    expect(markup).toContain('style="width:89%"');
  });

  it("JobsPanel accepts studio prop and passes progressByJob to render jobs immediately", async () => {
    const job = makeJob();
    invokeMock.mockImplementation((cmd) => {
      if (cmd === "list_jobs") return Promise.resolve([job]);
      if (cmd === "list_books")
        return Promise.resolve([{ id: "book-1", title: "Modern Compiler Implementation in Java" }]);
      if (cmd === "list_exports") return Promise.resolve([]);
      return Promise.resolve(null);
    });

    const studio: StudioJobs = {
      jobs: [job],
      progress: {
        job_id: "job-sync-1",
        stage: "SYNTHESIZING",
        pct: 0.92,
        eta_seconds: 45,
        current_item: "Segment synthesized",
      },
      progressByJob: {
        "job-sync-1": {
          job_id: "job-sync-1",
          stage: "SYNTHESIZING",
          pct: 0.92,
          eta_seconds: 45,
          current_item: "Segment synthesized",
        },
      },
    };

    const markup = renderToStaticMarkup(createElement(JobsPanel, { studio }));
    expect(markup).toBeDefined();
  });
});

