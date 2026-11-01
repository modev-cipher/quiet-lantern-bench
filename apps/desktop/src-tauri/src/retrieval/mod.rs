//! Book Agent whole-book retrieval (charter B-0).
//!
//! Everything here is compiled only under the `book-agent` feature and is not
//! advertised by any production session. The module lives outside `src/voice/`
//! on purpose: the voice excerpt source scan forbids page-text/DB surfaces
//! inside the voice tree, so retrieval reads `script_segment` here and hands
//! the voice layer already-bounded structs, mirroring `SessionSnapshot`.
//!
//! Layout:
//! - `provider` — the object-safe `RetrievalProvider` trait and app-local types.
//! - `tantivy_provider` — Option A implementation (tantivy BM25, dual
//!   English/Arabic analyzed fields, atomic publish, generation-pinned readers).
//! - `hook` — ingestion-completion index build, lazy `ensure_indexed` rebuild,
//!   and deletion cleanup, reading real `script_segment` rows.
//! - `contract` — the fixture-harness `search_book`/`open_passage` contracts:
//!   variant validation, opaque handle table, bounded serialization. Sentinel
//!   wrapping stays at the voice boundary (shared machinery, never copied).
//! - `fixture` — the preserved B-0 spike fixture, used by tests and the
//!   `book-agent-bench` regression harness.

pub mod agent_port;
pub mod contract;
pub mod fixture;
pub mod hook;
pub mod provider;
pub mod tantivy_provider;

pub use provider::{
    BookFingerprint, IndexStats, Provenance, RetrievalError, RetrievalProvider, SearchHit,
    SearchOptions, SegmentHandle, SegmentRow,
};
pub use tantivy_provider::TantivyRetrieval;
