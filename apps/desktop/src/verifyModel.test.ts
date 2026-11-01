import { describe, expect, it } from "vitest";

import {
  bandLabel as bandLabelMessage,
  checkedCount,
  parseVerifyBlock,
  resynthesisBlockReason as resynthesisBlockReasonMessage,
  reviewRows,
  staleExportNotice as staleExportNoticeMessage,
  scoredNothing,
  scoredNothingNotice as scoredNothingNoticeMessage,
  uncheckedNotice as uncheckedNoticeMessage,
  verifySummary as verifySummaryMessage,
  werLabel as werLabelMessage,
  type SegmentSummary,
  type VerifyCounts,
} from "./verifyModel";
import type { JobState } from "./jobModel";

import { i18nFor, type MessageDescriptor } from "./i18n";
import { stripIsolates } from "./i18n/format";

function english(message: MessageDescriptor): string {
  return stripIsolates(i18nFor("en").t(message.key, message.params));
}
const bandLabel = (...args: Parameters<typeof bandLabelMessage>) =>
  english(bandLabelMessage(...args));
const werLabel = (...args: Parameters<typeof werLabelMessage>) =>
  english(werLabelMessage(...args));
const verifySummary = (...args: Parameters<typeof verifySummaryMessage>) =>
  english(verifySummaryMessage(...args));
const scoredNothingNotice = (
  ...args: Parameters<typeof scoredNothingNoticeMessage>
) => {
  const message = scoredNothingNoticeMessage(...args);
  return message ? english(message) : null;
};
const uncheckedNotice = (
  ...args: Parameters<typeof uncheckedNoticeMessage>
) => {
  const message = uncheckedNoticeMessage(...args);
  return message ? english(message) : null;
};
const resynthesisBlockReason = (
  ...args: Parameters<typeof resynthesisBlockReasonMessage>
) => {
  const message = resynthesisBlockReasonMessage(...args);
  return message ? english(message) : null;
};
const staleExportNotice = (
  ...args: Parameters<typeof staleExportNoticeMessage>
) => {
  const message = staleExportNoticeMessage(...args);
  return message ? english(message) : null;
};

function segment(overrides: Partial<SegmentSummary> = {}): SegmentSummary {
  return {
    id: "s1",
    chapterId: "c1",
    idx: 0,
    pageNo: 1,
    kind: "text",
    text: "A sentence.",
    flags: [],
    hasAudio: true,
    durationMs: 900,
    edited: false,
    wer: null,
    alignSource: "whisperx",
    ...overrides,
  };
}

const REPORT = JSON.stringify({
  verify: {
    enabled: true,
    threshold: 0.15,
    counts: { ok: 40, failed: 2, missing: 1, unscoreable_ref: 3 },
    worst: [
      {
        segment_id: "s2",
        chapter_id: "c1",
        idx: 1,
        band: "failed",
        wer: 0.42,
        reference_words: 12,
      },
      {
        segment_id: "s3",
        chapter_id: "c1",
        idx: 2,
        band: "missing",
        wer: null,
        reference_words: 8,
      },
    ],
  },
});

describe("parseVerifyBlock", () => {
  it("reads counts, threshold and worst entries", () => {
    const block = parseVerifyBlock(REPORT);
    expect(block).not.toBeNull();
    expect(block!.enabled).toBe(true);
    expect(block!.threshold).toBe(0.15);
    expect(block!.counts).toEqual({
      ok: 40,
      failed: 2,
      missing: 1,
      unscoreable_ref: 3,
      unchecked: 0,
    });
    expect(block!.worst.map((entry) => entry.segmentId)).toEqual(["s2", "s3"]);
    expect(block!.worst[0]!.wer).toBe(0.42);
    expect(block!.worst[1]!.wer).toBeNull();
  });

  it("survives an absent block, malformed json and a disabled pass", () => {
    expect(parseVerifyBlock(null)).toBeNull();
    expect(parseVerifyBlock("{not json")).toBeNull();
    expect(parseVerifyBlock(JSON.stringify({ durations_s: {} }))).toBeNull();
    const off = parseVerifyBlock(
      JSON.stringify({
        verify: {
          enabled: false,
          counts: { ok: 0, failed: 0, missing: 0, unscoreable_ref: 0 },
          worst: [],
        },
      }),
    );
    expect(off!.enabled).toBe(false);
    expect(checkedCount(off!.counts)).toBe(0);
  });

  it("drops entries the backend could not describe rather than rendering blanks", () => {
    const block = parseVerifyBlock(
      JSON.stringify({
        verify: {
          enabled: true,
          counts: {},
          worst: [
            { segment_id: "s9", band: "nonsense", wer: 1 },
            { band: "failed", wer: 1 },
            { segment_id: "s8", band: "failed", wer: 0.3 },
          ],
        },
      }),
    );
    expect(block!.worst.map((entry) => entry.segmentId)).toEqual(["s8"]);
  });
});

describe("reviewRows", () => {
  it("lists flagged segments worst-first and appends edited ones", () => {
    const segments = [
      segment({ id: "s1" }),
      segment({ id: "s2", text: "Wrong." }),
      segment({ id: "s3", text: "Silent." }),
      segment({ id: "s4", text: "Fixed.", edited: true, wer: 0.01 }),
    ];
    const rows = reviewRows(segments, parseVerifyBlock(REPORT));
    expect(rows.map((row) => row.segment.id)).toEqual(["s2", "s3", "s4"]);
    expect(rows[0]!.band).toBe("failed");
    expect(rows[1]!.band).toBe("missing");
    expect(rows[2]!.band).toBeNull();
  });

  it("never lists a segment twice and ignores ids the job does not have", () => {
    const rows = reviewRows(
      [segment({ id: "s2", edited: true })],
      parseVerifyBlock(REPORT),
    );
    expect(rows.map((row) => row.segment.id)).toEqual(["s2"]);
  });

  it("is empty when nothing was flagged and nothing was edited", () => {
    expect(reviewRows([segment()], parseVerifyBlock(REPORT))).toEqual([]);
    expect(reviewRows([segment()], null)).toEqual([]);
  });
});

describe("resynthesisBlockReason", () => {
  it("allows editing only on a finished cast", () => {
    expect(resynthesisBlockReason("DONE")).toBeNull();
  });

  it("explains every non-finished state instead of leaving a dead button", () => {
    const states: JobState[] = [
      "CREATED",
      "QUEUED",
      "INGESTING",
      "LAYOUT",
      "VISION",
      "SCRIPTING",
      "SYNTHESIZING",
      "ALIGNING",
      "RENDERING",
      "PAUSED",
      "CANCELLED",
      "FAILED",
    ];
    for (const state of states) {
      const reason = resynthesisBlockReason(state);
      expect(reason, state).toBeTruthy();
      expect(reason!.length, state).toBeGreaterThan(10);
    }
  });
});

describe("labels", () => {
  it("names every band and formats a rate", () => {
    expect(bandLabel("ok")).toContain("clean");
    expect(bandLabel("failed")).toContain("differently");
    expect(bandLabel("missing")).toContain("nothing");
    expect(bandLabel("unscoreable_ref")).toContain("no words");
    expect(bandLabel("unchecked")).toBe("not checked");
    expect(bandLabel("unchecked")).not.toContain("nothing");
    expect(werLabel(0.42)).toBe("42% different");
    expect(werLabel(null)).toBe("no score");
  });

  it("only warns about stale exports when there are some", () => {
    expect(staleExportNotice(0)).toBeNull();
    expect(staleExportNotice(1)).toContain("no longer matches");
    expect(staleExportNotice(3)).toContain("3 exports");
  });
});

describe("an accuracy check that never ran", () => {
  function block(counts: Partial<VerifyCounts>) {
    return parseVerifyBlock(
      JSON.stringify({
        verify: {
          enabled: true,
          threshold: 0.15,
          counts: {
            ok: 0,
            failed: 0,
            missing: 0,
            unscoreable_ref: 0,
            ...counts,
          },
          worst: [],
        },
      }),
    );
  }

  it("does not count an unchecked sentence as checked", () => {
    const parsed = block({ unchecked: 469 });
    expect(parsed!.counts.unchecked).toBe(469);
    expect(checkedCount(parsed!.counts)).toBe(0);
  });

  it("refuses to summarise 469 unscored sentences as zero needing a listen", () => {
    const summary = verifySummary(block({ unchecked: 469 }));
    expect(summary).toBe("469 of 469 not checked");
    expect(summary).not.toContain("need a listen");
    expect(summary).not.toContain("accuracy check off");
  });

  it("keeps the ordinary summary when every sentence was scored", () => {
    expect(verifySummary(block({ ok: 40, failed: 2 }))).toBe(
      "2 of 42 need a listen",
    );
    expect(uncheckedNotice(block({ ok: 40, failed: 2 }))).toBeNull();
  });

  it("still says the check is off when it is off", () => {
    expect(verifySummary(null)).toBe("accuracy check off");
    expect(uncheckedNotice(null)).toBeNull();
  });

  it("tells the user the check did not run and what to do about it", () => {
    const notice = uncheckedNotice(block({ unchecked: 469 }));
    expect(notice).toContain("469 sentences");
    expect(notice).toContain("Re-run");
    expect(uncheckedNotice(block({ unchecked: 1 }))).toContain("1 sentence ");
  });

  it("refuses to summarise the 469-missing production report as zero needing a listen", () => {
    const parsed = block({ missing: 469 });
    expect(scoredNothing(parsed)).toBe(true);
    const summary = verifySummary(parsed);
    expect(summary).not.toBe("0 of 469 need a listen");
    expect(summary).not.toContain("need a listen");
    expect(summary).toContain("scored nothing");
  });

  it("says the checker produced no evidence rather than that the audio was silent", () => {
    const notice = scoredNothingNotice(block({ missing: 469 }));
    expect(notice).toContain("469 sentences");
    expect(notice).toContain("not run, not as a pass");
    expect(notice).not.toContain("nothing heard");
  });

  it("treats one real score as enough to summarise normally", () => {
    const parsed = block({ ok: 1, missing: 468 });
    expect(scoredNothing(parsed)).toBe(false);
    expect(scoredNothingNotice(parsed)).toBeNull();
    expect(verifySummary(parsed)).toBe("0 of 469 need a listen");
  });

  it("does not call an all-unscoreable report a scoring failure", () => {
    const parsed = block({ unscoreable_ref: 7 });
    expect(scoredNothing(parsed)).toBe(false);
    expect(scoredNothingNotice(parsed)).toBeNull();
  });

  it("leaves the never-ran wording to the unchecked notice", () => {
    const parsed = block({ unchecked: 469 });
    expect(scoredNothingNotice(parsed)).toBeNull();
    expect(uncheckedNotice(parsed)).toContain("Re-run");
  });

  it("never calls a scored-nothing report off or clean", () => {
    for (const counts of [{ missing: 3 }, { missing: 1 }]) {
      const summary = verifySummary(block(counts));
      expect(summary).not.toContain("accuracy check off");
      expect(summary).not.toContain("nothing to check");
    }
  });

  it("reads a report written before the band existed without inventing one", () => {
    const legacy = parseVerifyBlock(
      JSON.stringify({
        verify: {
          enabled: true,
          counts: { ok: 1, failed: 0, missing: 2, unscoreable_ref: 0 },
          worst: [],
        },
      }),
    );
    expect(legacy!.counts.unchecked).toBe(0);
    expect(verifySummary(legacy)).toBe("0 of 3 need a listen");
  });
});
