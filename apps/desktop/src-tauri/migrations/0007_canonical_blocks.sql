ALTER TABLE book ADD COLUMN blocks_version TEXT;
ALTER TABLE book ADD COLUMN blocks_page_count INTEGER;

ALTER TABLE page ADD COLUMN needs_upgrade INTEGER NOT NULL DEFAULT 0;

CREATE INDEX idx_page_upgrade ON page(book_id, needs_upgrade);
