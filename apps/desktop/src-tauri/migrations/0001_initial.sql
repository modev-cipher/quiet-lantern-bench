CREATE TABLE book (
  id TEXT PRIMARY KEY NOT NULL,
  title TEXT NOT NULL,
  author TEXT,
  source_path TEXT NOT NULL,
  format TEXT NOT NULL CHECK (format IN ('pdf','epub')),
  page_count INTEGER,
  language TEXT,
  is_scanned INTEGER NOT NULL DEFAULT 0,
  ingest_path TEXT,
  cover_path TEXT,
  content_hash TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE chapter (
  id TEXT PRIMARY KEY NOT NULL,
  book_id TEXT NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  idx INTEGER NOT NULL,
  title TEXT NOT NULL,
  page_start INTEGER NOT NULL,
  page_end INTEGER NOT NULL,
  is_pseudo INTEGER NOT NULL DEFAULT 0,
  word_count INTEGER
);

CREATE TABLE page (
  id TEXT PRIMARY KEY NOT NULL,
  book_id TEXT NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  page_no INTEGER NOT NULL,
  width_pt REAL, height_pt REAL,
  render_hash TEXT,
  text_json TEXT,
  UNIQUE (book_id, page_no)
);

CREATE TABLE figure_region (
  id TEXT PRIMARY KEY NOT NULL,
  book_id TEXT NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  page_no INTEGER NOT NULL,
  bbox_json TEXT NOT NULL,
  type TEXT NOT NULL CHECK (type IN ('figure','chart','table','equation','decorative')),
  confidence REAL NOT NULL,
  crop_hash TEXT,
  caption TEXT,
  explanation TEXT,
  caption_confidence REAL
);

CREATE TABLE voice_profile (
  id TEXT PRIMARY KEY NOT NULL,
  name TEXT NOT NULL,
  engine TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('preset','cloned')),
  preset_voice_id TEXT,
  reference_wav_path TEXT,
  params_json TEXT,
  created_at INTEGER NOT NULL
);

CREATE TABLE render_job (
  id TEXT PRIMARY KEY NOT NULL,
  book_id TEXT NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  scope_json TEXT NOT NULL,
  mode TEXT NOT NULL CHECK (mode IN ('narrator','teacher')),
  voice_profile_id TEXT NOT NULL REFERENCES voice_profile(id),
  output_format TEXT NOT NULL CHECK (output_format IN ('mp3','mp4')),
  model_overrides_json TEXT,
  resource_plan_json TEXT,
  state TEXT NOT NULL,
  checkpoint_json TEXT,
  overnight INTEGER NOT NULL DEFAULT 0,
  eta_seconds INTEGER,
  report_json TEXT,
  output_paths_json TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE script_segment (
  id TEXT PRIMARY KEY NOT NULL,
  job_id TEXT NOT NULL REFERENCES render_job(id) ON DELETE CASCADE,
  chapter_id TEXT NOT NULL REFERENCES chapter(id) ON DELETE CASCADE,
  idx INTEGER NOT NULL,
  page_no INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('text','figure_cue','heading','chapter_intro')),
  figure_id TEXT REFERENCES figure_region(id) ON DELETE SET NULL,
  text TEXT NOT NULL,
  source_span_json TEXT,
  flags_json TEXT,
  audio_hash TEXT,
  duration_ms INTEGER,
  UNIQUE (job_id, idx)
);

CREATE TABLE alignment (
  segment_id TEXT PRIMARY KEY NOT NULL REFERENCES script_segment(id) ON DELETE CASCADE,
  source TEXT NOT NULL CHECK (source IN ('tts_native','whisperx')),
  words_json TEXT NOT NULL,
  wer REAL
);

CREATE TABLE model_install (
  model_id TEXT PRIMARY KEY NOT NULL,
  path TEXT NOT NULL,
  sha256 TEXT NOT NULL,
  disk_bytes INTEGER NOT NULL,
  source TEXT NOT NULL DEFAULT 'curated',
  installed_at INTEGER NOT NULL
);

CREATE TABLE hw_benchmark (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  profile_json TEXT NOT NULL,
  compute_score INTEGER NOT NULL,
  tok_s_gpu REAL, tok_s_cpu REAL,
  rtf_table_json TEXT,
  measured_at INTEGER NOT NULL
);

CREATE TABLE cache_entry (
  hash TEXT PRIMARY KEY NOT NULL,
  kind TEXT NOT NULL,
  bytes INTEGER NOT NULL,
  last_used_at INTEGER NOT NULL
);

CREATE INDEX idx_chapter_book ON chapter(book_id, idx);
CREATE INDEX idx_page_book ON page(book_id, page_no);
CREATE INDEX idx_figure_region_book ON figure_region(book_id, page_no);
CREATE INDEX idx_script_segment_job ON script_segment(job_id, idx);
CREATE INDEX idx_cache_entry_lru ON cache_entry(last_used_at);
