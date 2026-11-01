CREATE TABLE suppressed_block (
  id TEXT PRIMARY KEY NOT NULL,
  job_id TEXT NOT NULL REFERENCES render_job(id) ON DELETE CASCADE,
  page_no INTEGER NOT NULL,
  role TEXT NOT NULL,
  reason TEXT NOT NULL,
  certainty TEXT NOT NULL,
  text TEXT NOT NULL,
  words INTEGER NOT NULL
);

CREATE INDEX idx_suppressed_block_job_id ON suppressed_block(job_id);
