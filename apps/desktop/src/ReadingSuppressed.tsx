import { useState } from "react";
import {
  sortUncertainFirst,
  suppressedDetailLabel,
  type SuppressedBlock,
} from "./readingSuppressedModel";
import { useI18n } from "./i18n";

export default function ReadingSuppressed({
  records,
}: {
  records: SuppressedBlock[];
}) {
  const [open, setOpen] = useState(false);
  const { t, tx } = useI18n();
  if (records.length <= 0) return null;
  // visible copy: the count is a <bdi dir="ltr"> island, no control characters in the text node
  const summary = tx("reader.blocksRemoved", { count: records.length });
  const ordered = sortUncertainFirst(records);
  const SUPPRESSED_CAP = 50;
  const capped = ordered.slice(0, SUPPRESSED_CAP);
  return (
    <div className="shrink-0 border-b border-ink-800/60 px-5 py-2">
      <button
        type="button"
        data-testid="reading-suppressed-toggle"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
        className="text-label text-ink-300 underline-offset-2 hover:underline"
      >
        {summary}
      </button>
      {open && (
        <ul
          data-testid="reading-suppressed-panel"
          className="mt-2 max-h-48 space-y-2 overflow-y-auto"
        >
          {capped.map((record) => (
            <li
              key={record.id}
              data-certainty={record.certainty}
              className="text-label leading-snug"
            >
              <span
                className={
                  record.certainty === "uncertain"
                    ? "text-reading-stale"
                    : "text-ink-400"
                }
              >
                {suppressedDetailLabel(record)}
                {record.certainty === "uncertain"
                  ? t("reader.checkThisOne")
                  : ""}
              </span>
              <span className="block break-words text-ink-200">
                “{record.text}”
              </span>
            </li>
          ))}
          {ordered.length > SUPPRESSED_CAP && (
            <li className="text-label text-ink-400">
              {t("reader.suppressedMore", {
                count: ordered.length - SUPPRESSED_CAP,
              })}
            </li>
          )}
        </ul>
      )}
    </div>
  );
}
