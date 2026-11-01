-- Exclusive admission for the two writers that can replace a book's blocks.
--
-- The job worker and the library command thread are separate futures. Checking "is a job
-- running" and then starting a re-extraction is two reads and a write, so both sides can
-- observe a free book and proceed; the window is the whole multi-hour pass. The only
-- serialisation point they already share is SQLite's write lock, so the claim is taken in
-- one IMMEDIATE transaction that inspects jobs and writes the lease together.
--
-- NULL means idle. A timestamp means a pass is in flight. Any lease present at startup is
-- stale by construction: the holder is an in-process future, so if the process is starting
-- there is nothing running, and the sweep clears it. A hosted deployment would need an
-- owner and an expiry here instead.
ALTER TABLE book ADD COLUMN extracting_since INTEGER;

-- Whether the source paginates. Known from the probe at import and immutable thereafter,
-- so the menu can withhold an action that would never do anything instead of discovering
-- it after a round trip.
ALTER TABLE book ADD COLUMN reflowed INTEGER NOT NULL DEFAULT 0;
