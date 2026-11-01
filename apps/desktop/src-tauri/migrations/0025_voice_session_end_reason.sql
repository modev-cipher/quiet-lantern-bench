-- F11 ruling #32 (2026-09-04): the §9.4 additive exception. Stored sessions
-- keep the typed end reason the runtime already knew, so history rows can
-- render "the provider session expired" / "the connection closed" instead of
-- collapsing every finished row to "ended". CHECK values are pinned equal to
-- `EndReason::ALL` wire strings (voice_session.rs) by a test that reads this
-- file. Rows written before this migration read as 'ended'.
ALTER TABLE voice_session
  ADD COLUMN end_reason TEXT NOT NULL DEFAULT 'ended'
  CHECK (end_reason IN ('ended', 'unexpected', 'expired', 'connection_closed'));
