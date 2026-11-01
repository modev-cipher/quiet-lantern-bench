CREATE TABLE cache_ref (
  hash TEXT NOT NULL REFERENCES cache_entry(hash) ON DELETE CASCADE,
  job_id TEXT NOT NULL REFERENCES render_job(id) ON DELETE CASCADE,
  PRIMARY KEY (hash, job_id)
);

CREATE INDEX idx_cache_ref_job ON cache_ref(job_id);

INSERT OR IGNORE INTO cache_ref (hash, job_id)
SELECT s.audio_hash, s.job_id
FROM script_segment s
JOIN render_job j ON j.id = s.job_id
JOIN cache_entry c ON c.hash = s.audio_hash
WHERE s.audio_hash IS NOT NULL AND j.state NOT IN ('DONE', 'FAILED', 'CANCELLED');

INSERT OR IGNORE INTO cache_ref (hash, job_id)
SELECT f.crop_hash, j.id
FROM figure_region f
JOIN render_job j ON j.book_id = f.book_id
JOIN cache_entry c ON c.hash = f.crop_hash
WHERE f.crop_hash IS NOT NULL AND j.state NOT IN ('DONE', 'FAILED', 'CANCELLED');
