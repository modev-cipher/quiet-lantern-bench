import type { MessageDescriptor } from "./i18n";
import type { JobState } from "./jobModel";

export type VerifyBand =
  "ok" | "failed" | "missing" | "unscoreable_ref" | "unchecked";

export interface SegmentSummary {
  id: string;
  chapterId: string;
  idx: number;
  pageNo: number;
  kind: string;
  text: string;
  flags: string[];
  hasAudio: boolean;
  durationMs: number | null;
  edited: boolean;
  wer: number | null;
  alignSource: string | null;
}

export interface VerifyCounts {
  ok: number;
  failed: number;
  missing: number;
  unscoreable_ref: number;
  unchecked: number;
}

export interface VerifyEntry {
  segmentId: string;
  band: VerifyBand;
  wer: number | null;
  referenceWords: number | null;
}

export interface VerifyBlock {
  enabled: boolean;
  threshold: number;
  counts: VerifyCounts;
  worst: VerifyEntry[];
}

const BANDS: VerifyBand[] = [
  "ok",
  "failed",
  "missing",
  "unscoreable_ref",
  "unchecked",
];

function asBand(value: unknown): VerifyBand | null {
  return typeof value === "string" && (BANDS as string[]).includes(value)
    ? (value as VerifyBand)
    : null;
}

function asNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

export function parseVerifyBlock(
  reportJson: string | null,
): VerifyBlock | null {
  if (!reportJson) return null;
  let block: Record<string, unknown>;
  try {
    const parsed: unknown = JSON.parse(reportJson);
    if (!parsed || typeof parsed !== "object") return null;
    const candidate = (parsed as Record<string, unknown>).verify;
    if (!candidate || typeof candidate !== "object") return null;
    block = candidate as Record<string, unknown>;
  } catch {
    return null;
  }
  const rawCounts = (block.counts ?? {}) as Record<string, unknown>;
  const counts: VerifyCounts = {
    ok: asNumber(rawCounts.ok) ?? 0,
    failed: asNumber(rawCounts.failed) ?? 0,
    missing: asNumber(rawCounts.missing) ?? 0,
    unscoreable_ref: asNumber(rawCounts.unscoreable_ref) ?? 0,
    unchecked: asNumber(rawCounts.unchecked) ?? 0,
  };
  const worst: VerifyEntry[] = Array.isArray(block.worst)
    ? block.worst.flatMap((raw) => {
        if (!raw || typeof raw !== "object") return [];
        const entry = raw as Record<string, unknown>;
        const band = asBand(entry.band);
        const segmentId = entry.segment_id;
        if (!band || typeof segmentId !== "string") return [];
        return [
          {
            segmentId,
            band,
            wer: asNumber(entry.wer),
            referenceWords: asNumber(entry.reference_words),
          },
        ];
      })
    : [];
  return {
    enabled: block.enabled === true,
    threshold: asNumber(block.threshold) ?? 0.15,
    counts,
    worst,
  };
}

export function bandLabel(band: VerifyBand): MessageDescriptor {
  return { key: `review.band.${band}` };
}

export function werLabel(wer: number | null): MessageDescriptor {
  return wer == null
    ? { key: "review.noScore" }
    : { key: "review.wer", params: { percent: Math.round(wer * 100) } };
}

export function checkedCount(counts: VerifyCounts): number {
  return counts.ok + counts.failed + counts.missing + counts.unscoreable_ref;
}

export function scoredCount(counts: VerifyCounts): number {
  return counts.ok + counts.failed;
}

export function scoreableCount(counts: VerifyCounts): number {
  return counts.ok + counts.failed + counts.missing + counts.unchecked;
}

export function scoredNothing(block: VerifyBlock | null): boolean {
  if (!block?.enabled) return false;
  return scoreableCount(block.counts) > 0 && scoredCount(block.counts) === 0;
}

export function verifySummary(block: VerifyBlock | null): MessageDescriptor {
  if (!block?.enabled) return { key: "review.off" };
  const checked = checkedCount(block.counts);
  const { unchecked } = block.counts;
  if (unchecked > 0)
    return {
      key: "review.uncheckedSummary",
      params: { count: unchecked, total: checked + unchecked },
    };
  if (scoredNothing(block))
    return {
      key: "review.scoredNoneSummary",
      params: { count: scoreableCount(block.counts) },
    };
  if (checked === 0) return { key: "review.none" };
  return {
    key: "review.summary",
    params: { count: block.counts.failed, total: checked },
  };
}

export function scoredNothingNotice(
  block: VerifyBlock | null,
): MessageDescriptor | null {
  if (!block || !scoredNothing(block) || block.counts.unchecked > 0)
    return null;
  return {
    key: "review.scoredNoneNotice",
    params: { count: scoreableCount(block.counts) },
  };
}

export function uncheckedNotice(
  block: VerifyBlock | null,
): MessageDescriptor | null {
  if (!block?.enabled || block.counts.unchecked <= 0) return null;
  return {
    key: "review.uncheckedNotice",
    params: { count: block.counts.unchecked },
  };
}

export interface ReviewRow {
  segment: SegmentSummary;
  band: VerifyBand | null;
  wer: number | null;
  referenceWords: number | null;
}

export function reviewRows(
  segments: SegmentSummary[],
  block: VerifyBlock | null,
): ReviewRow[] {
  const byId = new Map(segments.map((segment) => [segment.id, segment]));
  const rows: ReviewRow[] = [];
  const seen = new Set<string>();
  for (const entry of block?.worst ?? []) {
    const segment = byId.get(entry.segmentId);
    if (!segment || seen.has(segment.id)) continue;
    seen.add(segment.id);
    rows.push({
      segment,
      band: entry.band,
      wer: entry.wer,
      referenceWords: entry.referenceWords,
    });
  }
  for (const segment of segments) {
    if (seen.has(segment.id) || !segment.edited) continue;
    seen.add(segment.id);
    rows.push({
      segment,
      band: null,
      wer: segment.wer,
      referenceWords: null,
    });
  }
  return rows;
}

export function resynthesisBlockReason(
  state: JobState,
): MessageDescriptor | null {
  if (state === "DONE") return null;
  if (state === "FAILED") return { key: "review.block.failed" };
  if (state === "CANCELLED") return { key: "review.block.cancelled" };
  if (state === "PAUSED") return { key: "review.block.paused" };
  return { key: "review.block.running" };
}

export function staleExportNotice(count: number): MessageDescriptor | null {
  return count <= 0 ? null : { key: "review.staleExport", params: { count } };
}
