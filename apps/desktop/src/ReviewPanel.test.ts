// @vitest-environment jsdom
import { createElement } from "react";
import {
  act,
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

import ReviewPanel from "./ReviewPanel";
import type { JobSummary } from "./jobModel";
import type { SegmentSummary } from "./verifyModel";

const JOB_ID = "job-review-1";

const VERIFY_REPORT = JSON.stringify({
  segment_count: 3,
  verify: {
    enabled: true,
    threshold: 0.15,
    counts: { ok: 2, failed: 1, missing: 0, unscoreable_ref: 0 },
    worst: [
      {
        segment_id: "seg-2",
        band: "failed",
        wer: 0.42,
        reference_words: 12,
      },
    ],
  },
});

function job(overrides: Partial<JobSummary> = {}): JobSummary {
  return {
    id: JOB_ID,
    bookId: "book-1",
    scope: { kind: "chapters", chapter_ids: [] },
    mode: "narrator",
    voiceProfileId: "voice-1",
    outputFormat: "mp3",
    state: "DONE",
    overnight: false,
    etaSeconds: null,
    reportJson: VERIFY_REPORT,
    outputPathsJson: null,
    createdAt: 1,
    updatedAt: 1,
    ...overrides,
  };
}

function segment(
  text: string,
  overrides: Partial<SegmentSummary> = {},
): SegmentSummary {
  return {
    id: "seg-2",
    chapterId: "ch-1",
    idx: 1,
    pageNo: 1,
    kind: "body",
    text,
    flags: [],
    hasAudio: true,
    durationMs: 1200,
    edited: false,
    wer: 0.42,
    alignSource: null,
    ...overrides,
  };
}

describe("ReviewPanel is reachable before its sentences load", () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });
  afterEach(() => {
    cleanup();
  });

  it("shows the accuracy summary and its toggle for a checked job", () => {
    render(createElement(ReviewPanel, { job: job() }));
    expect(screen.getByTestId(`review-toggle-${JOB_ID}`)).toBeTruthy();
    expect(screen.getByTestId(`review-summary-${JOB_ID}`).textContent).toBe(
      "1 of 3 need a listen",
    );
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("says the accuracy check was off for a job that never asked for it", () => {
    render(createElement(ReviewPanel, { job: job({ reportJson: null }) }));
    expect(screen.getByTestId(`review-summary-${JOB_ID}`).textContent).toBe(
      "accuracy check off",
    );
  });

  it("opening it loads the flagged sentence", async () => {
    invokeMock.mockResolvedValue([segment("The lyin sleps tonite.")]);
    render(createElement(ReviewPanel, { job: job() }));
    fireEvent.click(screen.getByTestId(`review-toggle-${JOB_ID}`));
    await waitFor(() => {
      expect(screen.getByTestId("review-text-seg-2").textContent).toBe(
        "The lyin sleps tonite.",
      );
    });
    expect(invokeMock).toHaveBeenCalledWith("list_reviewable_segments", {
      jobId: JOB_ID,
    });
  });

  it("an open panel reloads when the job moves on, so a re-recorded sentence shows its new text", async () => {
    invokeMock.mockResolvedValueOnce([segment("The lyin sleps tonite.")]);
    const view = render(createElement(ReviewPanel, { job: job() }));
    fireEvent.click(screen.getByTestId(`review-toggle-${JOB_ID}`));
    await waitFor(() => {
      expect(screen.getByTestId("review-text-seg-2").textContent).toBe(
        "The lyin sleps tonite.",
      );
    });

    invokeMock.mockResolvedValueOnce([segment("The lion sleeps tonight.")]);
    view.rerender(createElement(ReviewPanel, { job: job({ updatedAt: 2 }) }));
    await waitFor(() => {
      expect(screen.getByTestId("review-text-seg-2").textContent).toBe(
        "The lion sleeps tonight.",
      );
    });
    expect(invokeMock).toHaveBeenCalledTimes(2);
  });

  it("a refresh that finishes after a newer one never brings back the old sentence or a stale error", async () => {
    invokeMock.mockResolvedValueOnce([segment("The lyin sleps tonite.")]);
    const view = render(createElement(ReviewPanel, { job: job() }));
    fireEvent.click(screen.getByTestId(`review-toggle-${JOB_ID}`));
    await waitFor(() => {
      expect(screen.getByTestId("review-text-seg-2").textContent).toBe(
        "The lyin sleps tonite.",
      );
    });

    let resolveStale: (rows: SegmentSummary[]) => void = () => {};
    let rejectStale: (reason: unknown) => void = () => {};
    let resolveNewest: (rows: SegmentSummary[]) => void = () => {};
    invokeMock.mockReturnValueOnce(
      new Promise((resolve) => {
        resolveStale = resolve;
      }),
    );
    invokeMock.mockReturnValueOnce(
      new Promise((_resolve, reject) => {
        rejectStale = reject;
      }),
    );
    invokeMock.mockReturnValueOnce(
      new Promise((resolve) => {
        resolveNewest = resolve;
      }),
    );
    view.rerender(createElement(ReviewPanel, { job: job({ updatedAt: 2 }) }));
    view.rerender(createElement(ReviewPanel, { job: job({ updatedAt: 3 }) }));
    view.rerender(createElement(ReviewPanel, { job: job({ updatedAt: 4 }) }));
    expect(invokeMock).toHaveBeenCalledTimes(4);

    await act(async () => {
      resolveNewest([segment("The lion sleeps tonight.")]);
    });
    await act(async () => {
      resolveStale([segment("The lyin sleps tonite.")]);
    });
    await act(async () => {
      rejectStale(new Error("stale failure"));
    });
    expect(screen.getByTestId("review-text-seg-2").textContent).toBe(
      "The lion sleeps tonight.",
    );
    expect(screen.queryByTestId(`review-error-${JOB_ID}`)).toBeNull();
  });

  it("protects a draft from another sentence's edit or revert", async () => {
    invokeMock.mockImplementation((command) =>
      command === "list_reviewable_segments"
        ? Promise.resolve([
            segment("Earlier edit", { id: "seg-1", idx: 0, edited: true }),
            segment("Needs correction"),
          ])
        : Promise.resolve(undefined),
    );
    render(createElement(ReviewPanel, { job: job() }));
    fireEvent.click(screen.getByTestId(`review-toggle-${JOB_ID}`));
    await screen.findByTestId("review-revert-seg-1");
    fireEvent.click(screen.getByTestId("review-edit-seg-2"));
    const input = screen.getByTestId(
      "review-input-seg-2",
    ) as HTMLTextAreaElement;
    fireEvent.change(input, {
      target: { value: "Keep this unsaved correction" },
    });
    expect(document.activeElement).toBe(input);
    expect(
      (screen.getByTestId("review-revert-seg-1") as HTMLButtonElement).disabled,
    ).toBe(true);
    expect(
      (screen.getByTestId("review-edit-seg-1") as HTMLButtonElement).disabled,
    ).toBe(true);
    fireEvent.click(screen.getByTestId("review-revert-seg-1"));
    expect(
      invokeMock.mock.calls.filter(
        ([command]) => command === "resynthesize_segment",
      ),
    ).toHaveLength(0);
    expect(screen.queryByTestId("review-input-seg-2")).toBe(input);
    expect(input.value).toBe("Keep this unsaved correction");
    expect(document.activeElement).toBe(input);
  });

  it("serializes mutations across rows before a job is requeued", async () => {
    let finish = () => {};
    invokeMock.mockImplementation((command) =>
      command === "list_reviewable_segments"
        ? Promise.resolve([
            segment("Earlier edit", { id: "seg-1", idx: 0, edited: true }),
            segment("Needs correction", { edited: true }),
          ])
        : new Promise<void>((resolve) => {
            finish = resolve;
          }),
    );
    render(createElement(ReviewPanel, { job: job() }));
    fireEvent.click(screen.getByTestId(`review-toggle-${JOB_ID}`));
    const first = await screen.findByTestId("review-revert-seg-1");
    const second = screen.getByTestId("review-revert-seg-2");
    act(() => {
      fireEvent.click(first);
      fireEvent.click(second);
    });
    expect(
      invokeMock.mock.calls.filter(
        ([command]) => command === "resynthesize_segment",
      ),
    ).toHaveLength(1);
    expect(
      (screen.getByTestId("review-edit-seg-2") as HTMLButtonElement).disabled,
    ).toBe(true);
    await act(async () => {
      finish();
    });
    expect(
      (screen.getByTestId("review-edit-seg-2") as HTMLButtonElement).disabled,
    ).toBe(false);
  });

  it("locks a submitted draft until its acknowledgment without implying that Cancel can undo it", async () => {
    let finish = () => {};
    invokeMock.mockImplementation((command) =>
      command === "list_reviewable_segments"
        ? Promise.resolve([
            segment("Earlier edit", { id: "seg-1", idx: 0, edited: true }),
            segment("Needs correction"),
          ])
        : new Promise<void>((resolve) => {
            finish = resolve;
          }),
    );
    render(createElement(ReviewPanel, { job: job() }));
    fireEvent.click(screen.getByTestId(`review-toggle-${JOB_ID}`));
    fireEvent.click(await screen.findByTestId("review-edit-seg-2"));
    const input = screen.getByTestId(
      "review-input-seg-2",
    ) as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: "Submitted correction" } });
    fireEvent.click(screen.getByTestId("review-save-seg-2"));
    expect(input.readOnly).toBe(true);
    fireEvent.keyDown(input, { key: "Escape" });
    expect(screen.queryByTestId("review-input-seg-2")).toBe(input);
    expect(
      (screen.getByTestId("review-cancel-seg-2") as HTMLButtonElement).disabled,
    ).toBe(true);
    expect(
      (screen.getByTestId("review-edit-seg-1") as HTMLButtonElement).disabled,
    ).toBe(true);
    await act(async () => {
      finish();
    });
    expect(screen.queryByTestId("review-input-seg-2")).toBeNull();
  });

  it("retains a rejected edit and allows its save to be retried", async () => {
    let attempts = 0;
    invokeMock.mockImplementation((command) => {
      if (command === "list_reviewable_segments")
        return Promise.resolve([segment("Needs correction")]);
      return ++attempts === 1
        ? Promise.reject(new Error("temporarily locked"))
        : Promise.resolve(undefined);
    });
    render(createElement(ReviewPanel, { job: job() }));
    fireEvent.click(screen.getByTestId(`review-toggle-${JOB_ID}`));
    fireEvent.click(await screen.findByTestId("review-edit-seg-2"));
    fireEvent.change(screen.getByTestId("review-input-seg-2"), {
      target: { value: "Retained correction" },
    });
    fireEvent.click(screen.getByTestId("review-save-seg-2"));
    await screen.findByRole("alert");
    expect(
      (screen.getByTestId("review-input-seg-2") as HTMLTextAreaElement)
        .readOnly,
    ).toBe(false);
    expect(
      (screen.getByTestId("review-input-seg-2") as HTMLTextAreaElement).value,
    ).toBe("Retained correction");
    fireEvent.click(screen.getByTestId("review-save-seg-2"));
    await waitFor(() =>
      expect(screen.queryByTestId("review-input-seg-2")).toBeNull(),
    );
    expect(attempts).toBe(2);
  });
});

describe("localized sentence repair", () => {
  afterEach(async () => {
    cleanup();
    const { setLocalePreference } = await import("./i18n");
    invokeMock.mockResolvedValue(null);
    setLocalePreference("en");
  });

  it("translates summaries, measurements and an already-open error without losing the draft", async () => {
    const { setLocalePreference } = await import("./i18n");
    invokeMock.mockReset();
    invokeMock.mockResolvedValueOnce([segment("API يشرح 12 خطوة.")]);
    render(createElement(ReviewPanel, { job: job(), bookLanguage: "ar" }));
    fireEvent.click(screen.getByTestId(`review-toggle-${JOB_ID}`));
    await screen.findByTestId("review-edit-seg-2");
    const text = screen.getByTestId("review-text-seg-2");
    expect(text.getAttribute("dir")).toBe("rtl");
    fireEvent.click(screen.getByTestId("review-edit-seg-2"));
    const editor = screen.getByRole("textbox") as HTMLTextAreaElement;
    expect(editor.getAttribute("lang")).toBe("ar");
    fireEvent.change(editor, { target: { value: "API يشرح 13 خطوة." } });
    invokeMock.mockRejectedValueOnce("offline");
    fireEvent.click(screen.getByTestId("review-save-seg-2"));
    await screen.findByRole("alert");
    invokeMock.mockResolvedValue(null);
    await act(async () => setLocalePreference("ar"));
    expect(screen.getByTestId(`review-summary-${JOB_ID}`).textContent).toBe(
      "جملة واحدة من 3 تحتاج إلى الاستماع",
    );
    expect(screen.getByTestId("review-wer-seg-2").textContent).toBe(
      "42% اختلاف",
    );
    expect(screen.getByRole("alert").textContent).toContain(
      "تعذّرت إعادة تسجيل هذه الجملة",
    );
    expect(screen.getByRole("alert").textContent).toContain("offline");
    expect(screen.getByTestId("review-cancel-seg-2").textContent).toBe("إلغاء");
    expect(editor.value).toBe("API يشرح 13 خطوة.");
  });
});

it("does not claim that nothing needs review when loading failed, and offers a localized retry", async () => {
  invokeMock.mockReset().mockRejectedValueOnce("offline");
  render(createElement(ReviewPanel, { job: job() }));
  fireEvent.click(screen.getByTestId(`review-toggle-${JOB_ID}`));
  await screen.findByRole("alert");
  expect(screen.queryByTestId(`review-empty-${JOB_ID}`)).toBeNull();
  invokeMock.mockResolvedValue([segment("Recovered sentence.")]);
  const retry = screen.getByRole("button", { name: "Retry" });
  retry.focus();
  fireEvent.click(retry);
  expect(document.activeElement).toBe(
    screen.getByTestId(`review-toggle-${JOB_ID}`),
  );
  await screen.findByTestId("review-text-seg-2");
  expect(screen.queryByRole("alert")).toBeNull();
});
