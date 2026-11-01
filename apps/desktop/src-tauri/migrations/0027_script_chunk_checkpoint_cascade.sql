-- TC-STOR-02: Rebuild script_chunk_checkpoint with foreign key cascade to render_job(id)
CREATE TABLE script_chunk_checkpoint_new (
    job_id TEXT NOT NULL REFERENCES render_job(id) ON DELETE CASCADE,
    chunk_idx INTEGER NOT NULL,
    units_fingerprint TEXT NOT NULL,
    total_chunks INTEGER NOT NULL,
    outcome_json TEXT NOT NULL,
    summary TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (job_id, chunk_idx)
);

INSERT INTO script_chunk_checkpoint_new
    SELECT job_id, chunk_idx, units_fingerprint, total_chunks, outcome_json, summary, created_at
    FROM script_chunk_checkpoint;

DROP TABLE script_chunk_checkpoint;
ALTER TABLE script_chunk_checkpoint_new RENAME TO script_chunk_checkpoint;
