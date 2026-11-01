//! Option A implementation: tantivy BM25, per-book on-disk index.
//!
//! Productionized from the preserved B-0 spike (`tantivy_provider.rs` in the
//! spike crate) with the charter's hardening:
//! - dual analyzed fields only (`text_en` English stem, `text_ar` Arabic stem);
//!   the spike's probe-only `text_nostem` field is dropped (§3.6);
//! - atomic publish: build into a temp dir, `rename` into place; a crash never
//!   replaces a known-good index with a partial one (§3.4.3);
//! - full-fingerprint stamp (schema/tokenizer/indexer versions, book, job,
//!   content hash, blocks_version) in `fingerprint.json`, verified strictly (§3.4.2);
//! - chapter scope as a pre-ranking `TermQuery` conjunction, existence-checked;
//!   the spike's post-top-k filter is a named defect and is not reproduced (§3.1);
//! - generation-pinned cached readers: an atomic publish affects only
//!   subsequent reader acquisition (§3.4.8).
//!
//! Analyzer law (§3.5): analyzers are registered once on the Index and the
//! QueryParser is built from that same Index, so index-time and query-time
//! analysis cannot drift (tantivy#718 shape). Same text into both stem fields,
//! both fields searched every time, no language detection.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, INDEXED, STORED,
};
use tantivy::tokenizer::{
    Language, LowerCaser, RemoveLongFilter, SimpleTokenizer, Stemmer, TextAnalyzer,
};
use tantivy::{doc, Index, IndexReader, TantivyDocument, Term};

use super::provider::*;

/// RRF constant. k=20-30 measured better than the canonical 60 at single-book
/// corpus scale; owner-tunable (D11).
const RRF_K: f32 = 25.0;

/// Hard byte backstop for a parent window (charter §3.3).
const HARD_WINDOW_BYTES: usize = 64 * 1024;

/// Hard byte backstop for one snippet, against adversarial no-space Unicode.
const HARD_SNIPPET_BYTES: usize = 512;

const INDEX_DIR: &str = "index";
const CURRENT_DIR: &str = "current";
const RETIRED_DIR: &str = ".retired";
// NOT "meta.json": tantivy owns that name inside its index directory.
const META_FILE: &str = "fingerprint.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct IndexMeta {
    fingerprint: BookFingerprint,
    chapters: Vec<u32>,
    segments: usize,
}

pub struct TantivyRetrieval {
    /// The books root (`app_data_dir()/books`); each index lives inside the
    /// book's own folder so `delete_book` removes it with the book.
    root: PathBuf,
    readers: Mutex<HashMap<String, CachedReader>>,
}

struct CachedReader {
    fingerprint: BookFingerprint,
    index: Index,
    reader: IndexReader,
    chapters: HashSet<u32>,
}

/// A generation-pinned snapshot handed out of the cache; `Index` and
/// `IndexReader` are cheaply clonable handles over the same mmaps.
struct ReaderView {
    index: Index,
    reader: IndexReader,
    chapters: HashSet<u32>,
}

struct Fields {
    chapter: Field,
    page: Field,
    idx: Field,
    provenance: Field,
    text: Field,
    text_en: Field,
    text_ar: Field,
}

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let indexed_text = |tok: &str| {
        TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(tok)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        )
    };
    let fields = Fields {
        chapter: b.add_u64_field("chapter", INDEXED | STORED),
        page: b.add_u64_field("page", STORED),
        idx: b.add_u64_field("idx", INDEXED | STORED),
        provenance: b.add_u64_field("provenance", STORED),
        text: b.add_text_field("text", STORED),
        text_en: b.add_text_field("text_en", indexed_text("stem_en")),
        text_ar: b.add_text_field("text_ar", indexed_text("stem_ar")),
    };
    (b.build(), fields)
}

fn register_tokenizers(index: &Index) {
    let base = || {
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(RemoveLongFilter::limit(40))
            .filter(LowerCaser)
    };
    index.tokenizers().register(
        "stem_en",
        base().filter(Stemmer::new(Language::English)).build(),
    );
    index.tokenizers().register(
        "stem_ar",
        base().filter(Stemmer::new(Language::Arabic)).build(),
    );
}

fn provenance_to_u64(p: Provenance) -> u64 {
    match p {
        Provenance::SourceFaithful => 0,
        Provenance::Generated => 1,
    }
}

fn provenance_from_u64(v: u64) -> Provenance {
    if v == 0 {
        Provenance::SourceFaithful
    } else {
        Provenance::Generated
    }
}

impl TantivyRetrieval {
    pub fn new(books_root: impl Into<PathBuf>) -> Self {
        Self {
            root: books_root.into(),
            readers: Mutex::new(HashMap::new()),
        }
    }

    fn index_dir(&self, book_id: &str) -> PathBuf {
        self.root.join(book_id).join(INDEX_DIR)
    }

    fn current_dir(&self, book_id: &str) -> PathBuf {
        self.index_dir(book_id).join(CURRENT_DIR)
    }

    /// Poison-tolerant lock: a panic inside a past holder must not brick
    /// retrieval for the rest of the app session. The cache map itself stays
    /// coherent (entries are only inserted whole or removed).
    fn readers_lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, CachedReader>> {
        self.readers.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn read_meta(&self, book_id: &str) -> Option<IndexMeta> {
        self.restore_retired_if_needed(book_id);
        let raw = std::fs::read_to_string(self.current_dir(book_id).join(META_FILE)).ok()?;
        serde_json::from_str(&raw).ok()
    }

    /// A crash between the two publish renames leaves `.retired` but no
    /// `current`; restore the known-good generation rather than losing it.
    fn restore_retired_if_needed(&self, book_id: &str) {
        let current = self.current_dir(book_id);
        let retired = self.index_dir(book_id).join(RETIRED_DIR);
        if !current.exists() && retired.exists() {
            let _ = std::fs::rename(&retired, &current);
        }
    }

    /// Acquire a generation-pinned reader: reuse the cache only when its
    /// fingerprint matches; otherwise (re)open from the published directory
    /// after strict meta verification.
    fn acquire(&self, fingerprint: &BookFingerprint) -> Result<(), RetrievalError> {
        {
            let readers = self.readers_lock();
            if let Some(c) = readers.get(&fingerprint.book_id) {
                if &c.fingerprint == fingerprint {
                    return Ok(());
                }
            }
        }
        let meta = match self.read_meta(&fingerprint.book_id) {
            Some(m) => m,
            None => {
                return if self.current_dir(&fingerprint.book_id).exists() {
                    Err(RetrievalError::Corrupt)
                } else {
                    Err(RetrievalError::IndexMissing)
                };
            }
        };
        if &meta.fingerprint != fingerprint {
            return Err(RetrievalError::IndexStale);
        }
        let index = Index::open_in_dir(self.current_dir(&fingerprint.book_id))
            .map_err(|_| RetrievalError::Corrupt)?;
        register_tokenizers(&index);
        let reader = index.reader().map_err(|_| RetrievalError::Corrupt)?;
        let mut readers = self.readers_lock();
        readers.insert(
            fingerprint.book_id.clone(),
            CachedReader {
                fingerprint: fingerprint.clone(),
                index,
                reader,
                chapters: meta.chapters.into_iter().collect(),
            },
        );
        Ok(())
    }

    /// Clone the generation-pinned view out of the cache so no lock is held
    /// while a query runs: a panic mid-query cannot poison the cache guard,
    /// and searches on other books do not serialize behind this one.
    fn reader_view(&self, fingerprint: &BookFingerprint) -> Result<ReaderView, RetrievalError> {
        self.acquire(fingerprint)?;
        let readers = self.readers_lock();
        let cached = readers
            .get(&fingerprint.book_id)
            .filter(|c| &c.fingerprint == fingerprint)
            .ok_or(RetrievalError::IndexStale)?;
        Ok(ReaderView {
            index: cached.index.clone(),
            reader: cached.reader.clone(),
            chapters: cached.chapters.clone(),
        })
    }

    fn fetch_by_idx(
        searcher: &tantivy::Searcher,
        f: &Fields,
        chapter: u32,
        idx: u32,
    ) -> Result<Option<(TantivyDocument, Provenance)>, RetrievalError> {
        let q = BooleanQuery::new(vec![
            (
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_u64(f.idx, idx as u64),
                    IndexRecordOption::Basic,
                )) as Box<dyn Query>,
            ),
            (
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_u64(f.chapter, chapter as u64),
                    IndexRecordOption::Basic,
                )) as Box<dyn Query>,
            ),
        ]);
        let hits = searcher
            .search(&q, &TopDocs::with_limit(1).order_by_score())
            .map_err(|_| RetrievalError::Backend { code: "search" })?;
        let Some((_, addr)) = hits.into_iter().next() else {
            return Ok(None);
        };
        let d: TantivyDocument = searcher
            .doc(addr)
            .map_err(|_| RetrievalError::Backend { code: "doc" })?;
        let prov = provenance_from_u64(
            d.get_first(f.provenance)
                .and_then(|v| v.as_u64())
                .unwrap_or(1),
        );
        Ok(Some((d, prov)))
    }
}

impl RetrievalProvider for TantivyRetrieval {
    fn index_book(
        &self,
        fingerprint: &BookFingerprint,
        segments: &[SegmentRow],
    ) -> Result<IndexStats, RetrievalError> {
        let t0 = Instant::now();
        let index_root = self.index_dir(&fingerprint.book_id);
        std::fs::create_dir_all(&index_root)
            .map_err(|_| RetrievalError::Backend { code: "mkdir" })?;

        // Build into a temp dir beside the publish target (same filesystem, so
        // the final rename is atomic), then swap.
        let tmp = tempfile::Builder::new()
            .prefix(".build-")
            .tempdir_in(&index_root)
            .map_err(|_| RetrievalError::Backend { code: "tmpdir" })?;

        let (sch, f) = schema();
        let index = Index::create_in_dir(tmp.path(), sch)
            .map_err(|_| RetrievalError::Backend { code: "create" })?;
        register_tokenizers(&index);
        let mut writer = index
            .writer(32_000_000)
            .map_err(|_| RetrievalError::Backend { code: "writer" })?;
        let mut chapters: HashSet<u32> = HashSet::new();
        for s in segments {
            chapters.insert(s.chapter);
            writer
                .add_document(doc!(
                    f.chapter => s.chapter as u64,
                    f.page => s.page as u64,
                    f.idx => s.idx as u64,
                    f.provenance => provenance_to_u64(s.provenance),
                    f.text => s.text.clone(),
                    f.text_en => s.text.clone(),
                    f.text_ar => s.text.clone(),
                ))
                .map_err(|_| RetrievalError::Backend { code: "add_doc" })?;
        }
        writer
            .commit()
            .map_err(|_| RetrievalError::Backend { code: "commit" })?;
        drop(writer);

        let mut sorted_chapters: Vec<u32> = chapters.into_iter().collect();
        sorted_chapters.sort_unstable();
        let meta = IndexMeta {
            fingerprint: fingerprint.clone(),
            chapters: sorted_chapters,
            segments: segments.len(),
        };
        std::fs::write(
            tmp.path().join(META_FILE),
            serde_json::to_vec(&meta).map_err(|_| RetrievalError::Backend { code: "meta" })?,
        )
        .map_err(|_| RetrievalError::Backend { code: "meta_write" })?;

        // Atomic publish: retire the old generation out of the way first, then
        // rename the finished build into place. Readers already open on the old
        // generation keep their mmaps; only subsequent acquisition sees the new one.
        let current = self.current_dir(&fingerprint.book_id);
        let retired = index_root.join(RETIRED_DIR);
        let _ = std::fs::remove_dir_all(&retired);
        if current.exists() {
            std::fs::rename(&current, &retired)
                .map_err(|_| RetrievalError::Backend { code: "retire" })?;
        }
        let tmp_path = tmp.keep();
        if let Err(_e) = std::fs::rename(&tmp_path, &current) {
            let _ = std::fs::rename(&retired, &current);
            let _ = std::fs::remove_dir_all(&tmp_path);
            return Err(RetrievalError::Backend { code: "publish" });
        }
        let _ = std::fs::remove_dir_all(&retired);

        // Invalidate any cached reader for this book; next search re-acquires.
        self.readers_lock().remove(&fingerprint.book_id);

        let disk_bytes = dir_size(&current);
        Ok(IndexStats {
            segments: segments.len(),
            build_ms: t0.elapsed().as_millis(),
            disk_bytes,
        })
    }

    fn is_indexed(&self, fingerprint: &BookFingerprint) -> bool {
        self.read_meta(&fingerprint.book_id)
            .map(|m| &m.fingerprint == fingerprint)
            .unwrap_or(false)
    }

    fn search(
        &self,
        fingerprint: &BookFingerprint,
        queries: &[String],
        opts: &SearchOptions,
    ) -> Result<Vec<SearchHit>, RetrievalError> {
        let queries = validate_queries(queries)?;
        validate_options(opts)?;
        let view = self.reader_view(fingerprint)?;
        let (_, f) = schema();
        let f = &f;
        {
            if let Some(ch) = opts.chapter {
                if !view.chapters.contains(&ch) {
                    return Err(RetrievalError::UnknownChapter { chapter: ch });
                }
            }
            let searcher = view.reader.searcher();
            let parser = QueryParser::for_index(&view.index, vec![f.text_en, f.text_ar]);

            // RRF fusion across variants, keyed by segment idx. Chapter scope is
            // conjoined BEFORE TopDocs collection (pre-ranking filter law).
            let mut fused: HashMap<u32, (f32, SegmentHandle, String, Provenance)> = HashMap::new();
            for q_str in &queries {
                let (parsed, _errs) = parser.parse_query_lenient(q_str);
                let q: Box<dyn Query> = match opts.chapter {
                    Some(ch) => Box::new(BooleanQuery::new(vec![
                        (Occur::Must, parsed),
                        (
                            Occur::Must,
                            Box::new(TermQuery::new(
                                Term::from_field_u64(f.chapter, ch as u64),
                                IndexRecordOption::Basic,
                            )),
                        ),
                    ])),
                    None => parsed,
                };
                let hits = searcher
                    .search(&q, &TopDocs::with_limit(opts.top_k * 4).order_by_score())
                    .map_err(|_| RetrievalError::Backend { code: "search" })?;
                for (rank, (_score, addr)) in hits.into_iter().enumerate() {
                    let d: TantivyDocument = searcher
                        .doc(addr)
                        .map_err(|_| RetrievalError::Backend { code: "doc" })?;
                    let get_u = |field| d.get_first(field).and_then(|v| v.as_u64()).unwrap_or(0);
                    let handle = SegmentHandle {
                        chapter: get_u(f.chapter) as u32,
                        page: get_u(f.page) as u32,
                        idx: get_u(f.idx) as u32,
                    };
                    let text = d
                        .get_first(f.text)
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let prov = provenance_from_u64(get_u(f.provenance));
                    let contrib = 1.0 / (RRF_K + rank as f32 + 1.0);
                    fused
                        .entry(handle.idx)
                        .and_modify(|e| e.0 += contrib)
                        .or_insert((contrib, handle, text, prov));
                }
            }
            let mut ranked: Vec<_> = fused.into_values().collect();
            ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            ranked.truncate(opts.top_k);
            Ok(ranked
                .into_iter()
                .map(|(score, handle, text, provenance)| SearchHit {
                    handle,
                    snippet: snippet(&text, &queries, opts.snippet_max_words),
                    score,
                    provenance,
                })
                .collect())
        }
    }

    fn parent_window(
        &self,
        fingerprint: &BookFingerprint,
        handle: &SegmentHandle,
        max_words: usize,
    ) -> Result<String, RetrievalError> {
        if max_words == 0 {
            return Err(RetrievalError::InvalidQuery { code: "bad_window" });
        }
        let view = self.reader_view(fingerprint)?;
        let (_, f) = schema();
        let f = &f;
        {
            let searcher = view.reader.searcher();
            let Some((center_doc, center_prov)) =
                Self::fetch_by_idx(&searcher, f, handle.chapter, handle.idx)?
            else {
                return Err(RetrievalError::UnknownHandle);
            };
            // A generated hit is retrieval metadata; it cannot anchor an
            // evidence window (charter §3.2, T9). Callers resolve its source.
            if center_prov == Provenance::Generated {
                return Err(RetrievalError::GeneratedNotEvidence);
            }
            let text_of = |d: &TantivyDocument| {
                d.get_first(f.text)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            // The center segment itself is capped: §3.3 drops whole outer
            // children first, but an oversized single segment (adversarial or
            // badly segmented import) must still respect the word budget and
            // the 64 KiB hard backstop, so text truncation is the last resort.
            let center_text = cap_text(&text_of(&center_doc), max_words, HARD_WINDOW_BYTES);
            let mut parts: std::collections::VecDeque<String> = std::collections::VecDeque::new();
            let mut words = center_text.split_whitespace().count();
            let mut bytes = center_text.len();
            parts.push_back(center_text);

            // Alternating expansion over contiguous same-chapter neighbors,
            // dropping whole outer children rather than truncating text.
            // Expansion stops at chapter boundaries and at generated prose.
            // Neighbors are probed at idx +/- 1: the scripting writer emits
            // contiguous indices per job, and a gap (which UNIQUE(job_id,idx)
            // does not forbid) simply under-expands rather than erroring.
            let (mut lo, mut hi) = (handle.idx, handle.idx);
            let (mut lo_open, mut hi_open) = (handle.idx > 0, true);
            while lo_open || hi_open {
                let mut grew = false;
                for side in [true, false] {
                    let (open, next) = if side {
                        (&mut lo_open, lo.checked_sub(1))
                    } else {
                        (&mut hi_open, hi.checked_add(1))
                    };
                    if !*open {
                        continue;
                    }
                    let Some(next_idx) = next else {
                        *open = false;
                        continue;
                    };
                    match Self::fetch_by_idx(&searcher, f, handle.chapter, next_idx)? {
                        Some((d, Provenance::SourceFaithful)) => {
                            let t = text_of(&d);
                            let w = t.split_whitespace().count();
                            if words + w > max_words || bytes + t.len() + 1 > HARD_WINDOW_BYTES {
                                *open = false;
                                continue;
                            }
                            words += w;
                            bytes += t.len() + 1;
                            if side {
                                lo = next_idx;
                                parts.push_front(t);
                            } else {
                                hi = next_idx;
                                parts.push_back(t);
                            }
                            grew = true;
                        }
                        _ => *open = false,
                    }
                }
                if !grew {
                    break;
                }
            }
            Ok(parts.into_iter().collect::<Vec<_>>().join("\n"))
        }
    }

    fn delete_index(&self, book_id: &str) -> Result<(), RetrievalError> {
        self.readers_lock().remove(book_id);
        let dir = self.index_dir(book_id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .map_err(|_| RetrievalError::Backend { code: "delete" })?;
        }
        Ok(())
    }
}

/// Truncate to at most `max_words` whitespace words and `max_bytes` bytes at a
/// char boundary. Word-boundary drop first; byte truncation is the backstop.
fn cap_text(text: &str, max_words: usize, max_bytes: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut s = if words.len() > max_words {
        words[..max_words].join(" ")
    } else {
        text.to_string()
    };
    if s.len() > max_bytes {
        let cut = (0..=max_bytes)
            .rev()
            .find(|&i| s.is_char_boundary(i))
            .unwrap_or(0);
        s.truncate(cut);
    }
    s
}

/// Keyword-centered snippet: a window of at most `max_words` around the first
/// query-term occurrence, with a hard byte backstop against adversarial
/// no-whitespace input. The cap is the law; centering is best-effort. The
/// ellipsis markers are appended after the byte cap, so the serialized value
/// may exceed `HARD_SNIPPET_BYTES` by at most 6 marker bytes.
fn snippet(text: &str, queries: &[String], max_words: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let lower: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
    let terms: Vec<String> = queries
        .iter()
        .flat_map(|q| q.split_whitespace())
        .map(|t| t.to_lowercase())
        .filter(|t| t.chars().count() > 2)
        .collect();
    let center = lower
        .iter()
        .position(|w| terms.iter().any(|t| w.contains(t.as_str())))
        .unwrap_or(0);
    let start = center.saturating_sub(max_words / 2);
    let end = (start + max_words).min(words.len());
    let mut s = words[start..end].join(" ");
    if s.len() > HARD_SNIPPET_BYTES {
        let cut = (0..=HARD_SNIPPET_BYTES)
            .rev()
            .find(|&i| s.is_char_boundary(i))
            .unwrap_or(0);
        s.truncate(cut);
    }
    if start > 0 {
        s = format!("…{s}");
    }
    if end < words.len() {
        s.push('…');
    }
    s
}

fn dir_size(p: &Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            total += if path.is_dir() {
                dir_size(&path)
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            };
        }
    }
    total
}
