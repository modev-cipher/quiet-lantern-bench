-- VOICE_AGENT §3 journal + §7 retention artifact, in two tables because they
-- have two lifetimes: `voice_session` is the renewal-state row (1 per session,
-- read at renewal and relaunch recovery); `voice_transcript` is what the
-- 30-day retention setting and the transcript view govern — pruning deletes
-- transcript rows without touching renewal state.
--
-- Never-persist mode (§7) writes NEITHER table: the runtime holds a
-- memory-only twin of both shapes instead. Gemini session-resumption handles
-- are memory-only ALWAYS (owner ruling R4) — they reference server-side
-- conversation state and never get a column here.
--
-- No segment ids anywhere (cross-render stability): position is
-- `playback_pos_ms` on the book timeline. `job_id` pins which timeline that
-- position belongs to — on resume, a missing/changed job discards the
-- position while the summary stays usable as text.

CREATE TABLE voice_session (
  id TEXT PRIMARY KEY NOT NULL,               -- uuid v7 (the runtime mints one per session)
  book_id TEXT NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  job_id TEXT NOT NULL REFERENCES render_job(id) ON DELETE CASCADE,
  provider_base_url TEXT NOT NULL,            -- key/consent identity, NEVER key material (§7)
  started_at INTEGER NOT NULL,                -- unix ms (storage::now_ms)
  ended_at INTEGER,                           -- NULL while live; set at session end
  playback_pos_ms INTEGER NOT NULL DEFAULT 0, -- book timeline (the item clock IS the book clock)
  summary_text TEXT,                          -- rolling summary, session-LLM output (§3)
  summary_turns INTEGER NOT NULL DEFAULT 0,   -- turns folded into summary_text so far
  CHECK (summary_text IS NULL OR length(summary_text) <= 16384) -- backstop, not the working cap
);
CREATE INDEX idx_voice_session_book ON voice_session(book_id, started_at);

CREATE TABLE voice_transcript (
  session_id TEXT NOT NULL REFERENCES voice_session(id) ON DELETE CASCADE,
  turn_idx INTEGER NOT NULL,                  -- dense per session
  role TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
  text TEXT NOT NULL,
  created_at INTEGER NOT NULL,                -- unix ms
  -- The prompt-side eviction watermark: 1 once folded into summary_text.
  -- Eviction and user-facing retention are different lifetimes — folding a
  -- turn into the summary never deletes its transcript row.
  summarized INTEGER NOT NULL DEFAULT 0 CHECK (summarized IN (0, 1)),
  PRIMARY KEY (session_id, turn_idx)
);
