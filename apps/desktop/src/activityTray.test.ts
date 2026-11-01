import { describe, expect, it } from "vitest";

import type { JobProgress, JobSummary } from "./jobModel";
import { runningJob, runningPct } from "./ActivityTray";
import { readSource } from "./tests/readSource";

const TRAY = readSource(new URL("./ActivityTray.tsx", import.meta.url));

function job(overrides: Partial<JobSummary>): JobSummary {
  return {
    id: "job-1",
    bookId: "book-1",
    scope: {
      kind: "chapters",
      chapter_ids: [],
    } as unknown as JobSummary["scope"],
    mode: "narrate",
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

function progress(overrides: Partial<JobProgress>): JobProgress {
  return {
    job_id: "job-1",
    stage: "SYNTHESIZING",
    pct: 0.43,
    eta_seconds: null,
    current_item: null,
    ...overrides,
  } as JobProgress;
}

describe("the job progress contract is a fraction, displayed as a percent", () => {
  it("renders pct 0.43 as 43, never 0.43", () => {
    expect(runningPct(job({}), progress({ pct: 0.43 }))).toBe(43);
  });

  it("covers the fraction range endpoints", () => {
    expect(runningPct(job({}), progress({ pct: 0 }))).toBe(0);
    expect(runningPct(job({}), progress({ pct: 1 }))).toBe(100);
  });

  it("clamps garbage instead of overselling", () => {
    expect(runningPct(job({}), progress({ pct: 1.7 }))).toBe(100);
    expect(runningPct(job({}), progress({ pct: -0.2 }))).toBe(0);
  });

  it("reports nothing for a different job's progress", () => {
    expect(runningPct(job({}), progress({ job_id: "other" }))).toBeNull();
    expect(runningPct(null, progress({}))).toBeNull();
    expect(runningPct(job({}), null)).toBeNull();
  });
});

describe("runningJob", () => {
  it("finds the one active job and ignores terminal or waiting states", () => {
    const jobs = [
      job({ id: "done", state: "DONE" }),
      job({ id: "queued", state: "QUEUED" }),
      job({ id: "live", state: "RENDERING" }),
    ];
    expect(runningJob(jobs)?.id).toBe("live");
    expect(runningJob([job({ state: "PAUSED" })])).toBeNull();
    expect(runningJob(null)).toBeNull();
  });
});

describe("every tray dismissal returns focus to the pill", () => {
  it("routes all three paths through the one dismiss helper", () => {
    expect(TRAY).toMatch(
      /const dismiss = useCallback\(\(\) => \{\s*onClose\(\);\s*document\s*\.querySelector<HTMLElement>\(`\[data-testid="\$\{returnFocusTestId\}"\]`\)\s*\?\.focus\(\);/,
    );
    expect(
      TRAY,
      "the pill the tray returns to is the shell's unless the Book names its own",
    ).toContain('returnFocusTestId = "nav-jobs"');
    expect(
      TRAY,
      "the shared modal stack owns Escape so the reader underneath stays open",
    ).toContain("useDialogFocus(panelRef, true, { onEscape: dismiss });");
    expect(TRAY).toContain('aria-modal="true"');
    expect(TRAY).toMatch(
      /aria-label=\{t\("activity\.close"\)\}\s*tabIndex=\{-1\}\s*onClick=\{dismiss\}/,
    );
    expect(TRAY).toMatch(
      /data-testid="close-activity"\s*aria-label=\{t\("activity\.close"\)\}\s*onClick=\{dismiss\}/,
    );
    expect(TRAY).not.toMatch(/onClick=\{onClose\}/);
  });
});

describe("ActivityTray paused-job rate-limit notice rendering", () => {
  it("renders per-minute rate-limited notice with countdown", async () => {
    const { createElement } = await import("react");
    const { renderToStaticMarkup } = await import("react-dom/server");
    const ActivityTray = (await import("./ActivityTray")).default;

    const pausedJob = job({
      id: "job-rate-pm",
      state: "PAUSED",
      checkpointJson: JSON.stringify({
        stage: "SYNTHESIZING",
        paused_reason: {
          kind: "rate_limit",
          class: "per_minute",
          retry_at_epoch_ms: Date.now() + 65_000,
          provider: "google",
          segment_idx: 12,
          last_error_message: "RESOURCE_EXHAUSTED",
          attempt: 1,
        },
      }),
    });

    const markup = renderToStaticMarkup(
      createElement(ActivityTray, {
        studio: { jobs: [pausedJob], progress: null },
        engine: null,
        onOpenProduction: () => {},
        onClose: () => {},
      }),
    );

    expect(markup).toContain('data-testid="tray-rate-limit-job-rate-pm"');
    expect(markup).toContain(
      "Your Google Gemini API key is rate-limited. Retrying in",
    );
    expect(markup).toContain("(attempt 1)");
  });

  it("renders daily quota notice with Open Settings and Cancel buttons", async () => {
    const { createElement } = await import("react");
    const { renderToStaticMarkup } = await import("react-dom/server");
    const ActivityTray = (await import("./ActivityTray")).default;

    const pausedJob = job({
      id: "job-rate-daily",
      state: "PAUSED",
      checkpointJson: JSON.stringify({
        stage: "SYNTHESIZING",
        paused_reason: {
          kind: "rate_limit",
          class: "daily",
          retry_at_epoch_ms: null,
          provider: "google",
          segment_idx: 15,
          last_error_message: "Daily quota exhausted",
          attempt: 1,
        },
      }),
    });

    const markup = renderToStaticMarkup(
      createElement(ActivityTray, {
        studio: { jobs: [pausedJob], progress: null },
        engine: null,
        onOpenProduction: () => {},
        onClose: () => {},
      }),
    );

    expect(markup).toContain('data-testid="tray-rate-limit-job-rate-daily"');
    expect(markup).toContain(
      "Your Google Gemini API key has hit its daily quota. Switch or add a key in Settings, or cancel the job.",
    );
    expect(markup).toContain('data-testid="tray-settings-job-rate-daily"');
    expect(markup).toContain('data-testid="tray-cancel-job-rate-daily"');
  });

  it("renders generic paused notice when job has no rate-limit reason", async () => {
    const { createElement } = await import("react");
    const { renderToStaticMarkup } = await import("react-dom/server");
    const ActivityTray = (await import("./ActivityTray")).default;

    const pausedJob = job({
      id: "job-manual-pause",
      state: "PAUSED",
      checkpointJson: null,
      reportJson: null,
    });

    const markup = renderToStaticMarkup(
      createElement(ActivityTray, {
        studio: { jobs: [pausedJob], progress: null },
        engine: null,
        onOpenProduction: () => {},
        onClose: () => {},
      }),
    );

    expect(markup).toContain("Paused");
    expect(markup).not.toContain("rate-limited");
    expect(markup).not.toContain("daily quota");
  });
});
