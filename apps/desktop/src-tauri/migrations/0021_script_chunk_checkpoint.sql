-- S-3b (Hermes MF11): durable completed-chunk outcomes for the cloud Script
-- path. Each row is one chunk's provider-neutral outcome plus the rolling
-- summary AFTER that chunk, persisted at the safe boundary so pause/cancel/
-- resume never re-requests (re-pays) a completed chunk. The fingerprint ties
-- rows to the exact unit set they were generated from; a mismatch on resume
-- discards them rather than resuming against different source text.
CREATE TABLE script_chunk_checkpoint (
    job_id TEXT NOT NULL,
    chunk_idx INTEGER NOT NULL,
    units_fingerprint TEXT NOT NULL,
    total_chunks INTEGER NOT NULL,
    outcome_json TEXT NOT NULL,
    summary TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (job_id, chunk_idx)
);
