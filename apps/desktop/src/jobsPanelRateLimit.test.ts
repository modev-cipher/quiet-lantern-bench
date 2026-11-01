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

import {
  checkpointProgress,
  failureMessage,
  sanitizeAndRouteErrorMessage,
  type JobSummary,
  type PausedReason,
} from "./jobModel";
import { RateLimitPauseCard } from "./JobsPanel";

function makeJob(overrides: Partial<JobSummary> = {}): JobSummary {
  return {
    id: "job-rl-1",
    bookId: "book-1",
    scope: { kind: "chapters", chapter_ids: [] },
    mode: "narrator",
    voiceProfileId: "voice-1",
    outputFormat: "mp3",
    state: "PAUSED",
    overnight: false,
    etaSeconds: null,
    reportJson: null,
    outputPathsJson: null,
    createdAt: 1,
    updatedAt: 1,
    ...overrides,
  };
}

describe("RateLimitPauseCard", () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it("renders per-minute rate-limit card with status role, countdown, and attempt", () => {
    const job = makeJob();
    const reason: PausedReason = {
      kind: "rate_limit",
      class: "per_minute",
      retry_at_epoch_ms: Date.now() + 75_000,
      provider: "google",
      segment_idx: 4,
      last_error_message: "429 RESOURCE_EXHAUSTED",
      attempt: 2,
    };

    const markup = renderToStaticMarkup(
      createElement(RateLimitPauseCard, {
        job,
        reason,
      }),
    );

    expect(markup).toContain('data-testid="job-rate-limit-card-job-rl-1"');
    expect(markup).toContain('role="status"');
    expect(markup).toContain(
      "Your Google Gemini API key is rate-limited. Retrying in",
    );
    expect(markup).toContain("(attempt 2)");
    expect(markup).not.toContain("daily quota");
    expect(markup).not.toContain(
      'data-testid="job-rate-limit-settings-job-rl-1"',
    );
  });

  it("renders daily quota card with alert role, settings button, and cancel button", () => {
    const job = makeJob();
    const reason: PausedReason = {
      kind: "rate_limit",
      class: "daily",
      retry_at_epoch_ms: null,
      provider: "google",
      segment_idx: 10,
      last_error_message: "Daily limit reached",
      attempt: 1,
    };

    const markup = renderToStaticMarkup(
      createElement(RateLimitPauseCard, {
        job,
        reason,
        onControl: () => {},
      }),
    );

    expect(markup).toContain('data-testid="job-rate-limit-card-job-rl-1"');
    expect(markup).toContain('role="alert"');
    expect(markup).toContain(
      "Your Google Gemini API key has hit its daily quota. Switch or add a key in Settings, or cancel the job.",
    );
    expect(markup).toContain('data-testid="job-rate-limit-settings-job-rl-1"');
    expect(markup).toContain('data-testid="job-rate-limit-cancel-job-rl-1"');
    expect(markup).toContain("Open Settings");
    expect(markup).toContain("Cancel");
    expect(
      markup,
      "a daily pause never auto-resumes, so an overnight switch would promise a resume that never comes",
    ).not.toContain('data-testid="job-rate-limit-overnight-job-rl-1"');
  });

  it("renders switch to overnight button on per-minute card when not overnight", () => {
    const job = makeJob({ overnight: false });
    const reason: PausedReason = {
      kind: "rate_limit",
      class: "per_minute",
      retry_at_epoch_ms: Date.now() + 60_000,
      provider: "google",
      segment_idx: 1,
      last_error_message: "429",
      attempt: 1,
    };
    const markup = renderToStaticMarkup(
      createElement(RateLimitPauseCard, { job, reason }),
    );
    expect(markup).toContain('data-testid="job-rate-limit-overnight-job-rl-1"');
    expect(markup).toContain("Switch to Overnight");
  });

  it("does not render switch to overnight button when job is already overnight", () => {
    const job = makeJob({ overnight: true });
    const reason: PausedReason = {
      kind: "rate_limit",
      class: "per_minute",
      retry_at_epoch_ms: Date.now() + 60_000,
      provider: "google",
      segment_idx: 1,
      last_error_message: "429",
      attempt: 1,
    };
    const markup = renderToStaticMarkup(
      createElement(RateLimitPauseCard, { job, reason }),
    );
    expect(markup).not.toContain(
      'data-testid="job-rate-limit-overnight-job-rl-1"',
    );
  });

  it("cancel control invokes cancel_job on backend", async () => {
    const { runJobControl } = await import("./jobsPanelControl");
    const job = makeJob();
    await runJobControl("cancel_job", job.id, [job]);
    expect(invokeMock).toHaveBeenCalledWith("cancel_job", { jobId: job.id });
  });

  it("switch to overnight control invokes switch_job_to_overnight on backend", async () => {
    const { runJobControl } = await import("./jobsPanelControl");
    const job = makeJob();
    await runJobControl("switch_job_to_overnight", job.id, [job]);
    expect(invokeMock).toHaveBeenCalledWith("switch_job_to_overnight", {
      jobId: job.id,
    });
  });
});

describe("JobRow integration with rate-limited paused state", () => {
  it("renders manual resume button, rail, checkpoint notice, and rate-limit card when paused", async () => {
    const { JobRow } = await import("./JobsPanel");
    const job = makeJob({
      id: "job-paused-row",
      state: "PAUSED",
      checkpointJson: JSON.stringify({
        stage: "SYNTHESIZING",
        paused_reason: {
          kind: "rate_limit",
          class: "per_minute",
          retry_at_epoch_ms: Date.now() + 60_000,
          provider: "google",
          segment_idx: 8,
          last_error_message: "429",
          attempt: 1,
        },
      }),
    });

    const markup = renderToStaticMarkup(
      createElement(JobRow, {
        job,
        bookTitle: "Test Book",
        progress: null,
        index: 0,
        staleExports: 0,
        queueNotice: null,
        queueLabel: null,
        playableItem: null,
        onControl: () => {},
      }),
    );

    // Manual Resume button remains available
    expect(markup).toContain('data-testid="resume-job-job-paused-row"');
    // Cancel button in header is also available
    expect(markup).toContain('data-testid="cancel-job-job-paused-row"');
    // Checkpointed note is rendered
    expect(markup).toContain(
      'data-testid="job-checkpoint-note-job-paused-row"',
    );
    // Rate limit card is rendered
    expect(markup).toContain(
      'data-testid="job-rate-limit-card-job-paused-row"',
    );
    expect(markup).toContain("Your Google Gemini API key is rate-limited.");
  });

  it("renders daily quota card in JobRow with Open Settings and Cancel buttons", async () => {
    const { JobRow } = await import("./JobsPanel");
    const job = makeJob({
      id: "job-daily-row",
      state: "PAUSED",
      checkpointJson: JSON.stringify({
        stage: "SYNTHESIZING",
        paused_reason: {
          kind: "rate_limit",
          class: "daily",
          retry_at_epoch_ms: null,
          provider: "google",
          segment_idx: 20,
          last_error_message: "Resource exhausted",
          attempt: 1,
        },
      }),
    });

    const markup = renderToStaticMarkup(
      createElement(JobRow, {
        job,
        bookTitle: "Test Book Daily",
        progress: null,
        index: 0,
        staleExports: 0,
        queueNotice: null,
        queueLabel: null,
        playableItem: null,
        onControl: () => {},
      }),
    );

    expect(markup).toContain('data-testid="resume-job-job-daily-row"');
    expect(markup).toContain('data-testid="job-rate-limit-card-job-daily-row"');
    expect(markup).toContain(
      'data-testid="job-rate-limit-settings-job-daily-row"',
    );
    expect(markup).toContain(
      'data-testid="job-rate-limit-cancel-job-daily-row"',
    );
  });
});

describe("Calm UX Contract and Checkpoint Progress", () => {
  it("extracts checkpoint progress from checkpointJson or reportJson", () => {
    const jobWithCheckpoint = makeJob({
      checkpointJson: JSON.stringify({ done_segments: 276, total_segments: 289 }),
    });
    expect(checkpointProgress(jobWithCheckpoint)).toEqual({ done: 276, total: 289 });

    const jobWithReport = makeJob({
      checkpointJson: null,
      reportJson: JSON.stringify({ done_segments: 10, total_segments: 50 }),
    });
    expect(checkpointProgress(jobWithReport)).toEqual({ done: 10, total: 50 });

    const jobWithoutProgress = makeJob({ checkpointJson: null, reportJson: null });
    expect(checkpointProgress(jobWithoutProgress)).toBeNull();
  });

  it("renders Calm UX contract elements: headline, saved progress, live countdown notice, and resume button", () => {
    const job = makeJob({
      checkpointJson: JSON.stringify({ done_segments: 276, total_segments: 289 }),
    });
    const reason: PausedReason = {
      kind: "rate_limited",
      class: "per_minute",
      retry_at_epoch_ms: Date.now() + 50_000,
      provider: "google",
      segment_idx: 276,
      last_error_message: "Rate limit cooldown",
      attempt: 1,
    };

    const markup = renderToStaticMarkup(
      createElement(RateLimitPauseCard, {
        job,
        reason,
        onControl: () => {},
      }),
    );

    expect(markup).toContain("Taking a short break");
    expect(markup).toContain("Progress saved: 276/289 segments.");
    expect(markup).toContain("Resuming automatically in");
    expect(markup).toContain("You don&#x27;t need to do anything.");
    expect(markup).toContain('data-testid="job-rate-limit-resume-job-rl-1"');
    expect(markup).toContain("Resume now");
  });

  it("renders Waiting on the provider and Try now for daily quota card", () => {
    const job = makeJob({
      checkpointJson: JSON.stringify({ done_segments: 15, total_segments: 30 }),
    });
    const reason: PausedReason = {
      kind: "rate_limited",
      class: "daily",
      retry_at_epoch_ms: null,
      provider: "google",
      segment_idx: 15,
      last_error_message: "Quota exceeded",
      attempt: 1,
    };

    const markup = renderToStaticMarkup(
      createElement(RateLimitPauseCard, {
        job,
        reason,
        onControl: () => {},
      }),
    );

    expect(markup).toContain("Waiting on the provider");
    expect(markup).toContain("Progress saved: 15/30 segments.");
    expect(markup).toContain('data-testid="job-rate-limit-try-now-job-rl-1"');
    expect(markup).toContain("Try now");
  });
});

describe("Error routing and sanitization", () => {
  it("intercepts ERR_PROVIDER_KEY and raw key error strings", () => {
    expect(
      sanitizeAndRouteErrorMessage(
        "ERR_PROVIDER_KEY: no usable key for connection 'gemini': NoUsableKey { stored: 44 }",
      ),
    ).toBe("The provider rejected your key. Check it in Settings, then retry.");

    expect(
      sanitizeAndRouteErrorMessage("all 44 configured keys for 'gemini' are invalid"),
    ).toBe("The provider rejected your key. Check it in Settings, then retry.");
  });

  it("intercepts ERR_PROVIDER_QUOTA and quota strings", () => {
    expect(
      sanitizeAndRouteErrorMessage(
        "ERR_PROVIDER_QUOTA: rate limit pause attempts exceeded limit (5)",
      ),
    ).toBe("The provider reports your quota or spend limit is reached.");

    expect(sanitizeAndRouteErrorMessage("429 RESOURCE_EXHAUSTED")).toBe(
      "The provider reports your quota or spend limit is reached.",
    );
  });

  it("intercepts timeout, transport, and model errors", () => {
    expect(sanitizeAndRouteErrorMessage("ERR_PROVIDER_TIMEOUT")).toBe(
      "The provider took too long to answer. Retrying often clears this.",
    );
    expect(sanitizeAndRouteErrorMessage("ERR_PROVIDER_TRANSPORT")).toBe(
      "The provider could not be reached. Check your connection, then retry.",
    );
    expect(sanitizeAndRouteErrorMessage("ERR_PROVIDER_MODEL")).toBe(
      "The provider does not recognize this model. Re-select the model, or switch to local.",
    );
  });

  it("formats structured provider_failure from reportJson via failureMessage", () => {
    const report = JSON.stringify({
      provider_failure: {
        class: "quota",
        retryable: true,
        retry_after_secs: 60,
        detail: "quota cooldown",
      },
    });
    expect(failureMessage(report)).toBe(
      "The provider rate-limited this cast. It asked for a 60s wait before retrying.",
    );
  });

  it("routes raw error in reportJson through sanitizeAndRouteErrorMessage", () => {
    const report = JSON.stringify({
      error: "ERR_PROVIDER_KEY: no usable key for connection 'test'",
    });
    expect(failureMessage(report)).toBe(
      "The provider rejected your key. Check it in Settings, then retry.",
    );
  });
});
