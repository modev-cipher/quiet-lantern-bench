-- F6 standalone voice: `voice_session.book_id`/`job_id` become NULLABLE so a
-- standalone session (no book, no render job) journals identically to a
-- book-grounded one (docs/f6-standalone-voice-design.md §2.4). SQLite cannot
-- drop NOT NULL in place, so this is the documented table rebuild
-- (sqlite.org/lang_altertable.html §7, checked 2026-09-01). The migrator runs
-- on a dedicated foreign_keys=OFF connection (storage::open_pool) — with FKs
-- ON, the DROP below would cascade-delete every `voice_transcript` row.
CREATE TABLE voice_session_new (
  id TEXT PRIMARY KEY NOT NULL,
  book_id TEXT REFERENCES book(id) ON DELETE CASCADE,
  job_id TEXT REFERENCES render_job(id) ON DELETE CASCADE,
  provider_base_url TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  ended_at INTEGER,
  playback_pos_ms INTEGER NOT NULL DEFAULT 0,
  summary_text TEXT,
  summary_turns INTEGER NOT NULL DEFAULT 0,
  CHECK (summary_text IS NULL OR length(summary_text) <= 16384)
);

INSERT INTO voice_session_new
  SELECT id, book_id, job_id, provider_base_url, started_at, ended_at,
         playback_pos_ms, summary_text, summary_turns
  FROM voice_session;

DROP TABLE voice_session;
ALTER TABLE voice_session_new RENAME TO voice_session;

CREATE INDEX idx_voice_session_book
  ON voice_session(book_id, started_at) WHERE book_id IS NOT NULL;
