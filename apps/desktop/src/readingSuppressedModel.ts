import { t, type MessageKey } from "./i18n";
export interface SuppressedBlock {
  id: string;
  jobId: string;
  pageNo: number;
  role: string;
  reason: string;
  certainty: string;
  text: string;
  words: number;
}

export function sortUncertainFirst(
  records: SuppressedBlock[],
): SuppressedBlock[] {
  return [...records].sort((a, b) => {
    const ua = a.certainty === "uncertain" ? 0 : 1;
    const ub = b.certainty === "uncertain" ? 0 : 1;
    if (ua !== ub) return ua - ub;
    return a.pageNo - b.pageNo;
  });
}

export function suppressedSummaryLabel(count: number): string | null {
  if (count <= 0) return null;
  return t("reader.blocksRemoved", { count });
}

const REASON_KEY: Record<string, MessageKey> = {
  repetition: "reader.suppressedRepetition",
  "layout-label": "reader.suppressedLayoutLabel",
  "artifact-mark": "reader.suppressedArtifactMark",
};

export function suppressedDetailLabel(record: SuppressedBlock): string {
  const key = REASON_KEY[record.reason];
  const reason = key ? t(key) : record.reason;
  return `p. ${record.pageNo} · ${record.role} · ${reason}`;
}
