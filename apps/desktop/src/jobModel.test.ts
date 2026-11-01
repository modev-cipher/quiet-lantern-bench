import { describe, expect, it } from "vitest";

import {
  activeStageProgress,
  applyStateNotice,
  coversHeldNotice,
  executionNotices,
  canDeleteJob,
  castsLayout,
  failedStage,
  failureMessage,
  isRunning,
  isTerminal,
  jobBadges,
  OVERNIGHT_QUEUE_NOTICE,
  queuedNotice,
  STARTING_QUEUE_NOTICE,
  WAITING_QUEUE_NOTICE,
  missingModelFromFailure,
  parseOutputPaths,
  parseCheckpointStage,
  progressBarPercent,
  qualityNotices,
  railStage,
  stageEtaLabel,
  stageIndex,
  stageLabel,
  stepStatus,
  STAGE_LABELS,
  type JobProgress,
  type JobSummary,
} from "./jobModel";

function job(overrides: Partial<JobSummary> = {}): JobSummary {
  return {
    id: "job-1",
    bookId: "book-1",
    scope: { kind: "chapters", chapter_ids: ["ch-1"] },
    mode: "narrator",
    voiceProfileId: "preset-kokoro-af-heart",
    outputFormat: "mp3",
    state: "SYNTHESIZING",
    overnight: false,
    etaSeconds: null,
    reportJson: null,
    outputPathsJson: null,
    createdAt: 0,
    updatedAt: 0,
    ...overrides,
  };
}

describe("stageIndex", () => {
  it("maps pipeline states to steps", () => {
    expect(stageIndex("INGESTING")).toBe(0);
    expect(stageIndex("LAYOUT")).toBe(1);
    expect(stageIndex("VISION")).toBe(2);
    expect(stageIndex("SCRIPTING")).toBe(3);
    expect(stageIndex("SYNTHESIZING")).toBe(4);
    expect(stageIndex("ALIGNING")).toBe(5);
    expect(stageIndex("RENDERING")).toBe(6);
  });

  it("puts DONE past the last step", () => {
    expect(stageIndex("DONE")).toBe(STAGE_LABELS.length);
  });

  it("returns -1 for non-pipeline states", () => {
    expect(stageIndex("QUEUED")).toBe(-1);
    expect(stageIndex("PAUSED")).toBe(-1);
  });
});

describe("stepStatus", () => {
  it("marks earlier steps done and later steps pending", () => {
    expect(stepStatus(0, "SCRIPTING")).toBe("done");
    expect(stepStatus(2, "SCRIPTING")).toBe("done");
    expect(stepStatus(3, "SCRIPTING")).toBe("active");
    expect(stepStatus(6, "SCRIPTING")).toBe("pending");
  });

  it("marks every step done when the job is done", () => {
    for (let step = 0; step < STAGE_LABELS.length; step += 1) {
      expect(stepStatus(step, "DONE")).toBe("done");
    }
  });
});

describe("railStage", () => {
  it("parses checkpoint stage directly", () => {
    expect(parseCheckpointStage(JSON.stringify({ stage: "SYNTHESIZING" }))).toBe("SYNTHESIZING");
    expect(parseCheckpointStage(JSON.stringify({ stage: "INVALID" }))).toBeNull();
    expect(parseCheckpointStage(null)).toBeNull();
  });

  it("uses checkpoint stage for PAUSED job", () => {
    const pausedJob = job({
      state: "PAUSED",
      checkpointJson: JSON.stringify({ stage: "SYNTHESIZING" }),
    });
    expect(railStage(pausedJob)).toBe("SYNTHESIZING");
    expect(stepStatus(0, railStage(pausedJob))).toBe("done");
    expect(stepStatus(1, railStage(pausedJob))).toBe("done");
    expect(stepStatus(2, railStage(pausedJob))).toBe("done");
    expect(stepStatus(3, railStage(pausedJob))).toBe("done");
    expect(stepStatus(4, railStage(pausedJob))).toBe("active");
    expect(stepStatus(5, railStage(pausedJob))).toBe("pending");
  });

  it("falls back to QUEUED when checkpoint has no stage", () => {
    const pausedJob = job({
      state: "PAUSED",
      checkpointJson: null,
    });
    expect(railStage(pausedJob)).toBe("QUEUED");
  });

  it("passes through non-PAUSED state directly", () => {
    const runningJob = job({
      state: "SCRIPTING",
      checkpointJson: JSON.stringify({ stage: "SYNTHESIZING" }),
    });
    expect(railStage(runningJob)).toBe("SCRIPTING");
  });
});

describe("state predicates", () => {
  it("classifies terminal states", () => {
    expect(isTerminal("DONE")).toBe(true);
    expect(isTerminal("FAILED")).toBe(true);
    expect(isTerminal("CANCELLED")).toBe(true);
    expect(isTerminal("PAUSED")).toBe(false);
  });

  it("classifies running states", () => {
    expect(isRunning("SYNTHESIZING")).toBe(true);
    expect(isRunning("PAUSED")).toBe(false);
    expect(isRunning("QUEUED")).toBe(false);
    expect(isRunning("DONE")).toBe(false);
  });

  it("allows deleting settled jobs only", () => {
    expect(canDeleteJob("DONE")).toBe(true);
    expect(canDeleteJob("CANCELLED")).toBe(true);
    expect(canDeleteJob("FAILED")).toBe(true);
    expect(canDeleteJob("PAUSED")).toBe(true);
    expect(canDeleteJob("QUEUED")).toBe(false);
    expect(canDeleteJob("SYNTHESIZING")).toBe(false);
  });
});

describe("parseOutputPaths", () => {
  it("parses a json array of strings", () => {
    expect(parseOutputPaths('["/exports/a.mp3","/exports/b.mp3"]')).toEqual([
      "/exports/a.mp3",
      "/exports/b.mp3",
    ]);
  });

  it("drops non-string entries", () => {
    expect(parseOutputPaths('["/a.mp3", 4, null]')).toEqual(["/a.mp3"]);
  });

  it("returns empty for null or invalid json", () => {
    expect(parseOutputPaths(null)).toEqual([]);
    expect(parseOutputPaths("not json")).toEqual([]);
    expect(parseOutputPaths('{"a":1}')).toEqual([]);
  });
});

describe("failureMessage", () => {
  it("prefers the error field", () => {
    expect(failureMessage('{"error":"OOM during synthesis"}')).toBe(
      "OOM during synthesis",
    );
  });

  it("falls back to the message field", () => {
    expect(failureMessage('{"message":"stage crashed"}')).toBe("stage crashed");
  });

  it("returns raw text for non-json reports", () => {
    expect(failureMessage("plain failure")).toBe("plain failure");
  });

  it("returns a default for missing reports", () => {
    expect(failureMessage(null)).toBe("Job failed.");
  });
});

describe("stageLabel", () => {
  it("maps pipeline states to their rail label", () => {
    expect(stageLabel("INGESTING")).toBe("Ingest");
    expect(stageLabel("SCRIPTING")).toBe("Script");
    expect(stageLabel("SYNTHESIZING")).toBe("Voices");
    expect(stageLabel("RENDERING")).toBe("Render");
  });

  it("returns null for non-pipeline states", () => {
    expect(stageLabel("QUEUED")).toBeNull();
    expect(stageLabel("DONE")).toBeNull();
  });
});

describe("failedStage", () => {
  it("reads an explicit failed_stage from the report", () => {
    expect(failedStage('{"failed_stage":"SYNTHESIZING"}', null)).toBe(
      "SYNTHESIZING",
    );
  });

  it("accepts a lowercase stage field", () => {
    expect(failedStage('{"stage":"rendering"}', null)).toBe("RENDERING");
  });

  it("falls back to the last live stage when the report has none", () => {
    expect(failedStage('{"error":"boom"}', "ALIGNING")).toBe("ALIGNING");
    expect(failedStage(null, "SCRIPTING")).toBe("SCRIPTING");
    expect(failedStage("not json", "LAYOUT")).toBe("LAYOUT");
  });

  it("ignores non-pipeline stages", () => {
    expect(failedStage('{"failed_stage":"DONE"}', null)).toBeNull();
    expect(failedStage(null, "PAUSED")).toBeNull();
    expect(failedStage(null, null)).toBeNull();
  });
});

describe("missingModelFromFailure", () => {
  it("extracts the model id from the backend error", () => {
    expect(missingModelFromFailure("model 'kokoro-82m' is not installed")).toBe(
      "kokoro-82m",
    );
  });

  it("handles unquoted ids", () => {
    expect(missingModelFromFailure("model qwen-9b is not installed")).toBe(
      "qwen-9b",
    );
  });

  it("returns null for unrelated failures", () => {
    expect(missingModelFromFailure("OOM during synthesis")).toBeNull();
  });
});

describe("stageEtaLabel", () => {
  it("formats minutes remaining", () => {
    expect(stageEtaLabel(600)).toBe("~10 min left");
  });

  it("handles sub-minute estimates", () => {
    expect(stageEtaLabel(20)).toBe("under a minute left");
  });

  it("returns empty for unknown estimates", () => {
    expect(stageEtaLabel(null)).toBe("");
    expect(stageEtaLabel(-5)).toBe("");
  });
});

describe("progressBarPercent", () => {
  function progress(overrides: Partial<JobProgress> = {}): JobProgress {
    return {
      job_id: "job-1",
      stage: "SYNTHESIZING",
      pct: 0,
      eta_seconds: null,
      current_item: "segment 1/10",
      ...overrides,
    };
  }

  it("scales the 0..1 stage fraction to 0..100", () => {
    expect(progressBarPercent(progress({ pct: 0.5 }))).toBe(50);
    expect(progressBarPercent(progress({ pct: 0.731 }))).toBeCloseTo(73.1);
    expect(progressBarPercent(progress({ pct: 1 }))).toBe(100);
  });

  it("clamps out-of-range fractions", () => {
    expect(progressBarPercent(progress({ pct: -0.2 }))).toBe(0);
    expect(progressBarPercent(progress({ pct: 3 }))).toBe(100);
  });

  it("is 100 when the stage is DONE regardless of pct", () => {
    expect(progressBarPercent(progress({ stage: "DONE", pct: 0 }))).toBe(100);
  });

  it("is 0 without progress", () => {
    expect(progressBarPercent(null)).toBe(0);
  });
});

describe("jobBadges", () => {
  it("lists mode and format", () => {
    expect(jobBadges(job())).toEqual(["narrator", "mp3"]);
  });

  it("adds overnight when queued overnight", () => {
    expect(jobBadges(job({ overnight: true }))).toEqual([
      "narrator",
      "mp3",
      "overnight",
    ]);
  });
});

describe("queuedNotice", () => {
  it("says nothing unless the job is queued", () => {
    expect(queuedNotice(job({ state: "SYNTHESIZING" }), [])).toBeNull();
    expect(
      queuedNotice(job({ state: "DONE", overnight: true }), []),
    ).toBeNull();
  });

  it("promises the overnight batch only to a job that asked for it", () => {
    const overnight = job({ id: "job-2", state: "QUEUED", overnight: true });
    expect(queuedNotice(overnight, [overnight])).toBe(OVERNIGHT_QUEUE_NOTICE());
  });

  it("tells a job waiting behind the worker what it is waiting for", () => {
    const queued = job({ id: "job-2", state: "QUEUED" });
    const running = job({ id: "job-1", state: "RENDERING" });
    expect(queuedNotice(queued, [running, queued])).toBe(
      WAITING_QUEUE_NOTICE(),
    );
  });

  it("does not claim a cast is running when none is", () => {
    const queued = job({ id: "job-2", state: "QUEUED" });
    const settled = job({ id: "job-1", state: "DONE" });
    expect(queuedNotice(queued, [settled, queued])).toBe(
      STARTING_QUEUE_NOTICE(),
    );
    expect(queuedNotice(queued, [queued])).toBe(STARTING_QUEUE_NOTICE());
  });

  it("does not count the queued job itself as the cast it waits for", () => {
    const queued = job({ id: "job-2", state: "QUEUED" });
    expect(queuedNotice(queued, [queued, queued])).toBe(
      STARTING_QUEUE_NOTICE(),
    );
  });

  it("re-records queued after an edit are not overnight work", () => {
    const requeued = job({ id: "job-1", state: "QUEUED", overnight: false });
    expect(queuedNotice(requeued, [requeued])).not.toBe(
      OVERNIGHT_QUEUE_NOTICE(),
    );
  });
});

describe("activeStageProgress", () => {
  function progress(overrides: Partial<JobProgress> = {}): JobProgress {
    return {
      job_id: "job-1",
      stage: "VISION",
      pct: 1,
      eta_seconds: 0,
      current_item: "2 figures explained",
      ...overrides,
    };
  }

  it("discards progress left over from a stage the job has moved past", () => {
    expect(activeStageProgress(progress(), "SCRIPTING")).toBeNull();
  });

  it("keeps progress that belongs to the current stage", () => {
    const current = progress({ stage: "SCRIPTING", pct: 0.25 });
    expect(activeStageProgress(current, "SCRIPTING")).toBe(current);
  });

  it("keeps the last known progress while paused", () => {
    const last = progress({ stage: "SCRIPTING", pct: 0.4 });
    expect(activeStageProgress(last, "PAUSED")).toBe(last);
  });

  it("keeps progress for terminal states", () => {
    const last = progress({ stage: "RENDERING", pct: 1 });
    expect(activeStageProgress(last, "DONE")).toBe(last);
    expect(activeStageProgress(last, "FAILED")).toBe(last);
    expect(activeStageProgress(last, "CANCELLED")).toBe(last);
  });

  it("passes null through", () => {
    expect(activeStageProgress(null, "SCRIPTING")).toBeNull();
  });

  it("discards a stale layout event once vision starts", () => {
    const layoutDone = progress({
      stage: "LAYOUT",
      current_item: "12 regions",
    });
    expect(activeStageProgress(layoutDone, "VISION")).toBeNull();
  });
});

describe("qualityNotices", () => {
  it("reports how many segments fell back to narrator", () => {
    const report = JSON.stringify({
      segment_count: 37,
      flag_counts: { fallback_narrator: 37 },
    });
    expect(qualityNotices(report)).toEqual([
      "37 of 37 segments read as narrator instead of teacher.",
    ]);
  });

  it("reports dropped figure markers as a count, not per segment", () => {
    const report = JSON.stringify({
      segment_count: 39,
      flag_counts: { code_block: 1 },
      figure_markers_dropped: 1,
    });
    expect(qualityNotices(report)).toEqual([
      "1 of 39 segments summarised a code block.",
      "1 unusable figure marker was dropped from the lecture.",
    ]);
  });

  it("pluralises a multi-marker drop", () => {
    const report = JSON.stringify({
      segment_count: 5,
      figure_markers_dropped: 3,
    });
    expect(qualityNotices(report)).toEqual([
      "3 unusable figure markers were dropped from the lecture.",
    ]);
  });

  it("stays silent on a clean job", () => {
    expect(
      qualityNotices(JSON.stringify({ segment_count: 5, flag_counts: {} })),
    ).toEqual([]);
    expect(qualityNotices(null)).toEqual([]);
    expect(qualityNotices("not json")).toEqual([]);
  });

  it("ignores unknown flags", () => {
    const report = JSON.stringify({
      segment_count: 3,
      flag_counts: { something_new: 2 },
    });
    expect(qualityNotices(report)).toEqual([]);
  });

  it("surfaces the language advisory naming what the voice speaks", () => {
    const report = JSON.stringify({
      language_advisory: { book: "zh", voice_speaks: "en" },
    });
    expect(qualityNotices(report)).toEqual([
      "This book is in Chinese, but the chosen voice speaks English — check the narration.",
    ]);
  });

  it("surfaces the language advisory when only the voice engine mismatches", () => {
    const report = JSON.stringify({
      language_advisory: { book: "ar", voice_speaks: null },
    });
    expect(qualityNotices(report)).toEqual([
      "This book is in Arabic, but the chosen voice engine does not list Arabic among the languages it can speak — check the narration.",
    ]);
  });

  it("stays silent when the report carries no language advisory", () => {
    expect(qualityNotices(JSON.stringify({ segment_count: 2 }))).toEqual([]);
    expect(
      qualityNotices(JSON.stringify({ language_advisory: "corrupt" })),
    ).toEqual([]);
  });
});

describe("applyStateNotice", () => {
  const busy = "Engine busy — waiting to retry";
  const recovery = "Reduced GPU usage to continue";

  it("holds a busy notice for as long as the stage keeps refusing", () => {
    let notices = applyStateNotice(
      {},
      { job_id: "j1", state: "SCRIPTING", notice: busy },
    );
    for (let retry = 0; retry < 8; retry += 1) {
      notices = applyStateNotice(notices, {
        job_id: "j1",
        state: "SCRIPTING",
        notice: busy,
      });
    }
    expect(notices.j1).toEqual({ text: busy, state: "SCRIPTING" });
  });

  it("keeps the notice across notice-less updates in the same stage", () => {
    const held = applyStateNotice(
      {},
      { job_id: "j1", state: "SCRIPTING", notice: busy },
    );
    const after = applyStateNotice(held, { job_id: "j1", state: "SCRIPTING" });
    expect(after.j1).toEqual({ text: busy, state: "SCRIPTING" });
    expect(after).toBe(held);
  });

  it("clears the notice when the job moves on", () => {
    const held = applyStateNotice(
      {},
      { job_id: "j1", state: "SCRIPTING", notice: busy },
    );
    expect(
      applyStateNotice(held, { job_id: "j1", state: "SYNTHESIZING" }).j1,
    ).toBeUndefined();
    expect(
      applyStateNotice(held, { job_id: "j1", state: "DONE" }).j1,
    ).toBeUndefined();
    expect(
      applyStateNotice(held, { job_id: "j1", state: "FAILED" }).j1,
    ).toBeUndefined();
  });

  it("replaces one notice with the next", () => {
    const held = applyStateNotice(
      {},
      { job_id: "j1", state: "SCRIPTING", notice: busy },
    );
    const recovered = applyStateNotice(held, {
      job_id: "j1",
      state: "PAUSED",
      notice: recovery,
    });
    expect(recovered.j1).toEqual({ text: recovery, state: "PAUSED" });
  });

  it("keeps jobs independent", () => {
    let notices = applyStateNotice(
      {},
      { job_id: "j1", state: "SCRIPTING", notice: busy },
    );
    notices = applyStateNotice(notices, {
      job_id: "j2",
      state: "SYNTHESIZING",
      notice: busy,
    });
    notices = applyStateNotice(notices, { job_id: "j2", state: "ALIGNING" });
    expect(notices.j1).toEqual({ text: busy, state: "SCRIPTING" });
    expect(notices.j2).toBeUndefined();
  });

  it("re-reports the same notice raised again in a later stage", () => {
    const held = applyStateNotice(
      {},
      { job_id: "j1", state: "SCRIPTING", notice: busy },
    );
    const moved = applyStateNotice(held, {
      job_id: "j1",
      state: "SYNTHESIZING",
    });
    const again = applyStateNotice(moved, {
      job_id: "j1",
      state: "SYNTHESIZING",
      notice: busy,
    });
    expect(again.j1).toEqual({ text: busy, state: "SYNTHESIZING" });
  });
});
describe("executionNotices", () => {
  it("says nothing about a job that never degraded or waited", () => {
    expect(executionNotices(null, "DONE")).toEqual([]);
    expect(executionNotices("not json", "DONE")).toEqual([]);
    expect(
      executionNotices(JSON.stringify({ segment_count: 12 }), "DONE"),
    ).toEqual([]);
    expect(
      executionNotices(
        JSON.stringify({
          plan_revisions: 0,
          contention: { retries: 0, waited_s: 0 },
        }),
        "DONE",
      ),
    ).toEqual([]);
  });

  it("surfaces a degradation that only the checkpoint used to know about", () => {
    const report = JSON.stringify({
      plan_revisions: 2,
      recovery_log: [
        {
          stage: "SYNTHESIZING",
          rev: 1,
          action: "tts batch/workers halved",
          at: 1,
        },
        { stage: "SYNTHESIZING", rev: 2, action: "fallback to cpu", at: 2 },
      ],
    });
    expect(executionNotices(report, "DONE")).toEqual([
      "Ran with reduced hardware use after 2 adjustments (fallback to cpu).",
    ]);
  });

  it("reports a degradation with no log rather than staying silent", () => {
    expect(
      executionNotices(JSON.stringify({ plan_revisions: 1 }), "FAILED"),
    ).toEqual(["Ran with reduced hardware use after 1 adjustment."]);
  });

  it("reports engine waiting on a job that still finished", () => {
    const report = JSON.stringify({ contention: { retries: 5, waited_s: 71 } });
    expect(executionNotices(report, "DONE")).toEqual([
      "Waited 71s for the engine over 5 retries.",
    ]);
  });

  it("explains a contention pause with the totals the decision to resume needs", () => {
    const report = JSON.stringify({
      contention: {
        retries: 9,
        waited_s: 200,
        paused: {
          stage: "SCRIPTING",
          retries: 3,
          waited_s: 121,
          resume_attempts: 2,
          max_pauses: 3,
          at: 1783468800000,
        },
      },
    });
    expect(executionNotices(report, "PAUSED")).toEqual([
      "Paused during Script — the engine stayed busy for 200s over 9 retries. " +
        "Resume to continue (pause 2 of 3).",
    ]);
  });

  it("does not advertise a resume on a job that is no longer paused", () => {
    const report = JSON.stringify({
      contention: {
        retries: 9,
        waited_s: 200,
        paused: {
          stage: "SCRIPTING",
          waited_s: 121,
          resume_attempts: 1,
          max_pauses: 3,
        },
      },
    });
    for (const state of ["CANCELLED", "FAILED", "DONE", "SCRIPTING"] as const) {
      const notices = executionNotices(report, state);
      expect(notices.some((n) => n.includes("Resume to continue"))).toBe(false);
      expect(notices).toEqual(["Waited 200s for the engine over 9 retries."]);
    }
  });

  it("prefers the pause over the running total, so a paused job is not described as finished waiting", () => {
    const report = JSON.stringify({
      plan_revisions: 1,
      recovery_log: [
        { stage: "VISION", rev: 1, action: "gpu layers reduced 20%", at: 1 },
      ],
      contention: {
        retries: 9,
        waited_s: 200,
        paused: {
          stage: "VISION",
          waited_s: 121,
          resume_attempts: 1,
          max_pauses: 3,
        },
      },
    });
    const notices = executionNotices(report, "PAUSED");
    expect(notices).toHaveLength(2);
    expect(notices[0]).toContain("reduced hardware use");
    expect(notices[1]).toContain("Resume to continue");
    expect(
      notices.some((n) => n === "Waited 200s for the engine over 9 retries."),
    ).toBe(false);
  });

  it("omits the cap when the report predates it", () => {
    const report = JSON.stringify({
      contention: {
        retries: 1,
        waited_s: 5,
        paused: { stage: "VISION", resume_attempts: 1 },
      },
    });
    expect(executionNotices(report, "PAUSED")[0]).toContain("(pause 1).");
  });

  it("ignores a hand-edited report instead of throwing", () => {
    expect(
      executionNotices(
        JSON.stringify({ contention: 7, plan_revisions: "two" }),
        "DONE",
      ),
    ).toEqual([]);
    expect(
      executionNotices(
        JSON.stringify({ contention: { paused: [] }, recovery_log: "no" }),
        "PAUSED",
      ),
    ).toEqual([]);
  });
});

describe("coversHeldNotice", () => {
  const pause = [
    "Paused during Script — the engine stayed busy for 200s over 9 retries. " +
      "Resume to continue (pause 2 of 3).",
  ];

  it("suppresses the transient notice once the durable line says the same thing", () => {
    expect(coversHeldNotice(pause, "Paused — the engine stayed busy")).toBe(
      true,
    );
  });

  it("keeps a transient notice that nothing durable covers", () => {
    expect(coversHeldNotice([], "Engine busy — waiting to retry")).toBe(false);
    expect(
      coversHeldNotice(
        ["Waited 200s for the engine over 9 retries."],
        "Engine busy — waiting to retry",
      ),
    ).toBe(false);
    expect(coversHeldNotice(pause, undefined)).toBe(false);
  });
});

describe("qualityNotices verify band reporting", () => {
  const withVerify = (verify: unknown) =>
    JSON.stringify({ segment_count: 10, flag_counts: {}, verify });

  it("says nothing when the accuracy check was not requested", () => {
    expect(
      qualityNotices(withVerify({ enabled: false, counts: { failed: 3 } })),
    ).toEqual([]);
  });

  it("reports failed, missing and unscoreable segments separately", () => {
    const notices = qualityNotices(
      withVerify({
        enabled: true,
        threshold: 0.15,
        counts: { ok: 6, failed: 2, missing: 1, unscoreable_ref: 1 },
      }),
    );
    expect(notices).toEqual([
      "2 segments read back with more than 15% word difference.",
      "1 segment could not be checked because the accuracy pass heard nothing.",
      "1 segment has no words to check against.",
    ]);
  });

  it("keeps the pre-existing notices ahead of the verify ones", () => {
    const notices = qualityNotices(
      JSON.stringify({
        segment_count: 4,
        flag_counts: { code_block: 1 },
        verify: { enabled: true, threshold: 0.15, counts: { failed: 1 } },
      }),
    );
    expect(notices[0]).toBe("1 of 4 segments summarised a code block.");
    expect(notices).toHaveLength(2);
  });

  it("ignores a malformed verify block", () => {
    expect(qualityNotices(withVerify("nonsense"))).toEqual([]);
    expect(qualityNotices(withVerify({ enabled: true }))).toEqual([]);
  });

  it("says loudest of all when the check never ran", () => {
    const notices = qualityNotices(
      withVerify({
        enabled: true,
        threshold: 0.15,
        counts: {
          ok: 0,
          failed: 0,
          missing: 0,
          unscoreable_ref: 0,
          unchecked: 469,
        },
      }),
    );
    expect(notices).toEqual([
      "The accuracy check did not run on 469 of 469 segments — re-run it to score them.",
    ]);
  });

  it("puts the check that never ran ahead of the sentences it did score", () => {
    const notices = qualityNotices(
      withVerify({
        enabled: true,
        threshold: 0.15,
        counts: {
          ok: 5,
          failed: 2,
          missing: 0,
          unscoreable_ref: 0,
          unchecked: 3,
        },
      }),
    );
    expect(notices[0]).toBe(
      "The accuracy check did not run on 3 of 10 segments — re-run it to score them.",
    );
    expect(notices).toHaveLength(2);
  });
});

describe("qualityNotices reports a failed verification exactly once", () => {
  it("does not repeat the flag count beside the verify count", () => {
    const notices = qualityNotices(
      JSON.stringify({
        segment_count: 10,
        flag_counts: { verify_failed: 2 },
        verify: {
          enabled: true,
          threshold: 0.15,
          counts: { ok: 8, failed: 2, missing: 0, unscoreable_ref: 0 },
        },
      }),
    );
    expect(notices).toEqual([
      "2 segments read back with more than 15% word difference.",
    ]);
  });
});

describe("providerFailure recovery facts", () => {
  it("parses the typed projection and ignores anything else", async () => {
    const { providerFailure, providerFailureCopy } = await import("./jobModel");
    const report = JSON.stringify({
      provider_failure: {
        class: "quota",
        retryable: true,
        retry_after_secs: 30,
        detail: "provider returned HTTP 429",
      },
    });
    const failure = providerFailure(report);
    expect(failure).toEqual({
      class: "quota",
      retryable: true,
      retryAfterSecs: 30,
      detail: "provider returned HTTP 429",
    });
    expect(providerFailureCopy(failure!)).toContain("30s");
    expect(providerFailure(JSON.stringify({ other: 1 }))).toBeNull();
    expect(providerFailure(null)).toBeNull();
  });

  it("every failure class has actionable copy that never blames the wrong thing", async () => {
    const { providerFailureCopy } = await import("./jobModel");
    for (const klass of [
      "auth",
      "quota",
      "transport",
      "timeout",
      "model_not_found",
      "malformed",
      "provider",
    ]) {
      const copy = providerFailureCopy({
        class: klass,
        retryable: false,
        retryAfterSecs: null,
        detail: "",
      });
      expect(copy.length).toBeGreaterThan(10);
      if (klass === "transport" || klass === "timeout") {
        expect(copy).not.toMatch(/key/i);
      }
    }
  });
});

describe("recovery gating (Hermes R2 blocker 7)", () => {
  it("non-retryable failures do not offer a same-binding retry", async () => {
    const { providerFailure } = await import("./jobModel");
    const failure = providerFailure(
      JSON.stringify({
        provider_failure: {
          class: "auth",
          retryable: false,
          retry_after_secs: null,
          detail: "401",
        },
      }),
    );
    expect(failure?.retryable).toBe(false);
  });
});

describe("recovery history is visible in the final report (S-4 F7 design pass)", () => {
  it("parses provider_recovery_log entries and renders the calm line", async () => {
    const { recoveryHistory, recoveryLine } = await import("./jobModel");
    const report = JSON.stringify({
      provider_recovery_log: [
        { action: "retry_same_binding", plan_rev: 1, at_unix: 1 },
        { action: "switch_to_local", plan_rev: 2, at_unix: 2 },
      ],
      provider_plan_rev: 2,
    });
    const events = recoveryHistory(report);
    expect(events).toEqual([
      { action: "retry_same_binding", planRev: 1 },
      { action: "switch_to_local", planRev: 2 },
    ]);
    expect(recoveryLine(events)).toBe(
      "Recovered: switched to local · attempt 2",
    );
  });

  it("a single retry reads without an attempt count", async () => {
    const { recoveryHistory, recoveryLine } = await import("./jobModel");
    const events = recoveryHistory(
      JSON.stringify({
        provider_recovery_log: [
          { action: "retry_same_binding", plan_rev: 1, at_unix: 1 },
        ],
      }),
    );
    expect(recoveryLine(events)).toBe("Recovered: retried the same provider");
  });

  it("no log, malformed log, or malformed entries yield no line — never a crash", async () => {
    const { recoveryHistory, recoveryLine } = await import("./jobModel");
    expect(recoveryLine(recoveryHistory(null))).toBeNull();
    expect(recoveryLine(recoveryHistory("{}"))).toBeNull();
    expect(
      recoveryLine(
        recoveryHistory(JSON.stringify({ provider_recovery_log: "corrupt" })),
      ),
    ).toBeNull();
    expect(
      recoveryHistory(
        JSON.stringify({ provider_recovery_log: [null, 7, { action: 3 }] }),
      ),
    ).toEqual([]);
  });
});

describe("formatCountdown", () => {
  it("formats positive milliseconds as mm:ss with zero padding", async () => {
    const { formatCountdown } = await import("./jobModel");
    expect(formatCountdown(65_000)).toBe("01:05");
    expect(formatCountdown(5_000)).toBe("00:05");
    expect(formatCountdown(120_000)).toBe("02:00");
    expect(formatCountdown(599_000)).toBe("09:59");
    expect(formatCountdown(600_000)).toBe("10:00");
  });

  it("handles boundary values, zero, and negative values safely", async () => {
    const { formatCountdown } = await import("./jobModel");
    expect(formatCountdown(0)).toBe("00:00");
    expect(formatCountdown(-1_000)).toBe("00:00");
    expect(formatCountdown(Number.NaN)).toBe("00:00");
    expect(formatCountdown(Number.NEGATIVE_INFINITY)).toBe("00:00");
  });
});

describe("parsePausedReason", () => {
  it("parses per_minute rate limit reason from checkpointJson", async () => {
    const { parsePausedReason } = await import("./jobModel");
    const item = job({
      state: "PAUSED",
      checkpointJson: JSON.stringify({
        stage: "SYNTHESIZING",
        paused_reason: {
          kind: "rate_limit",
          class: "per_minute",
          retry_at_epoch_ms: 1726348900000,
          provider: "google",
          segment_idx: 42,
          last_error_message: "RESOURCE_EXHAUSTED",
          attempt: 2,
        },
      }),
    });
    const parsed = parsePausedReason(item);
    expect(parsed).toEqual({
      kind: "rate_limit",
      class: "per_minute",
      retry_at_epoch_ms: 1726348900000,
      provider: "google",
      segment_idx: 42,
      last_error_message: "RESOURCE_EXHAUSTED",
      attempt: 2,
    });
  });

  it("parses daily rate limit reason from checkpointJson", async () => {
    const { parsePausedReason } = await import("./jobModel");
    const item = job({
      state: "PAUSED",
      checkpointJson: JSON.stringify({
        stage: "SYNTHESIZING",
        paused_reason: {
          kind: "rate_limit",
          class: "daily",
          retry_at_epoch_ms: null,
          provider: "google",
          segment_idx: 10,
          last_error_message: "daily quota exceeded",
          attempt: 1,
        },
      }),
    });
    const parsed = parsePausedReason(item);
    expect(parsed).toEqual({
      kind: "rate_limit",
      class: "daily",
      retry_at_epoch_ms: null,
      provider: "google",
      segment_idx: 10,
      last_error_message: "daily quota exceeded",
      attempt: 1,
    });
  });

  it("falls back to reportJson when checkpointJson has no paused_reason", async () => {
    const { parsePausedReason } = await import("./jobModel");
    const item = job({
      state: "PAUSED",
      checkpointJson: JSON.stringify({ stage: "SYNTHESIZING" }),
      reportJson: JSON.stringify({
        paused_reason: {
          kind: "rate_limit",
          class: "per_minute",
          retry_at_epoch_ms: 1726348900000,
          provider: "google",
          segment_idx: 5,
          last_error_message: "Rate limit exceeded",
        },
      }),
    });
    const parsed = parsePausedReason(item);
    expect(parsed).not.toBeNull();
    expect(parsed?.class).toBe("per_minute");
    expect(parsed?.attempt).toBe(1);
  });

  it("returns null when neither checkpointJson nor reportJson has paused_reason", async () => {
    const { parsePausedReason } = await import("./jobModel");
    expect(
      parsePausedReason(job({ checkpointJson: null, reportJson: null })),
    ).toBeNull();
    expect(
      parsePausedReason(job({ checkpointJson: "{}", reportJson: "{}" })),
    ).toBeNull();
  });

  it("returns null for malformed or unrecognized rate limit classes", async () => {
    const { parsePausedReason } = await import("./jobModel");
    const item = job({
      checkpointJson: JSON.stringify({
        paused_reason: {
          kind: "rate_limit",
          class: "unknown_class",
          provider: "google",
        },
      }),
    });
    expect(parsePausedReason(item)).toBeNull();
  });

  it("returns null for corrupt JSON without crashing", async () => {
    const { parsePausedReason } = await import("./jobModel");
    const item = job({
      checkpointJson: "corrupt{json",
      reportJson: "corrupt[json",
    });
    expect(parsePausedReason(item)).toBeNull();
  });
});

describe("castsLayout", () => {
  it("puts unfinished casts in Now and everything finished in History, keeping order", () => {
    const layout = castsLayout([
      job({ id: "run", state: "SYNTHESIZING" }),
      job({ id: "queued", state: "QUEUED" }),
      job({ id: "done-new", state: "DONE" }),
      job({ id: "done-old", state: "DONE" }),
    ]);
    expect(layout.now.map((j) => j.id)).toEqual(["run", "queued"]);
    expect(layout.history.map((h) => h.job.id)).toEqual([
      "done-new",
      "done-old",
    ]);
  });

  it("keeps the newest finished cast open and folds older ones", () => {
    const { history } = castsLayout([
      job({ id: "a", state: "DONE" }),
      job({ id: "b", state: "DONE" }),
      job({ id: "c", state: "DONE" }),
    ]);
    expect(history.map((h) => h.openByDefault)).toEqual([true, false, false]);
  });

  it("never folds a failed or cancelled cast, which still needs a decision", () => {
    const { history } = castsLayout([
      job({ id: "ok", state: "DONE" }),
      job({ id: "bad", state: "FAILED" }),
      job({ id: "stop", state: "CANCELLED" }),
      job({ id: "old", state: "DONE" }),
    ]);
    expect(history.map((h) => [h.job.id, h.openByDefault])).toEqual([
      ["ok", true],
      ["bad", true],
      ["stop", true],
      ["old", false],
    ]);
  });

  it("is empty for no casts", () => {
    expect(castsLayout([])).toEqual({ now: [], history: [] });
  });
});
