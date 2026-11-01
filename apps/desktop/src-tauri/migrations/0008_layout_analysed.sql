-- Whether the layout model was run over this book at import.
--
-- The pass costs 0.25 s/page on a GPU and 11.39 s/page on CPU, so a user on a laptop may
-- reasonably decline it. That decision has to survive: a backfill triggered by an
-- algorithm change must not silently spend two hours re-running something the user turned
-- down, and the library needs to know which books can still be improved.
ALTER TABLE book ADD COLUMN layout_analysed INTEGER NOT NULL DEFAULT 0;
