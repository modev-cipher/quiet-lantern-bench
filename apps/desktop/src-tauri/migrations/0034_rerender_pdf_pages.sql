UPDATE page
SET render_hash = NULL
WHERE render_hash IS NOT NULL
  AND book_id IN (SELECT id FROM book WHERE format = 'pdf');
