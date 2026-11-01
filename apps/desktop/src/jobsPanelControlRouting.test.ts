// @vitest-environment jsdom
import { createElement } from "react";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

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

import { RateLimitPauseCard } from "./JobsPanel";
import type { JobSummary, PausedReason } from "./jobModel";

const REFUSAL = "the worker is shutting down, so this cast was not switched";
const JOB_ID = "job-rl-1";

function job(overrides: Partial<JobSummary> = {}): JobSummary {
  return {
    id: JOB_ID,
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

function reason(kind: PausedReason["class"]): PausedReason {
  return {
    kind: "rate_limit",
    class: kind,
    retry_at_epoch_ms: kind === "per_minute" ? Date.now() + 60_000 : null,
    provider: "google",
    segment_idx: 4,
    last_error_message: "429 RESOURCE_EXHAUSTED",
    attempt: 2,
  };
}

function refusing() {
  return vi
    .fn<(command: string, jobId: string) => Promise<void>>()
    .mockRejectedValue(new Error(REFUSAL));
}

function accepting() {
  return vi
    .fn<(command: string, jobId: string) => Promise<void>>()
    .mockResolvedValue(undefined);
}

function showCard(
  kind: PausedReason["class"],
  onControl: (command: string, jobId: string) => void | Promise<void>,
) {
  return render(
    createElement(RateLimitPauseCard, {
      job: job(),
      reason: reason(kind),
      onControl,
    }),
  );
}

function refusalNode(): HTMLElement | null {
  return screen.queryByTestId(`job-rate-limit-error-${JOB_ID}`);
}

async function press(
  testId: string,
  onControl: (command: string, jobId: string) => void | Promise<void>,
): Promise<void> {
  fireEvent.click(screen.getByTestId(testId));
  await waitFor(() => {
    expect(
      onControl,
      `[data-testid="${testId}"] never called onControl`,
    ).toHaveBeenCalled();
  });
}

async function expectRefusalReached(testId: string): Promise<void> {
  await waitFor(() => {
    expect(
      refusalNode(),
      `[data-testid="${testId}"] was refused and the card shows the user nothing. A refusal that \
reaches no rendered node is a button that looks broken`,
    ).not.toBeNull();
  });
  expect(refusalNode()?.textContent).toContain(REFUSAL);
}

afterEach(() => {
  cleanup();
});

describe("a refused control on the per-minute card reaches the user", () => {
  beforeEach(() => invokeMock.mockReset().mockResolvedValue(null));

  it("shows the refusal after the overnight button is refused", async () => {
    const onControl = refusing();
    showCard("per_minute", onControl);

    expect(
      refusalNode(),
      "no refusal has happened yet, so the card must not pre-render one",
    ).toBeNull();

    const testId = `job-rate-limit-overnight-${JOB_ID}`;
    await press(testId, onControl);
    await expectRefusalReached(testId);

    expect(onControl).toHaveBeenCalledWith("switch_job_to_overnight", JOB_ID);
  });

  it("shows the refusal after resume-now is refused", async () => {
    const onControl = refusing();
    showCard("per_minute", onControl);

    const testId = `job-rate-limit-resume-${JOB_ID}`;
    await press(testId, onControl);
    await expectRefusalReached(testId);

    expect(onControl).toHaveBeenCalledWith("resume_job", JOB_ID);
  });

  it("renders exactly one alert, and it is not inside the status card", async () => {
    const onControl = refusing();
    showCard("per_minute", onControl);

    await press(`job-rate-limit-overnight-${JOB_ID}`, onControl);
    await expectRefusalReached(`job-rate-limit-overnight-${JOB_ID}`);

    const alerts = screen.queryAllByRole("alert");
    expect(
      alerts,
      "a second reachable alert makes getByRole('alert') ambiguous between the refusal and the \
card, which breaks every spec that selects the alert by role",
    ).toHaveLength(1);
    expect(alerts[0]?.textContent).toContain(REFUSAL);
    expect(
      screen
        .getByTestId(`job-rate-limit-card-${JOB_ID}`)
        .querySelectorAll('[role="alert"]'),
      "the alert must be a sibling of the role='status' card, not a descendant: nesting an alert \
inside a live-region status node is exactly the ambiguity this markup has to avoid",
    ).toHaveLength(0);
  });

  it("stays silent when the backend accepts the switch", async () => {
    const onControl = accepting();
    showCard("per_minute", onControl);

    await press(`job-rate-limit-overnight-${JOB_ID}`, onControl);

    expect(onControl).toHaveBeenCalledWith("switch_job_to_overnight", JOB_ID);
    expect(refusalNode()).toBeNull();
    expect(screen.queryAllByRole("alert")).toHaveLength(0);
  });
});

describe("the daily card still routes its own refusal", () => {
  beforeEach(() => invokeMock.mockReset().mockResolvedValue(null));

  it("shows the refusal on the daily branch", async () => {
    const onControl = refusing();
    showCard("daily", onControl);

    expect(refusalNode()).toBeNull();

    const testId = `job-rate-limit-try-now-${JOB_ID}`;
    await press(testId, onControl);
    await expectRefusalReached(testId);

    expect(onControl).toHaveBeenCalledWith("resume_job", JOB_ID);
  });

  it("shows the refusal from the daily cancel button too", async () => {
    for (const [testId, command] of [
      [`job-rate-limit-cancel-${JOB_ID}`, "cancel_job"],
    ] as const) {
      const onControl = refusing();
      const view = showCard("daily", onControl);

      await press(testId, onControl);
      await expectRefusalReached(testId);

      expect(onControl).toHaveBeenCalledWith(command, JOB_ID);

      view.unmount();
    }
  });

  it("stays silent when the daily control is accepted", async () => {
    const onControl = accepting();
    showCard("daily", onControl);

    await press(`job-rate-limit-cancel-${JOB_ID}`, onControl);

    expect(onControl).toHaveBeenCalledWith("cancel_job", JOB_ID);
    expect(refusalNode()).toBeNull();
  });
});

describe("the card still renders its own copy when nothing is refused", () => {
  beforeEach(() => invokeMock.mockReset().mockResolvedValue(null));

  it("keeps the per-minute card's copy and its polite status root", () => {
    const view = showCard("per_minute", accepting());

    expect(
      view.container.querySelector('[role="status"]'),
      "the per-minute card's root must stay a polite status region, not an alert",
    ).not.toBeNull();
    expect(screen.queryAllByRole("alert")).toHaveLength(0);
    expect(view.container.textContent).toContain("Taking a short break");
    expect(view.container.textContent).toContain(
      "Your Google Gemini API key is rate-limited.",
    );
    expect(view.container.textContent).toContain("Resume now");
    expect(view.container.textContent).toContain("Switch to Overnight");

    view.unmount();
  });

  it("keeps the daily card's copy and its three controls", () => {
    const view = showCard("daily", accepting());

    expect(screen.getByTestId(`job-rate-limit-card-${JOB_ID}`)).not.toBeNull();
    expect(view.container.textContent).toContain("Waiting on the provider");
    for (const testId of [
      `job-rate-limit-try-now-${JOB_ID}`,
      `job-rate-limit-settings-${JOB_ID}`,
      `job-rate-limit-cancel-${JOB_ID}`,
    ]) {
      expect(screen.getByTestId(testId), `missing ${testId}`).not.toBeNull();
    }
    expect(
      screen.queryByTestId(`job-rate-limit-overnight-${JOB_ID}`),
      "a daily pause never auto-resumes, so the card must not offer an overnight switch",
    ).toBeNull();
  });
});

describe("the per-minute card offers only its own controls", () => {
  beforeEach(() => invokeMock.mockReset().mockResolvedValue(null));

  it("has no cancel or settings control on the per-minute branch", () => {
    showCard("per_minute", accepting());

    expect(
      screen.queryByTestId(`job-rate-limit-cancel-${JOB_ID}`),
      "cancel and Open Settings belong to the daily card; a per-minute row that grew them would \
change what the alert-count assertion above is counting",
    ).toBeNull();
    expect(
      screen.queryByTestId(`job-rate-limit-settings-${JOB_ID}`),
    ).toBeNull();
  });
});
