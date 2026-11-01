ALTER TABLE script_segment ADD COLUMN original_text TEXT;

CREATE TABLE chapter_render (
  job_id TEXT NOT NULL REFERENCES render_job(id) ON DELETE CASCADE,
  chapter_id TEXT NOT NULL REFERENCES chapter(id) ON DELETE CASCADE,
  idx INTEGER NOT NULL,
  hash TEXT NOT NULL,
  artifact TEXT NOT NULL CHECK (artifact IN ('mp3', 'none')),
  duration_ms INTEGER NOT NULL,
  encoded_ms INTEGER,
  CHECK ((artifact = 'mp3') = (encoded_ms IS NOT NULL)),
  PRIMARY KEY (job_id, chapter_id)
);
