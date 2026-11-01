import { invoke } from "@tauri-apps/api/core";

import type { JobSummary } from "./jobModel";
import { invalidateHistory } from "./sessionHistoryStore";

// F11-U6 C5 (spec §3 J7.3): a job control as the Jobs panel issues it. A
// confirmed `delete_job` takes the cast's tutor sessions with it (R2 cascade),
// so once the runtime has confirmed the delete the book's history scope is
// invalidated and a mounted rail re-lists. Other controls touch no history.
// Rejections propagate: nothing is invalidated on a refused delete.
export async function runJobControl(
  command: string,
  jobId: string,
  jobs: readonly JobSummary[],
): Promise<void> {
  const bookId =
    command === "delete_job"
      ? jobs.find((job) => job.id === jobId)?.bookId
      : undefined;
  await invoke(command, { jobId });
  if (bookId != null) invalidateHistory({ bookId });
}
