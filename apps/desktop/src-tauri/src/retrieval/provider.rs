//! `RetrievalProvider` — provider-neutral whole-book retrieval contract.
//!
//! Productionized from the preserved B-0 spike handoff
//! (`~/.paseo/orchestration/spikes/book-agent-spike/src/provider.rs`) with the
//! charter's app-local hardening: a full generation fingerprint instead of a
//! bare version string, typed provenance on every child, typed unavailable
//! states instead of a stringly backend error, and bounded search options.
//!
//! Signatures stay synchronous on purpose: the voice dispatcher owns the
//! async/NON_BLOCKING envelope, Option A queries are sub-millisecond, and a
//! future Option B fits `spawn_blocking` behind the same trait.

use serde::{Deserialize, Serialize};

/// Typed provenance of an indexed child (charter §3.2). Generated/edited text
/// may improve recall but is never quotable evidence; callers must resolve a
/// hit to imported source before citing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provenance {
    /// Narrator text/heading whose text matches the resolved imported source.
    SourceFaithful,
    /// Generated or edited prose (Teacher explanation, chapter intro, figure
    /// prose, edited rows): retrieval metadata only.
    Generated,
}

/// One retrieval child, projected from a `script_segment` row at index time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentRow {
    pub chapter: u32,
    pub page: u32,
    /// Segment index within the job (`script_segment.idx`) — stable per render,
    /// never stable across renders; the fingerprint pins the render.
    pub idx: u32,
    pub text: String,
    pub provenance: Provenance,
}

/// Internal citation coordinate. Never crosses the LLM tool boundary — the
/// contract layer seals it into an opaque session handle (charter §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SegmentHandle {
    pub chapter: u32,
    pub page: u32,
    pub idx: u32,
}

/// One ranked result. `snippet` is word-capped; `score` is a within-response
/// RRF fusion value, exposed to the model only as ordinal rank.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub handle: SegmentHandle,
    pub snippet: String,
    pub score: f32,
    pub provenance: Provenance,
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// Max hits after fusion. Owner-tunable; too small causes false
    /// "this book does not cover that" refusals.
    pub top_k: usize,
    /// Snippet cap in words. Owner-tunable; charter v1 default is 15 after the
    /// measured 25-word payload overrun (§4.4.6).
    pub snippet_max_words: usize,
    /// Optional chapter scope, applied as a pre-ranking filter and
    /// existence-checked against the index (charter §3.1).
    pub chapter: Option<u32>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            top_k: 5,
            snippet_max_words: 15,
            chapter: None,
        }
    }
}

/// Full generation identity of a per-book index (charter §3.4). Every field
/// must match for an index to be considered fresh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookFingerprint {
    pub book_id: String,
    pub job_id: String,
    pub content_hash: String,
    pub blocks_version: String,
    pub schema_version: u32,
    pub tokenizer_version: u32,
    pub indexer_version: u32,
}

/// Current versions of the index schema/tokenizer/indexer. Bumping any of
/// these invalidates existing indexes and triggers lazy rebuild.
pub const SCHEMA_VERSION: u32 = 1;
pub const TOKENIZER_VERSION: u32 = 1;
pub const INDEXER_VERSION: u32 = 1;

impl BookFingerprint {
    pub fn new(book_id: &str, job_id: &str, content_hash: &str, blocks_version: &str) -> Self {
        Self {
            book_id: book_id.to_string(),
            job_id: job_id.to_string(),
            content_hash: content_hash.to_string(),
            blocks_version: blocks_version.to_string(),
            schema_version: SCHEMA_VERSION,
            tokenizer_version: TOKENIZER_VERSION,
            indexer_version: INDEXER_VERSION,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexStats {
    pub segments: usize,
    pub build_ms: u128,
    pub disk_bytes: u64,
}

/// Provider-neutral failures. Messages are safe codes, never SQL, paths,
/// book text, or backend internals.
#[derive(Debug)]
pub enum RetrievalError {
    /// No published index for this book.
    IndexMissing,
    /// A published index exists but its fingerprint does not match the active
    /// generation; caller should rebuild lazily.
    IndexStale,
    /// The requested chapter scope does not exist in the index.
    UnknownChapter { chapter: u32 },
    /// Query variants or options violate the bounded contract.
    InvalidQuery { code: &'static str },
    /// A handle that does not resolve in the active generation. Refused,
    /// never best-effort remapped (T10).
    UnknownHandle,
    /// The resolved hit is generated prose: retrieval metadata, never an
    /// evidence anchor (T9).
    GeneratedNotEvidence,
    /// Index directory unreadable or structurally invalid.
    Corrupt,
    /// Filesystem/backend failure with a bounded code.
    Backend { code: &'static str },
}

impl std::fmt::Display for RetrievalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IndexMissing => write!(f, "index_missing"),
            Self::IndexStale => write!(f, "index_stale"),
            Self::UnknownChapter { chapter } => write!(f, "unknown_chapter:{chapter}"),
            Self::InvalidQuery { code } => write!(f, "invalid_query:{code}"),
            Self::UnknownHandle => write!(f, "unknown_handle"),
            Self::GeneratedNotEvidence => write!(f, "generated_not_evidence"),
            Self::Corrupt => write!(f, "index_corrupt"),
            Self::Backend { code } => write!(f, "retrieval_backend:{code}"),
        }
    }
}
impl std::error::Error for RetrievalError {}

/// Bounds on the query-variant contract (charter §4.2): 1-4 nonblank,
/// length-capped lexical variants of one question, treated as data.
pub const MAX_QUERY_VARIANTS: usize = 4;
pub const MAX_QUERY_CHARS: usize = 256;

/// Bounds on search options (charter §4.2/§4.4): at most 5 hits, snippets at
/// most 25 words. Out-of-range options are a typed contract violation, not a
/// clamp and never a panic.
pub const MAX_TOP_K: usize = 5;
pub const MAX_SNIPPET_WORDS: usize = 25;

pub fn validate_options(opts: &SearchOptions) -> Result<(), RetrievalError> {
    if opts.top_k == 0 || opts.top_k > MAX_TOP_K {
        return Err(RetrievalError::InvalidQuery { code: "bad_top_k" });
    }
    if opts.snippet_max_words == 0 || opts.snippet_max_words > MAX_SNIPPET_WORDS {
        return Err(RetrievalError::InvalidQuery {
            code: "bad_snippet_cap",
        });
    }
    Ok(())
}

/// Validate and normalize query variants: trim, drop empties after trim is an
/// error (blank variants are a contract violation, not silently skipped),
/// enforce count/length caps, dedupe preserving order.
pub fn validate_queries(queries: &[String]) -> Result<Vec<String>, RetrievalError> {
    if queries.is_empty() {
        return Err(RetrievalError::InvalidQuery { code: "empty" });
    }
    if queries.len() > MAX_QUERY_VARIANTS {
        return Err(RetrievalError::InvalidQuery { code: "too_many" });
    }
    let mut out: Vec<String> = Vec::with_capacity(queries.len());
    for q in queries {
        let t = q.trim();
        if t.is_empty() {
            return Err(RetrievalError::InvalidQuery { code: "blank" });
        }
        if t.chars().count() > MAX_QUERY_CHARS {
            return Err(RetrievalError::InvalidQuery { code: "too_long" });
        }
        if !out.iter().any(|e| e == t) {
            out.push(t.to_string());
        }
    }
    Ok(out)
}

/// The retrieval surface. One instance per app; indexes are per-book on disk
/// under the book's own data directory so book deletion cleans them.
pub trait RetrievalProvider: Send + Sync {
    /// Build the per-book index from projected rows and publish atomically.
    /// Idempotent; a crash never replaces a known-good index with a partial one.
    fn index_book(
        &self,
        fingerprint: &BookFingerprint,
        segments: &[SegmentRow],
    ) -> Result<IndexStats, RetrievalError>;

    /// True iff a published index whose stamp matches `fingerprint` exists.
    fn is_indexed(&self, fingerprint: &BookFingerprint) -> bool;

    /// Multi-query search: 1-4 validated lexical variants of one question,
    /// RRF-fused, deduped by handle, at most `top_k` hits.
    fn search(
        &self,
        fingerprint: &BookFingerprint,
        queries: &[String],
        opts: &SearchOptions,
    ) -> Result<Vec<SearchHit>, RetrievalError>;

    /// Bounded parent window around a hit for `open_passage`: contiguous
    /// same-chapter, source-faithful neighbors, word- and byte-capped.
    /// Never crosses a chapter boundary or includes generated prose.
    fn parent_window(
        &self,
        fingerprint: &BookFingerprint,
        handle: &SegmentHandle,
        max_words: usize,
    ) -> Result<String, RetrievalError>;

    /// Remove the book's index from disk. Idempotent.
    fn delete_index(&self, book_id: &str) -> Result<(), RetrievalError>;
}
