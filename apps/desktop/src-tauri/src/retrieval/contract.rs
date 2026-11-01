//! Fixture-harness `search_book` / `open_passage` contracts (charter B-0).
//!
//! No production session advertises these yet; the B-1/B-2 dispatcher will
//! route real tool calls here. The contract seals internal `SegmentHandle`
//! coordinates into opaque, session-local, generation-bound tokens — raw
//! `{chapter,page,idx}` never crosses the LLM tool boundary — and serializes
//! bounded results: at most 5 hits, ordinal rank only (no scores), word-capped
//! snippets, and a hard payload byte cap. Sentinel wrapping of book-derived
//! strings happens at the voice boundary through the existing shared
//! machinery; this layer produces the bounded payload it wraps.

use std::collections::HashMap;

use rand::RngCore;
use serde::Serialize;

use super::provider::{
    BookFingerprint, Provenance, RetrievalError, RetrievalProvider, SearchOptions, SegmentHandle,
};

/// Owner-tunable payload ceiling (charter §4.4.6): top-k 5 at the 15-word
/// snippet default must serialize to at most 900 bytes on the payload fixture.
pub const MAX_PAYLOAD_BYTES: usize = 900;

/// Default `open_passage` parent-window word budget (charter §3.3: normally
/// 1024-1500 tokens; words are the conservative proxy here).
pub const DEFAULT_WINDOW_WORDS: usize = 1200;

/// Session-local opaque handle table. Tokens are 128-bit CSPRNG hex values
/// bound to the minting fingerprint; they expire with the table (session end)
/// and are refused across generations by construction.
pub struct HandleTable {
    fingerprint: BookFingerprint,
    entries: HashMap<String, (SegmentHandle, Provenance)>,
    mint_order: std::collections::VecDeque<String>,
}

/// Live-handle cap per session table: oldest mints expire first so a long,
/// search-heavy session stays bounded (§4.3 expiry-no-later-than-session-end).
pub const MAX_LIVE_HANDLES: usize = 256;

impl HandleTable {
    pub fn new(fingerprint: BookFingerprint) -> Self {
        Self {
            fingerprint,
            entries: HashMap::new(),
            mint_order: std::collections::VecDeque::new(),
        }
    }

    fn mint(&mut self, handle: SegmentHandle, provenance: Provenance) -> String {
        while self.mint_order.len() >= MAX_LIVE_HANDLES {
            if let Some(old) = self.mint_order.pop_front() {
                self.entries.remove(&old);
            }
        }
        let mut bytes = [0u8; 16];
        rand::rng().fill_bytes(&mut bytes);
        let token = hex::encode(bytes);
        self.entries.insert(token.clone(), (handle, provenance));
        self.mint_order.push_back(token.clone());
        token
    }

    /// Resolve a live token minted by this table for this generation.
    pub fn resolve(&self, token: &str) -> Option<(SegmentHandle, Provenance)> {
        self.entries.get(token).copied()
    }

    pub fn fingerprint(&self) -> &BookFingerprint {
        &self.fingerprint
    }
}

/// One LLM-facing hit: opaque handle, location labels, ordinal rank, snippet.
/// No score, no raw coordinates beyond the visible chapter/page labels.
#[derive(Debug, Serialize)]
pub struct ToolHit {
    pub handle: String,
    pub chapter: u32,
    pub page: u32,
    pub rank: usize,
    pub snippet: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SearchBookResult {
    Ok {
        hits: Vec<ToolHit>,
    },
    /// Typed empty for missing/stale/corrupt index state: zero hits, a safe
    /// state code, returned immediately (the app owns rebuild/retry).
    RetrievalUnavailable {
        code: &'static str,
    },
    /// Bounded refusal for contract violations (bad variants, unknown chapter).
    Invalid {
        code: String,
    },
}

/// `search_book` contract: validate, search, seal handles, serialize bounded.
/// An empty index or a question the book does not cover yields `Ok { hits: [] }`
/// — the honest-absence signal the persona law builds refusals from.
pub fn search_book(
    provider: &dyn RetrievalProvider,
    handles: &mut HandleTable,
    queries: &[String],
    opts: &SearchOptions,
) -> SearchBookResult {
    let fingerprint = handles.fingerprint().clone();
    match provider.search(&fingerprint, queries, opts) {
        Ok(hits) => {
            let hits = hits
                .into_iter()
                .take(5)
                .enumerate()
                .map(|(i, h)| ToolHit {
                    handle: handles.mint(h.handle, h.provenance),
                    chapter: h.handle.chapter,
                    page: h.handle.page,
                    rank: i + 1,
                    snippet: h.snippet,
                })
                .collect();
            SearchBookResult::Ok { hits }
        }
        Err(RetrievalError::IndexMissing) => SearchBookResult::RetrievalUnavailable {
            code: "index_missing",
        },
        Err(RetrievalError::IndexStale) => SearchBookResult::RetrievalUnavailable {
            code: "index_stale",
        },
        Err(RetrievalError::Corrupt) => SearchBookResult::RetrievalUnavailable {
            code: "index_corrupt",
        },
        Err(other) => SearchBookResult::Invalid {
            code: other.to_string(),
        },
    }
}

/// Serialize a search result, enforcing the payload ceiling. Reduction order:
/// drop whole trailing words from the longest snippet; halve a wordless
/// snippet at a char boundary; finally drop trailing hits. The ceiling is a
/// law, never silently exceeded — an over-budget result is reduced, not widened.
pub fn serialize_bounded(result: &SearchBookResult) -> String {
    let payload =
        serde_json::to_string(result).unwrap_or_else(|_| "{\"status\":\"invalid\"}".into());
    if payload.len() <= MAX_PAYLOAD_BYTES {
        return payload;
    }
    let SearchBookResult::Ok { hits } = result else {
        return payload;
    };
    let mut trimmed: Vec<ToolHit> = hits
        .iter()
        .map(|h| ToolHit {
            handle: h.handle.clone(),
            chapter: h.chapter,
            page: h.page,
            rank: h.rank,
            snippet: h.snippet.clone(),
        })
        .collect();
    let render = |hits: &[ToolHit]| {
        serde_json::to_string(&SearchBookResult::Ok {
            hits: hits
                .iter()
                .map(|h| ToolHit {
                    handle: h.handle.clone(),
                    chapter: h.chapter,
                    page: h.page,
                    rank: h.rank,
                    snippet: h.snippet.clone(),
                })
                .collect(),
        })
        .unwrap_or_default()
    };
    let mut payload = render(&trimmed);
    while payload.len() > MAX_PAYLOAD_BYTES {
        if let Some(longest) = trimmed
            .iter_mut()
            .filter(|h| !h.snippet.is_empty())
            .max_by_key(|h| h.snippet.len())
        {
            if let Some(cut) = longest.snippet.rfind(' ') {
                longest.snippet.truncate(cut);
            } else {
                // Wordless adversarial snippet: halve at a char boundary.
                let target = longest.snippet.len() / 2;
                let cut = (0..=target)
                    .rev()
                    .find(|&i| longest.snippet.is_char_boundary(i))
                    .unwrap_or(0);
                longest.snippet.truncate(cut);
            }
        } else if trimmed.pop().is_none() {
            break;
        }
        payload = render(&trimmed);
    }
    payload
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OpenPassageResult {
    Ok {
        chapter: u32,
        page: u32,
        text: String,
    },
    Invalid {
        code: &'static str,
    },
}

/// `open_passage` contract: resolve a live opaque handle to one bounded
/// source-faithful parent window. Unknown/stale tokens and generated-prose
/// anchors are refused, never best-effort remapped (T10, T9).
pub fn open_passage(
    provider: &dyn RetrievalProvider,
    handles: &HandleTable,
    token: &str,
) -> OpenPassageResult {
    let Some((handle, provenance)) = handles.resolve(token) else {
        return OpenPassageResult::Invalid {
            code: "unknown_handle",
        };
    };
    if provenance == Provenance::Generated {
        return OpenPassageResult::Invalid {
            code: "generated_not_evidence",
        };
    }
    match provider.parent_window(handles.fingerprint(), &handle, DEFAULT_WINDOW_WORDS) {
        Ok(text) => OpenPassageResult::Ok {
            chapter: handle.chapter,
            page: handle.page,
            text,
        },
        Err(
            RetrievalError::IndexMissing | RetrievalError::IndexStale | RetrievalError::Corrupt,
        ) => OpenPassageResult::Invalid {
            code: "retrieval_unavailable",
        },
        Err(RetrievalError::UnknownHandle) => OpenPassageResult::Invalid {
            code: "unknown_handle",
        },
        Err(RetrievalError::GeneratedNotEvidence) => OpenPassageResult::Invalid {
            code: "generated_not_evidence",
        },
        Err(_) => OpenPassageResult::Invalid {
            code: "unresolvable",
        },
    }
}
