-- The user learning profile (owner ruling R1): pedagogical preferences the
-- user EXPLICITLY stated ("equations: goal first", tone cues), extracted by
-- the update_learning_profile tool and injected compactly into the session
-- prompt. Local-only; the explicit user statement is the consent (R1 — no
-- gate beyond the update being user-initiated).
--
-- Its own migration (0019), not an 0018 edit: 0018 is already pushed and
-- applied to dev databases, and sqlx migrations are append-only once shared
-- (checksums). Its own table, not app_setting rows: the CHECK bounds are the
-- §5 strict-schema discipline extended to the storage layer, and app_setting
-- has no such bounds.
--
-- The bounds mirror the tool-schema constants in voice::tools::registry
-- (PROFILE_KEY_MAX_CHARS / PROFILE_VALUE_MAX_CHARS) as a storage-side
-- backstop; the entry-count cap lives in the store function (SQLite CHECKs
-- cannot count rows).

CREATE TABLE learning_profile (
  key TEXT PRIMARY KEY NOT NULL CHECK (length(key) <= 48),
  value TEXT NOT NULL CHECK (length(value) <= 200),
  updated_at INTEGER NOT NULL
);
