-- Make the extraction lease owned and expiring rather than a bare flag.
--
-- An unconditional release lets one holder clear another's lease: a second app instance on
-- the same database wiped a live lease at start-up, after which jobs and a second pass
-- began underneath a running one. Ownership fixes the release side; an age threshold fixes
-- the recovery side without a start-up sweep that cannot tell a crashed holder from a
-- running one in another process.
--
-- The holder refreshes extracting_since while it works, so a live lease never ages out and
-- a dead one does. Recovery is therefore bounded by the threshold instead of requiring a
-- restart, and no sweep can kill work that is still running.
ALTER TABLE book ADD COLUMN extracting_owner TEXT;

-- Existing EPUBs predate the reflowed column and would otherwise be offered a layout pass
-- forever: the pass cannot run on reflowed pages, so layout_pass_ran stays false, the flag
-- stays 0, and the menu item never goes away. Written here rather than by editing 0009,
-- which has already been applied to working databases and whose checksum must not change.
UPDATE book SET reflowed = 1 WHERE format = 'epub';
