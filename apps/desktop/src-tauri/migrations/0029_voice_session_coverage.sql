-- Review 2026-10-09 (T3): what a tutor session covered survives the
-- session, so Continue knows which pages it already read and which it
-- skipped. JSON: {"units":"offset"|"ms","ranges":[[start,end],...]} — the
-- session's played/visited ranges in its own position units. NULL for
-- sessions before this migration and for never-persist sessions.
ALTER TABLE voice_session ADD COLUMN coverage_json TEXT
  CHECK (coverage_json IS NULL OR length(coverage_json) <= 65536);
