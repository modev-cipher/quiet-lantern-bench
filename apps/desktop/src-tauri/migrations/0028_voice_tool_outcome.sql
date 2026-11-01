-- 4b (owner ruling 2026-09-28): complete tool-execution observability —
-- one row per ANSWERED tool call, written by the session harness (the
-- journal's sole-writer rule, VOICE_AGENT §3). Would have settled the
-- 2026-09-28 field questions post-hoc: did the model call a navigation
-- tool at all, and what did it answer?
CREATE TABLE voice_tool_outcome (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL REFERENCES voice_session(id) ON DELETE CASCADE,
  tool TEXT NOT NULL,
  call_id TEXT NOT NULL,
  ok INTEGER NOT NULL,
  code TEXT,
  pos_ms INTEGER,
  created_at INTEGER NOT NULL
);

CREATE INDEX idx_voice_tool_outcome_session
  ON voice_tool_outcome(session_id, created_at);
