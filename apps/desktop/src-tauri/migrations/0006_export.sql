CREATE TABLE export (
  id TEXT PRIMARY KEY NOT NULL,
  book_id TEXT REFERENCES book(id) ON DELETE SET NULL,
  job_id TEXT REFERENCES render_job(id) ON DELETE SET NULL,
  path TEXT NOT NULL UNIQUE,
  bytes INTEGER NOT NULL DEFAULT 0,
  format TEXT NOT NULL,
  mode TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE INDEX idx_export_created ON export(created_at DESC);
CREATE INDEX idx_export_book ON export(book_id);

INSERT OR IGNORE INTO export (id, book_id, job_id, path, bytes, format, mode, created_at)
SELECT
  lower(hex(randomblob(16))),
  job.book_id,
  job.id,
  entry.value,
  0,
  job.output_format,
  job.mode,
  job.created_at
FROM render_job AS job, json_each(job.output_paths_json) AS entry
WHERE job.output_paths_json IS NOT NULL
  AND json_valid(job.output_paths_json)
  AND json_type(job.output_paths_json) = 'array'
  AND entry.value IS NOT NULL
  AND entry.value <> '';
